use crate::{
    runtime::{class::Class, structure::Structure},
    vm::VirtualMachine,
};

use super::{addr::Address, block::Block, precise_allocation::PreciseAllocation};
use core::{mem::size_of, mem::transmute};
#[cfg(feature = "debug-snapshots")]
use erased_serde::serialize_trait_object;
use minivec::MiniVec;
use mopa::{mopafy, Any};
use std::collections::HashMap;
use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};
pub const GC_DEAD: u8 = 0x4;
pub const GC_WHITE: u8 = 0x0;
pub const GC_BLACK: u8 = 0x1;
pub const GC_GRAY: u8 = 0x2;
pub const GC_MARKED: u8 = GC_BLACK;
pub const GC_UNMARKED: u8 = GC_WHITE;

pub trait Tracer {
    fn trace(&mut self, header: *mut Header);
}
/// Indicates that a type can be traced by a garbage collector.
///
/// This doesn't necessarily mean that the type is safe to allocate in a garbage collector ([Cell]).
///
/// ## Safety
/// See the documentation of the `trace` method for more info.
/// Essentially, this object must faithfully trace anything that
/// could contain garbage collected pointers or other `Trace` items.
pub unsafe trait Trace: Any {
    /// Visit each field in this type
    ///
    ///
    /// Structures should trace each of their fields,
    /// and collections should trace each of their elements.
    ///
    /// ### Safety
    /// Some types (like `Gc`) need special actions taken when they're traced,
    /// but those are somewhat rare and are usually already provided by the garbage collector.
    ///
    /// ## Always Permitted
    /// - Reading your own memory (includes iteration)
    ///   - Interior mutation is undefined behavior, even if you use `RefCell`
    /// - Calling `Tracer::trace` with the specified collector
    ///   - `Tracer::trace` already verifies that it owns the data, so you don't need to do that
    /// - Panicking
    ///   - This should be reserved for cases where you are seriously screwed up,
    ///       and can't fulfill your contract to trace your interior properly.
    ///   - This rule may change in future versions, depending on how we deal with multi-threading.
    /// ## Never Permitted Behavior
    /// - Forgetting a element of a collection, or field of a structure
    ///   - If you forget an element undefined behavior will result
    ///   - This is why we always prefer automatically derived implementations where possible.
    ///     - You will never trigger undefined behavior with an automatic implementation,
    ///       and it'll always be completely sufficient for safe code (aside from destructors).
    ///     - With an automatically derived implementation you will never miss a field
    /// - Invoking this function directly, without delegating to `Tracer`
    #[allow(unused_variables)]
    fn trace(&self, tracer: &mut dyn Tracer) {
        /* no-op */
    }
}
mopafy!(Trace);

#[cfg(not(feature = "debug-snapshots"))]
pub trait __CellBase {}
#[cfg(not(feature = "debug-snapshots"))]
impl<T> __CellBase for T {}

#[cfg(feature = "debug-snapshots")]
pub trait __CellBase: erased_serde::Serialize {}
#[cfg(feature = "debug-snapshots")]
impl<T: erased_serde::Serialize> __CellBase for T {}

/// `Cell` is a type that can be allocated in GC heap and passed to JavaScript environment.
///
///
/// All cells that is not part of `src/runtime` treatened as dummy objects and property accesses
/// is no-op on them.
///
pub trait Cell: Any + Trace + __CellBase {
    /// Compute size of `Cell` for allocation.
    ///
    /// This function allows us to have some kind of unsized values on the GC heap.
    ///
    fn compute_size(&self) -> usize {
        std::mem::size_of_val(self)
    }
    /// Return JS class of this cell.
    fn get_class_value(&self) -> Option<&'static Class> {
        None
    }
    /// Get name of this cell.
    fn get_typename(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
    /// Return JS structure of this cell.
    fn get_structure(&self) -> Option<Gc<Structure>> {
        None
    }
    fn set_class_value(&mut self, _class: &'static Class) {}
    fn set_structure(&mut self, _vm: &mut VirtualMachine, _structure: Gc<Structure>) {}
}
#[cfg(feature = "debug-snapshots")]
serialize_trait_object!(Cell);
mopafy!(Cell, core = core);

#[derive(Copy, Clone, Default)]
#[repr(C)]
pub struct Header {
    /// pointer to type vtable
    ty: usize,
    #[cfg(any(feature = "tag-field", target_pointer_width = "32"))]
    tag: u8,
    //zap: bool,
    data: [u8; 0],
}

impl Header {
    pub fn new(vtable: usize) -> Self {
        let mut this = Self {
            ty: 0,
            #[cfg(any(feature = "tag-field", target_pointer_width = "32"))]
            tag: 0,
            // zap: true,
            data: [],
        };
        unsafe { this.set_vtable(vtable) };
        this
    }
    /// Return true if this object is precie allocation
    pub fn is_precise_allocation(&self) -> bool {
        PreciseAllocation::is_precise(self as *const _ as *mut _)
    }
    /// Return precise allocation from this object
    pub fn precise_allocation(&self) -> *mut PreciseAllocation {
        PreciseAllocation::from_cell(self as *const _ as *mut _)
    }
    /// Return block where this cell was allocated
    pub fn block(&self) -> *mut Block {
        Block::from_cell(Address::from_ptr(self))
    }
    pub fn data_start(&self) -> Address {
        Address::from_ptr(self.data.as_ptr())
    }
    #[allow(clippy::mut_from_ref)]
    pub fn get_dyn(&mut self) -> &mut dyn Cell {
        unsafe {
            transmute(TraitObject {
                data: self.data_start().to_mut_ptr(),
                vtable: self.vtable() as *mut (),
            })
        }
    }

    /// Zap object
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn zap(&mut self, _reason: u32) {
        //assert!(self.vtable() != 0x10);
        self.set_vtable(0);
        // self.zap = true;
        //self.set_vtable(0);
    }
    /// Check if object is zapped
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn is_zapped(&self) -> bool {
        self.vtable() == 0
        //self.vtable() == 0
    }
}

#[cfg(all(not(feature = "tag-field"), target_pointer_width = "64"))]
impl Header {
    pub fn vtable(&self) -> usize {
        self.ty & (!0x03)
    }

    pub fn tag(&self) -> u8 {
        (self.ty & 0x03) as _
    }
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn set_vtable(&mut self, vtable: usize) {
        self.ty = vtable | self.tag() as usize;
    }
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn set_tag(&mut self, tag: u8) {
        self.ty = self.vtable() | tag as usize;
    }
}

#[cfg(any(feature = "tag-field", target_pointer_width = "32"))]
impl Header {
    pub fn vtable(&self) -> usize {
        self.ty
    }

    pub fn tag(&self) -> u8 {
        self.tag
    }

    pub unsafe fn set_vtable(&mut self, vtable: usize) {
        self.ty = vtable;
    }

    pub unsafe fn set_tag(&mut self, tag: u8) {
        self.tag = tag;
    }
}
#[repr(C)]
pub struct TraitObject {
    pub data: *mut (),
    pub vtable: *mut (),
}

pub fn object_ty_of<T: Cell>(x: *const T) -> usize {
    unsafe { core::mem::transmute::<_, TraitObject>(x as *const dyn Cell).vtable as _ }
}

pub fn object_ty_of_type<T: Cell + Sized>() -> usize {
    object_ty_of(core::ptr::null::<T>())
}

/// A garbage collected pointer to a value.
///
/// This is the equivalent of a garbage collected smart-pointer.
///
///
/// The smart pointer is simply a guarantee to the garbage collector
/// that this points to a garbage collected object with the correct header,
/// and not some arbitrary bits that you've decided to heap allocate.
pub struct Gc<T: Cell + ?Sized> {
    pub cell: NonNull<Header>,
    pub marker: PhantomData<T>,
}

macro_rules! impl_prim {
    ($($t:ty)*) => {$(
        unsafe impl Trace for $t {}
        impl Cell for $t {}
    )*
    };
}

impl_prim!(() bool f32 f64 u8 u16 u32 u64 u128 i8 i16 i32 i64 i128);
impl<T: Cell + ?Sized> Gc<T> {
    pub fn ptr_eq<U: Cell + ?Sized>(this: Gc<T>, other: Gc<U>) -> bool {
        this.cell == other.cell
    }
    /// Create `Heap<T>` from raw pointer.
    ///
    /// # Safety
    ///
    /// If `ptr` is not pointing to GC heap invking this method might lead to UB or segfaults.
    pub unsafe fn from_raw(ptr: *const T) -> Self {
        Self {
            cell: NonNull::new(ptr.cast::<u8>().sub(size_of::<Header>()) as *mut Header)
                .expect("expected non-null pointer"),
            marker: Default::default(),
        }
    }

    pub fn get_dyn(&self) -> &dyn Cell {
        unsafe { (*self.cell.as_ptr()).get_dyn() }
    }

    pub fn get_dyn_mut(&mut self) -> &mut dyn Cell {
        unsafe { (*self.cell.as_ptr()).get_dyn() }
    }

    pub fn as_dyn(&self) -> Gc<dyn Cell> {
        Gc {
            cell: self.cell,
            marker: Default::default(),
        }
    }
    pub fn is<U: Cell>(self) -> bool {
        unsafe {
            let this_vtbl = (*self.cell.as_ptr()).vtable();
            let u_vtbl = object_ty_of_type::<U>();
            this_vtbl == u_vtbl
        }
    }
}

impl Gc<dyn Cell> {
    /// Casts this heap cell to `T` without making any checks.
    ///
    ///
    /// # Safety
    /// This function is unsafe because it does not do any checks to see if this heap cell is `T` or no.
    pub unsafe fn downcast_unchecked<T: Cell>(self) -> Gc<T> {
        {
            Gc {
                cell: self.cell,
                marker: Default::default(),
            }
        }
    }

    pub fn downcast<T: Cell>(self) -> Option<Gc<T>> {
        if self.is::<T>() {
            unsafe { Some(self.downcast_unchecked()) }
        } else {
            None
        }
    }
}
impl<T: Cell> Deref for Gc<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe {
            let cell = &mut *self.cell.as_ptr();
            &*cell.data_start().to_mut_ptr::<T>()
        }
    }
}
impl<T: Cell> DerefMut for Gc<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            let cell = &mut *self.cell.as_ptr();
            &mut *cell.data_start().to_mut_ptr::<T>()
        }
    }
}

unsafe impl<T: Cell + ?Sized> Trace for Gc<T> {
    fn trace(&self, tracer: &mut dyn Tracer) {
        tracer.trace(self.cell.as_ptr());
    }
}

impl<T: Cell + ?Sized> Copy for Gc<T> {}

impl<T: Cell + ?Sized> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: Cell, V: Cell> Cell for HashMap<K, V> {}
unsafe impl<K: Trace, V: Trace> Trace for HashMap<K, V> {
    fn trace(&self, tracer: &mut dyn Tracer) {
        for (k, v) in self.iter() {
            k.trace(tracer);
            v.trace(tracer);
        }
    }
}

impl<T: Cell> Cell for Option<T> {}
unsafe impl<T: Trace> Trace for Option<T> {
    fn trace(&self, tracer: &mut dyn Tracer) {
        match self {
            Some(elem) => elem.trace(tracer),
            _ => (),
        }
    }
}

impl<T: Cell> Cell for Gc<T> {
    fn get_class_value(&self) -> Option<&'static Class> {
        (**self).get_class_value()
    }

    fn get_structure(&self) -> Option<Gc<Structure>> {
        (**self).get_structure()
    }

    fn set_class_value(&mut self, _class: &'static Class) {
        (**self).set_class_value(_class)
    }

    fn set_structure(&mut self, _vm: &mut VirtualMachine, _structure: Gc<Structure>) {
        (**self).set_structure(_vm, _structure)
    }
}

#[cfg(feature = "debug-snapshots")]
impl<T: Cell> serde::Serialize for Heap<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        format!("Heap(at {:x})", self.cell).serialize(serializer)
    }
}

impl<T: Cell> Cell for Vec<T> {}
unsafe impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, tracer: &mut dyn Tracer) {
        for elem in self.iter() {
            elem.trace(tracer);
        }
    }
}
unsafe impl<T: Trace> Trace for MiniVec<T> {
    fn trace(&self, tracer: &mut dyn Tracer) {
        for elem in self.iter() {
            elem.trace(tracer);
        }
    }
}

impl<T: Cell> Cell for MiniVec<T> {}
