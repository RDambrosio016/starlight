use std::{
    mem::{size_of, transmute},
    ops::{Deref, DerefMut},
};

use crate::{
    gc::{handle::Handle, heap_cell::HeapObject},
    heap::trace::Tracer,
};

use super::{
    attributes::AttrSafe, js_cell::JsCell, js_value::JsValue, property_descriptor::StoredSlot,
};

pub struct Slot {
    pub parent: StoredSlot,
    base: Option<Handle<dyn HeapObject>>,
    offset: u32,
    flags: u32,
}

impl Slot {
    pub const FLAG_USED: u32 = 1;
    pub const FLAG_CACHEABLE: u32 = 2;
    pub const FLAG_PUT_CACHEABLE: u32 = 4;
    pub const FLAG_FORCE_PUT_UNCACHEABLE: u32 = 8;
    pub const FLAG_INIT: u32 = Slot::FLAG_CACHEABLE;
    pub const PUT_SHIFT: u32 = 4;
    pub const PUT_MASK: u32 = 3;
    fn is_cacheable(&self) -> bool {
        (self.flags & Self::FLAG_CACHEABLE) != 0
    }
    fn is_put_force_unchacheable(&self) -> bool {
        (self.flags & Self::FLAG_FORCE_PUT_UNCACHEABLE) != 0
    }

    fn set_put_result_type(&mut self, ty: PutResultType) {
        self.flags &= !(Self::PUT_MASK << Self::PUT_SHIFT);
        self.flags |= (ty as u32) << Self::PUT_SHIFT
    }

    pub fn put_result_type(&self) -> PutResultType {
        unsafe { transmute((self.flags >> Self::PUT_SHIFT) & Self::PUT_MASK) }
    }

    pub fn mark_put_result(&mut self, ty: PutResultType, offset: u32) {
        self.set_put_result_type(ty);
        self.offset = offset;
        self.flags |= Self::FLAG_PUT_CACHEABLE;
    }

    pub fn is_used(&self) -> bool {
        (self.flags & Self::FLAG_USED) != 0
    }

    pub fn clear(&mut self) {
        self.set(JsValue::undefined(), AttrSafe::not_found());
        self.flags = Self::FLAG_INIT;
        self.base = None;
        self.offset = u32::MAX;
    }

    pub fn make_used(&mut self) {
        self.flags &= !Self::FLAG_USED;
    }

    pub fn is_put_cacheable(&self) -> bool {
        (self.flags & Self::FLAG_PUT_CACHEABLE) != 0 && !self.is_put_force_unchacheable()
    }

    pub fn make_put_uncacheable(&mut self) {
        self.flags |= Self::FLAG_FORCE_PUT_UNCACHEABLE;
    }

    pub fn make_uncacheable(&mut self) {
        self.flags &= !Self::FLAG_CACHEABLE;
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn base(&self) -> &Option<Handle<dyn HeapObject>> {
        &self.base
    }

    pub fn set_1(
        &mut self,
        value: JsValue,
        attributes: AttrSafe,
        obj: Option<Handle<dyn HeapObject>>,
    ) {
        self.set(value, attributes);
        self.make_used();
        self.make_uncacheable();
        self.base = obj;
        self.offset = u32::MAX;
    }

    pub fn set_woffset(
        &mut self,
        value: JsValue,
        attributes: AttrSafe,
        obj: Option<Handle<dyn HeapObject>>,
        offset: u32,
    ) {
        self.set(value, attributes);
        self.make_used();
        self.base = obj;
        self.offset = offset;
    }

    pub fn set_from_slot(&mut self, slot: &StoredSlot, obj: Option<Handle<dyn HeapObject>>) {
        self.set_1(slot.value(), slot.attributes(), obj);
    }

    pub fn is_load_cacheable(&self) -> bool {
        self.is_cacheable() && self.attributes().is_data()
    }

    pub fn is_store_cacheable(&self) -> bool {
        self.is_cacheable() && self.attributes().is_simple_data()
    }

    pub fn has_offset(&self) -> bool {
        self.offset != u32::MAX
    }

    pub fn is_not_found(&self) -> bool {
        self.attributes().is_not_found()
    }

    pub fn new() -> Self {
        Self {
            parent: StoredSlot::new_raw(JsValue::undefined(), AttrSafe::not_found()),
            offset: u32::MAX,
            flags: Self::FLAG_INIT,
            base: None,
        }
    }
}

impl HeapObject for Slot {
    fn visit_children(&mut self, tracer: &mut dyn Tracer) {
        if let Some(ref mut obj) = self.base {
            obj.visit_children(tracer);
        }
        if self.value().is_cell() && !self.value().is_empty() {
            self.value.as_cell_ref_mut().visit_children(tracer);
        }
    }

    fn compute_size(&self) -> usize {
        size_of::<Self>()
    }

    fn needs_destruction(&self) -> bool {
        false
    }
}
impl JsCell for Slot {}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum PutResultType {
    None = 0,
    Replace,
    New,
    IndexedOptimized,
}

impl Deref for Slot {
    type Target = StoredSlot;
    fn deref(&self) -> &Self::Target {
        &self.parent
    }
}

impl DerefMut for Slot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.parent
    }
}