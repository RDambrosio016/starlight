use super::{
    attributes::*,
    object::JsObject,
    symbol::{Symbol, DUMMY_SYMBOL},
};
use crate::{
    heap::cell::{Cell, Gc, Trace, Tracer},
    vm::VirtualMachine,
};
use std::collections::HashMap;
use wtf_rs::unwrap_unchecked;

/// In JavaScript programs, it's common to have multiple objects with the same property keys. Such objects
/// have the same *shape*.
/// ```js
/// let obj1 = {x: 1,y: 2}
/// let obj2 = {x: 3,y: 4}
/// ```
///
/// It's also common to access property on objects with the same shape:
///
/// ```js
/// function f(obj) {
///     return obj.x;
/// }
///
/// f(obj1);
/// f(obj2);
/// ```
///
/// With that in mind, Starlight can optimize object property accesses based on the object's shape or `Structure` how
/// call it.
///
///
/// `Structure` stores property keys, offsets within JSObject and property attributes, structures might be shared between
/// multiple objects. When property is added new structure is created (if does not exist) and transition happens to the
/// new structure. This way we can optimize field load into single `object.slots + field_offset` load.
///
/// More info here: [JavaScript engine fundamentals: Shapes and Inline Caches](https://mathiasbynens.be/notes/shapes-ics)
pub struct Structure {
    id: StructureID,
    transitions: TransitionsTable,
    table: Option<Gc<TargetTable>>,
    deleted: DeletedEntryHolder,
    added: (Symbol, MapEntry),
    previous: Option<Gc<Structure>>,
    prototype: Option<Gc<JsObject>>,
    calculated_size: u32,
    transit_count: u32,
}

pub type StructureID = u32;

#[derive(Copy, Clone)]
pub struct MapEntry {
    pub offset: u32,
    pub attrs: AttrSafe,
}

impl MapEntry {
    pub fn not_found() -> Self {
        Self {
            offset: u32::MAX,
            attrs: AttrSafe::not_found(),
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.attrs.is_not_found()
    }
}

#[cfg(feature = "debug-snapshots")]
impl serde::Serialize for MapEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut x = serializer.serialize_struct("MapEntry", 1)?;
        x.serialize_field("offset", &self.offset)?;
        x.end()
    }
}
impl Cell for MapEntry {}
unsafe impl Trace for MapEntry {}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransitionKey {
    name: Symbol,
    attrs: u32,
}

impl Cell for TransitionKey {}
unsafe impl Trace for TransitionKey {}

#[cfg(feature = "debug-snapshots")]
impl serde::Serialize for TransitionKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut x = serializer.serialize_struct("TransitionKey", 2)?;
        x.serialize_field("name", &self.name.as_string())?;
        x.serialize_field("attrs", &format!("{:x}", self.attrs))?;
        x.end()
    }
}
union U {
    table: Option<Gc<Table>>,
    pair: (TransitionKey, Option<Gc<Structure>>),
}
pub struct Transitions {
    u: U,
    flags: u8,
}
#[derive(Clone, Copy)]
pub enum Transition {
    None,
    Table(Option<Gc<Table>>),
    Pair(TransitionKey, Option<Gc<Structure>>),
}

pub struct TransitionsTable {
    var: Transition,
    enabled: bool,
    unique: bool,
    indexed: bool,
}

impl TransitionsTable {
    pub fn new(enabled: bool, indexed: bool) -> Self {
        Self {
            var: Transition::None,
            unique: false,
            indexed,
            enabled,
        }
    }
    pub fn set_indexed(&mut self, indexed: bool) {
        self.indexed = indexed;
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn is_enabled_unique_transition(&self) -> bool {
        self.unique
    }

    pub fn enable_unique_transition(&mut self) {
        self.unique = true;
    }

    pub fn insert(
        &mut self,
        vm: &mut VirtualMachine,
        name: Symbol,
        attrs: AttrSafe,
        map: Gc<Structure>,
    ) {
        let key = TransitionKey {
            name,
            attrs: attrs.raw(),
        };
        if let Transition::Pair(x, y) = self.var {
            let mut table = vm.space().alloc(HashMap::new());
            table.insert(x, y);
            self.var = Transition::Table(Some(table));
        }
        if let Transition::Table(Some(mut table)) = self.var {
            table.insert(key, Some(map));
        } else {
            self.var = Transition::Pair(key, Some(map));
        }
    }

    pub fn find(&self, name: Symbol, attrs: AttrSafe) -> Option<Gc<Structure>> {
        let key = TransitionKey {
            name,
            attrs: attrs.raw(),
        };
        if let Transition::Table(ref table) = self.var {
            return table.unwrap().get(&key).copied().flatten();
        } else if let Transition::Pair(key_, map) = self.var {
            if key == key_ {
                return map;
            }
        }
        None
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    pub fn is_indexed(&self) -> bool {
        self.indexed
    }
}

const MASK_ENABLED: u8 = 1;
const MASK_UNIQUE_TRANSITION: u8 = 2;
const MASK_HOLD_SINGLE: u8 = 4;
const MASK_HOLD_TABLE: u8 = 8;
const MASK_INDEXED: u8 = 16;

type Table = HashMap<TransitionKey, Option<Gc<Structure>>>;

impl Transitions {
    pub fn new(enabled: bool, indexed: bool) -> Self {
        let mut this = Self {
            u: U { table: None },
            flags: 0,
        };
        this.set_enabled(enabled);
        this.set_indexed(indexed);
        this
    }
    pub fn set_indexed(&mut self, indexed: bool) {
        if indexed {
            self.flags |= MASK_INDEXED;
        } else {
            self.flags &= !MASK_INDEXED;
        }
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            self.flags |= MASK_ENABLED;
        } else {
            self.flags &= !MASK_ENABLED;
        }
    }

    pub fn is_enabled_unique_transition(&self) -> bool {
        (self.flags & MASK_UNIQUE_TRANSITION) != 0
    }

    pub fn enable_unique_transition(&mut self) {
        self.flags |= MASK_UNIQUE_TRANSITION;
    }

    pub fn insert(
        &mut self,
        vm: &mut VirtualMachine,
        name: Symbol,
        attrs: AttrSafe,
        map: Gc<Structure>,
    ) {
        let key = TransitionKey {
            name,
            attrs: attrs.raw(),
        };
        unsafe {
            if (self.flags & MASK_HOLD_SINGLE) != 0 {
                let mut table: Gc<Table> = vm.space().alloc(Default::default());
                table.insert(self.u.pair.0, self.u.pair.1);
                self.u.table = Some(table);
                self.flags &= !MASK_HOLD_SINGLE;
                self.flags &= MASK_HOLD_TABLE;
            }
            if (self.flags & MASK_HOLD_TABLE) != 0 {
                self.u.table.unwrap().insert(key, Some(map));
            } else {
                self.u.pair.0 = key;
                self.u.pair.1 = Some(map);
                self.flags |= MASK_HOLD_SINGLE;
            }
        }
    }

    pub fn find(&self, name: Symbol, attrs: AttrSafe) -> Option<Gc<Structure>> {
        let key = TransitionKey {
            name,
            attrs: attrs.raw(),
        };
        unsafe {
            if (self.flags & MASK_HOLD_TABLE) != 0 {
                return self.u.table.unwrap().get(&key).copied().flatten();
            } else if (self.flags & MASK_HOLD_SINGLE) != 0 {
                if self.u.pair.0 == key {
                    return self.u.pair.1;
                }
            }
        }
        None
    }

    pub fn is_enabled(&self) -> bool {
        (self.flags & MASK_ENABLED) != 0
    }

    pub fn is_indexed(&self) -> bool {
        (self.flags & MASK_INDEXED) != 0
    }
}

#[cfg(feature = "debug-snapshots")]
impl serde::Serialize for Structure {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut x = serializer.serialize_struct("Structure", 0);
        x.end()
    }
}
unsafe impl Trace for TransitionsTable {
    fn trace(&self, tracer: &mut dyn Tracer) {
        match self.var {
            Transition::Pair(_, x) => x.trace(tracer),
            Transition::Table(table) => {
                table.trace(tracer);
            }
            _ => (),
        }
    }
}
impl Cell for Structure {}
unsafe impl Trace for Structure {
    fn trace(&self, tracer: &mut dyn Tracer) {
        self.transitions.trace(tracer);
        self.table.trace(tracer);
        self.prototype.trace(tracer);
        self.deleted.entry.trace(tracer);
        match self.previous.as_ref() {
            Some(x) => {
                x.trace(tracer);
            }
            _ => (),
        }
    }
}

impl Structure {
    pub fn id(&self) -> StructureID {
        self.id
    }
    /// Set structure ID.
    ///
    /// # Safety
    ///
    /// It is unsafe to change structure id since it may change program behaviour.
    pub unsafe fn set_id(&mut self, id: StructureID) {
        self.id = id;
    }
}
#[derive(Clone, Copy)]
pub struct DeletedEntryHolder {
    entry: Option<Gc<DeletedEntry>>,
    size: u32,
}

impl DeletedEntryHolder {
    pub fn push(&mut self, vm: &mut VirtualMachine, offset: u32) {
        let entry = vm.space().alloc(DeletedEntry {
            prev: self.entry,
            offset,
        });
        self.entry = Some(entry);
    }
    pub fn pop(&mut self) -> u32 {
        let res = unwrap_unchecked(self.entry).offset;
        self.entry = unwrap_unchecked(self.entry).prev;
        self.size -= 1;
        res
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn empty(&self) -> bool {
        self.size == 0
    }
}

pub type TargetTable = HashMap<Symbol, MapEntry>;

pub struct DeletedEntry {
    prev: Option<Gc<DeletedEntry>>,
    offset: u32,
}

unsafe impl Trace for DeletedEntry {
    fn trace(&self, tracer: &mut dyn Tracer) {
        self.prev.trace(tracer)
    }
}

impl Cell for DeletedEntry {}

#[cfg(feature = "debug-snapshots")]
impl serde::Serialize for DeletedEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut x = serializer.serialize_struct("DeletedEntry", 2)?;
        x.serialize_field("offset", &self.offset)?;
        x.serialize_field("prev", &self.prev)?;
        x.end()
    }
}

impl Structure {
    pub fn delete(&mut self, vm: &mut VirtualMachine, name: Symbol) {
        let it = unwrap_unchecked(self.table.as_mut()).remove(&name).unwrap();
        self.deleted.push(vm, it.offset);
    }

    pub fn change_attributes(&mut self, name: Symbol, attributes: AttrSafe) {
        let it = unwrap_unchecked(self.table.as_mut())
            .get_mut(&name)
            .unwrap();
        it.attrs = attributes;
    }

    pub fn table(&self) -> Option<Gc<TargetTable>> {
        self.table
    }
    pub fn is_adding_map(&self) -> bool {
        self.added.0 != DUMMY_SYMBOL
    }

    pub fn has_table(&self) -> bool {
        self.table.is_some()
    }
    pub fn allocate_table(&mut self, vm: &mut VirtualMachine) {
        let mut stack = Vec::with_capacity(8);

        if self.is_adding_map() {
            stack.push(self as *mut Self);
        }

        let mut current = self.previous;
        loop {
            match current {
                Some(cur) => {
                    if cur.has_table() {
                        self.table =
                            Some(vm.space().alloc((**cur.table.as_ref().unwrap()).clone()));
                        break;
                    } else {
                        if cur.is_adding_map() {
                            stack.push(&*cur as *const Self as *mut Self);
                        }
                    }
                    current = cur.previous;
                }
                None => {
                    self.table = Some(vm.space().alloc(HashMap::new()));
                    break;
                }
            }
        }
        assert!(self.table.is_some());
        let mut table = self.table.unwrap();
        unsafe {
            for it in stack {
                table.insert((*it).added.0, (*it).added.1);
            }
        }
        self.previous = None;
    }

    pub fn allocate_table_if_needed(&mut self, vm: &mut VirtualMachine) -> bool {
        if !self.has_table() {
            if self.previous.is_none() {
                return false;
            }
            self.allocate_table(vm);
        }
        true
    }

    pub fn is_indexed(&self) -> bool {
        self.transitions.is_indexed()
    }

    pub fn is_unique(&self) -> bool {
        !self.transitions.is_enabled()
    }

    pub fn is_shaped(&self) -> bool {
        // we can use this map id as shape or not
        !self.is_unique() || self.transitions.is_enabled()
    }

    pub fn prototype(&self) -> Option<Gc<JsObject>> {
        self.prototype
    }

    pub fn flatten(&mut self) {
        if self.is_unique() {
            self.transitions.enable_unique_transition();
        }
    }

    pub fn get_slots_size(&self) -> usize {
        if let Some(table) = self.table {
            table.len() + self.deleted.size as usize
        } else {
            self.calculated_size as _
        }
    }
    fn ctor(vm: &mut VirtualMachine, previous: Gc<Self>, unique: bool) -> Gc<Self> {
        let mut this = vm.space().alloc(Self {
            prototype: previous.prototype,
            previous: Some(previous),
            table: if unique && previous.is_unique() {
                previous.table
            } else {
                None
            },
            transitions: TransitionsTable::new(!unique, previous.transitions.is_indexed()),
            deleted: previous.deleted,
            added: (
                DUMMY_SYMBOL,
                MapEntry {
                    offset: u32::MAX,
                    attrs: AttrSafe::not_found(),
                },
            ),
            id: 0,
            calculated_size: 0,
            transit_count: 0,
        });
        this.calculated_size = this.get_slots_size() as _;
        assert!(this.previous.is_some());
        this
    }

    fn ctor1(
        vm: &mut VirtualMachine,
        prototype: Option<Gc<JsObject>>,
        unique: bool,
        indexed: bool,
    ) -> Gc<Self> {
        vm.space().alloc(Self {
            prototype,
            previous: None,
            table: None,
            transitions: TransitionsTable::new(!unique, indexed),
            deleted: DeletedEntryHolder {
                entry: None,
                size: 0,
            },
            added: (
                DUMMY_SYMBOL,
                MapEntry {
                    offset: u32::MAX,
                    attrs: AttrSafe::not_found(),
                },
            ),
            id: 0,
            calculated_size: 0,
            transit_count: 0,
        })
    }
    #[allow(dead_code)]
    fn ctor2(
        vm: &mut VirtualMachine,
        table: Option<Gc<TargetTable>>,
        prototype: Option<Gc<JsObject>>,
        unique: bool,
        indexed: bool,
    ) -> Gc<Self> {
        let mut this = Self::ctor1(vm, prototype, unique, indexed);
        this.table = table;
        this.calculated_size = this.get_slots_size() as _;
        this
    }

    fn ctor3(vm: &mut VirtualMachine, it: &[(Symbol, MapEntry)]) -> Gc<Self> {
        let table = it.iter().copied().collect::<TargetTable>();
        let table = vm.space().alloc(table);
        let mut this = vm.space().alloc(Self {
            prototype: None,
            previous: None,
            table: Some(table),
            transitions: TransitionsTable::new(true, false),
            deleted: DeletedEntryHolder {
                entry: None,
                size: 0,
            },
            added: (
                DUMMY_SYMBOL,
                MapEntry {
                    offset: u32::MAX,
                    attrs: AttrSafe::not_found(),
                },
            ),
            id: 0,
            calculated_size: 0,
            transit_count: 0,
        });
        this.calculated_size = this.get_slots_size() as _;
        this
    }

    pub fn new(vm: &mut VirtualMachine, previous: Gc<Self>) -> Gc<Structure> {
        Self::ctor(vm, previous, false)
    }

    pub fn new_unique(vm: &mut VirtualMachine, previous: Gc<Self>) -> Gc<Structure> {
        Self::ctor(vm, previous, true)
    }
    pub fn new_unique_with_proto(
        vm: &mut VirtualMachine,
        proto: Option<Gc<JsObject>>,
        indexed: bool,
    ) -> Gc<Self> {
        Self::ctor2(vm, None, proto, true, indexed)
    }
    pub fn new_(vm: &mut VirtualMachine, it: &[(Symbol, MapEntry)]) -> Gc<Self> {
        Self::ctor3(vm, it)
    }
    pub fn new_from_table(
        vm: &mut VirtualMachine,
        table: Option<TargetTable>,
        prototype: Option<Gc<JsObject>>,
        unique: bool,
        indexed: bool,
    ) -> Gc<Structure> {
        let table = if let Some(table) = table {
            Some(vm.space().alloc(table))
        } else {
            None
        };

        Self::ctor2(vm, table, prototype, unique, indexed)
    }
    pub fn new_indexed(
        vm: &mut VirtualMachine,
        prototype: Option<Gc<JsObject>>,
        indexed: bool,
    ) -> Gc<Self> {
        Self::ctor1(vm, prototype, false, indexed)
    }
    pub fn new_unique_indexed(
        vm: &mut VirtualMachine,
        prototype: Option<Gc<JsObject>>,
        indexed: bool,
    ) -> Gc<Self> {
        Self::ctor1(vm, prototype, true, indexed)
    }

    pub fn new_from_point(vm: &mut VirtualMachine, map: Gc<Structure>) -> Gc<Self> {
        if map.is_unique() {
            return Self::new_unique(vm, map);
        }
        map
    }
}

impl Gc<Structure> {
    pub fn delete_property_transition(
        &mut self,
        vm: &mut VirtualMachine,
        name: Symbol,
    ) -> Gc<Structure> {
        let mut map = Structure::new_unique(
            vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
            *self,
        );
        if !map.has_table() {
            map.allocate_table(vm);
        }
        map.delete(vm, name);
        map
    }
    pub fn change_indexed_transition(&mut self, vm: &mut VirtualMachine) -> Gc<Structure> {
        if self.is_unique() {
            let mut map = if self.transitions.is_enabled_unique_transition() {
                Structure::new_unique(
                    vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                    *self,
                )
            } else {
                // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                *self
            };
            map.transitions.set_indexed(true);
            map
        } else {
            Structure::new_unique(
                vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                *self,
            )
            .change_indexed_transition(vm)
        }
    }

    pub fn change_prototype_transition(
        &mut self,
        vm: &mut VirtualMachine,
        prototype: Option<Gc<JsObject>>,
    ) -> Gc<Structure> {
        if self.is_unique() {
            let mut map = if self.transitions.is_enabled_unique_transition() {
                Structure::new_unique(
                    vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                    *self,
                )
            } else {
                // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                *self
            };
            map.prototype = prototype;
            map
        } else {
            let mut map = Structure::new_unique(
                vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                *self,
            );
            map.change_prototype_transition(vm, prototype)
        }
    }

    pub fn change_extensible_transition(&mut self, vm: &mut VirtualMachine) -> Gc<Structure> {
        Structure::new_unique(vm, *self)
    }
    pub fn change_attributes_transition(
        &mut self,
        vm: &mut VirtualMachine,
        name: Symbol,
        attributes: AttrSafe,
    ) -> Gc<Structure> {
        let mut map = Structure::new_unique(
            vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
            *self,
        );
        if !map.has_table() {
            map.allocate_table(vm);
        }
        map.change_attributes(name, attributes);
        map
    }

    pub fn get_own_property_names(
        &mut self,
        vm: &mut VirtualMachine,
        include: bool,
        mut collector: impl FnMut(Symbol, u32),
    ) {
        if self.allocate_table_if_needed(vm) {
            for entry in self.table.as_ref().unwrap().iter() {
                /*if entry.0.is_private() {
                    continue;
                }

                if entry.0.is_public() {
                    continue;
                }*/
                if include || entry.1.attrs.is_enumerable() {
                    collector(*entry.0, entry.1.offset);
                }
            }
        }
    }

    pub fn add_property_transition(
        &mut self,
        vm: &mut VirtualMachine,
        name: Symbol,
        attributes: AttrSafe,
        offset: &mut u32,
    ) -> Gc<Structure> {
        let mut entry = MapEntry {
            offset: 0,
            attrs: attributes,
        };

        if self.is_unique() {
            if !self.has_table() {
                self.allocate_table(vm);
            }

            let mut map = if self.transitions.is_enabled_unique_transition() {
                Structure::new_unique(
                    vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                    *self,
                )
            } else {
                // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                *self
            };
            if !map.deleted.empty() {
                entry.offset = map.deleted.pop();
            } else {
                entry.offset = self.get_slots_size() as _;
            }
            unwrap_unchecked(map.table.as_mut()).insert(name, entry);
            *offset = entry.offset;
            return map;
        }

        // existing transition check
        if let Some(map) = self.transitions.find(name, attributes) {
            *offset = map.added.1.offset;

            return map;
        }
        if self.transit_count > 32 {
            // stop transition
            let mut map = Structure::new_unique(
                vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
                *self,
            );
            // go to above unique path
            return map.add_property_transition(vm, name, attributes, offset);
        }
        let mut map = Structure::new(
            vm, // Heap::from_raw is safe here as there is no way to allocate JsObject not in the GC heap.
            *self,
        );

        if !map.deleted.empty() {
            let slot = map.deleted.pop();
            map.added = (
                name,
                MapEntry {
                    offset: slot,
                    attrs: attributes,
                },
            );
            map.calculated_size = self.get_slots_size() as _;
        } else {
            map.added = (
                name,
                MapEntry {
                    offset: self.get_slots_size() as _,
                    attrs: attributes,
                },
            );
            map.calculated_size = self.get_slots_size() as u32 + 1;
        }
        map.transit_count += 1;
        self.transitions.insert(vm, name, attributes, map);
        *offset = map.added.1.offset;
        assert!(map.get_slots_size() as u32 > map.added.1.offset);

        map
    }

    pub fn get(&mut self, vm: &mut VirtualMachine, name: Symbol) -> MapEntry {
        if !self.has_table() {
            if self.previous.is_none() {
                return MapEntry::not_found();
            }
            if self.is_adding_map() {
                if self.added.0 == name {
                    return self.added.1;
                }
            }
            self.allocate_table(vm);
        }
        let it = unwrap_unchecked(self.table.as_ref()).get(&name);

        it.copied().unwrap_or_else(MapEntry::not_found)
    }

    pub fn storage_capacity(&self) -> usize {
        let sz = self.get_slots_size();
        if sz == 0 {
            0
        } else if sz < 8 {
            8
        } else {
            fn clp2(number: usize) -> usize {
                let x = number - 1;
                let x = x | (x >> 1);
                let x = x | (x >> 2);
                let x = x | (x >> 4);
                let x = x | (x >> 8);
                let x = x | (x >> 16);
                x + 1
            }
            clp2(sz)
        }
    }
    pub fn change_prototype_with_no_transition(&mut self, prototype: Gc<JsObject>) -> Self {
        self.prototype = Some(prototype);
        *self
    }
}

impl Drop for Structure {
    fn drop(&mut self) {
        //println!("Ded");
    }
}

pub struct StructureBuilder {
    prototype: Option<Gc<JsObject>>,
    table: TargetTable,
}

impl StructureBuilder {
    pub fn new(prototype: Option<Gc<JsObject>>) -> Self {
        Self {
            prototype,
            table: TargetTable::new(),
        }
    }

    pub fn build(self, vm: &mut VirtualMachine, unique: bool, indexed: bool) -> Gc<Structure> {
        let Self { table, prototype } = self;
        Structure::new_from_table(vm, Some(table), prototype, unique, indexed)
    }

    pub fn add_with_index(&mut self, symbol: Symbol, index: usize, attributes: AttrSafe) {
        assert!(self.find(symbol).is_not_found());
        self.table.insert(
            symbol,
            MapEntry {
                offset: index as _,
                attrs: attributes,
            },
        );
    }

    pub fn add(&mut self, symbol: Symbol, attributes: AttrSafe) -> MapEntry {
        assert!(self.find(symbol).is_not_found());
        let index = self.table.len();
        let entry = MapEntry {
            offset: index as _,
            attrs: attributes,
        };
        self.table.insert(symbol, entry);
        entry
    }

    pub fn override_property(&mut self, symbol: Symbol, entry: MapEntry) {
        *self.table.get_mut(&symbol).unwrap() = entry;
    }

    pub fn find(&self, symbol: Symbol) -> MapEntry {
        self.table
            .get(&symbol)
            .copied()
            .unwrap_or_else(MapEntry::not_found)
    }
}
