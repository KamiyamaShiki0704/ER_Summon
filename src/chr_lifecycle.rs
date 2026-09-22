//! Verified engine-level character lifecycle operations.
//!
//! Everything here is grounded in disassembly of the supported executables rather
//! than in field-name guesses. Evidence table:
//! `docs/compatibility/engine-chr-lifecycle.md`.
//!
//! Verified against WW 2.7.1.0 (image base 0x140000000):
//!
//! ```text
//! ChrIns scalar deleting destructor, vftable slot 1, RVA 0x3E81F0
//!     test bl, 1 / je skip
//!     mov edx, 0x580 / call operator delete      -> ChrIns size is 0x580
//!     rva 0x3E7970 is the body that runs unconditionally
//!
//! ChrSet::free_chr_list, vftable slot 8, RVA 0x495890
//!     for each index < capacity:
//!         chr = entries[i]                        (stride 0x10, chr_ins at +0x00)
//!         if chr != null: dtor(chr, 0); allocator->deallocate(chr)
//!     clear entity_id_mapping (+0x28) and group_id_mapping (+0x40)
//!
//! DLMap (MSVC std::map) layout, from the map clearing function at RVA 0x4972E0
//!     +0x00 allocator pointer, deallocation through its vtable slot 0x68
//!     +0x08 node head
//!     +0x10 size
//!     node: left +0x00, parent +0x08, right +0x10, color +0x18, is_nil +0x19,
//!           value +0x20
//! ```
//!
//! The `DLMap` operations used below go through `fromsoftware_shared_stl`, whose
//! `Map` reimplements this exact layout and allocates through the game's own
//! allocator object.
//!
//! # Entry resolution
//!
//! Every write below acts on one `ChrSetEntry`. That entry is resolved from three
//! independent signals and the resolution is only accepted when the first two
//! agree, because acting on the wrong entry would clear or free another, live
//! character:
//!
//! 1. the index encoded in the `FieldInsHandle`'s selector, applied to
//!    `ChrSet::entries`,
//! 2. the character pointer the engine's own `get_chr_ins_by_handle` returns for
//!    that same handle — the entry must own exactly that character,
//! 3. the back pointer the character stores on itself (`ChrIns + 0x10`), which is
//!    reported but not required: a character that has not been registered yet
//!    stores a null there.

use std::fmt;
use std::ptr::NonNull;

use eldenring::cs::{
    BlockId, ChrIns, ChrLoadStatus, ChrSet, ChrSetEntry, ChrUpdateType, FieldInsHandle,
    FieldInsSelector, WorldChrMan,
};

/// Reported outcome of an entity id registration attempt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntityIdOutcome {
    /// Registration is disabled or no entity id is configured.
    NotConfigured,
    /// The engine had already registered the requested id for this character.
    AlreadyRegisteredByEngine,
    /// The id was written to the character and registered by this DLL.
    RegisteredByDll,
    /// Another character in the same character set already owns the id.
    Conflict,
    /// The handle and the character disagree about the owning entry.
    EntryMismatch,
    /// The character or its character set entry could not be resolved.
    Unavailable,
}

impl EntityIdOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "not-configured",
            Self::AlreadyRegisteredByEngine => "engine-registered",
            Self::RegisteredByDll => "dll-registered",
            Self::Conflict => "conflict",
            Self::EntryMismatch => "entry-mismatch",
            Self::Unavailable => "unavailable",
        }
    }
}

impl fmt::Display for EntityIdOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reported outcome of a real removal attempt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DestroyOutcome {
    /// The character was destroyed and its character set entry cleared.
    Destroyed,
    /// The handle no longer resolves, but a leftover entry was cleared.
    EntryCleared,
    /// The handle and the character disagree about the owning entry, so nothing
    /// was freed.
    EntryMismatch,
    /// The handle does not resolve and no entry was reachable.
    NotFound,
}

impl DestroyOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Destroyed => "destroyed",
            Self::EntryCleared => "entry-cleared",
            Self::EntryMismatch => "entry-mismatch",
            Self::NotFound => "not-found",
        }
    }
}

impl fmt::Display for DestroyOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn chr_set_for_handle(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
) -> Option<&mut ChrSet<ChrIns>> {
    let chr_set_index = handle.selector.container() as usize;
    let chr_set: &mut ChrSet<ChrIns> = world_chr_man.chr_sets.get_mut(chr_set_index)?.as_mut()?;
    Some(chr_set)
}

/// Entry the handle's own index points at.
fn entry_from_index(chr_set: &ChrSet<ChrIns>, handle: FieldInsHandle) -> Option<NonNull<ChrSetEntry<ChrIns>>> {
    let index = handle.selector.index() as usize;
    if index >= chr_set.capacity as usize {
        return None;
    }
    NonNull::new(unsafe { chr_set.entries.as_ptr().add(index) })
}

/// Reads the character's own back pointer as an address.
///
/// The field is typed as `NonNull`, but a character that has not been registered
/// in a character set yet stores a null there, which is not a valid `NonNull`
/// value. Reading the raw address avoids ever constructing that value.
fn back_pointer_address(chr: &ChrIns) -> usize {
    unsafe {
        std::ptr::addr_of!(chr.chr_set_entry)
            .cast::<usize>()
            .read_unaligned()
    }
}

/// A character set entry that the handle was resolved to.
struct ResolvedEntry {
    entry: NonNull<ChrSetEntry<ChrIns>>,
    /// The character's own back pointer names this same entry.
    back_pointer_agrees: bool,
}

impl ResolvedEntry {
    /// True when all three signals line up.
    fn is_fully_consistent(&self) -> bool {
        self.back_pointer_agrees
    }
}

enum EntryLookup {
    Resolved(ResolvedEntry),
    /// Entry and character disagree, so no write is safe.
    Mismatch,
    Unavailable,
}

fn lookup_entry(world_chr_man: &mut WorldChrMan, handle: FieldInsHandle) -> EntryLookup {
    let Some(chr_set) = chr_set_for_handle(world_chr_man, handle) else {
        return EntryLookup::Unavailable;
    };
    let Some(entry) = entry_from_index(chr_set, handle) else {
        return EntryLookup::Unavailable;
    };
    let Some(chr_ptr) = chr_set
        .chr_ins_by_handle_mut(&handle)
        .map(|chr| chr as *mut ChrIns)
    else {
        return EntryLookup::Unavailable;
    };

    // The indexed entry has to own the very character the engine hands out for
    // this handle. Without this the entry could belong to a different character.
    let entry_chr = unsafe { entry.as_ref().chr_ins };
    if entry_chr.map(NonNull::as_ptr) != Some(chr_ptr) {
        return EntryLookup::Mismatch;
    }

    let chr = unsafe { &*chr_ptr };
    EntryLookup::Resolved(ResolvedEntry {
        entry,
        back_pointer_agrees: back_pointer_address(chr) == entry.as_ptr() as usize,
    })
}

/// Makes the configured `event_entity_id` real for a freshly bound summon.
///
/// The debug character creator receives the id through its init data, but nothing
/// guarantees that the id ends up both on the character and in the owning
/// `ChrSet` event id mapping that event scripts resolve against. This writes the
/// character field and registers the mapping entry when the engine has not done
/// so, and reports which of the two happened.
pub(crate) fn register_event_entity_id(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
    event_entity_id: i32,
) -> EntityIdOutcome {
    if event_entity_id <= 0 {
        return EntityIdOutcome::NotConfigured;
    }
    let entry = match lookup_entry(world_chr_man, handle) {
        EntryLookup::Resolved(resolved) => resolved.entry,
        EntryLookup::Mismatch => return EntityIdOutcome::EntryMismatch,
        EntryLookup::Unavailable => return EntityIdOutcome::Unavailable,
    };
    let key = event_entity_id as u32;

    {
        let Some(chr_set) = chr_set_for_handle(world_chr_man, handle) else {
            return EntityIdOutcome::Unavailable;
        };
        if let Some(existing) = chr_set.entity_id_mapping.find(&key).copied() {
            if existing == entry {
                return EntityIdOutcome::AlreadyRegisteredByEngine;
            }
            return EntityIdOutcome::Conflict;
        }
    }

    // Field first, mapping second: a mapping entry pointing at an id the
    // character does not report would be worse than no mapping at all.
    let Some(chr) = world_chr_man.chr_ins_by_handle_mut(&handle) else {
        return EntityIdOutcome::Unavailable;
    };
    chr.event_entity_id = key;

    let Some(chr_set) = chr_set_for_handle(world_chr_man, handle) else {
        return EntityIdOutcome::Unavailable;
    };
    chr_set.entity_id_mapping.insert(key, entry);
    EntityIdOutcome::RegisteredByDll
}

/// Whether the character behind `handle` still reports `event_entity_id`.
pub(crate) fn character_reports_event_entity_id(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
    event_entity_id: i32,
) -> Option<bool> {
    let chr = world_chr_man.chr_ins_by_handle_mut(&handle)?;
    Some(chr.event_entity_id == event_entity_id as u32)
}

/// Removes every event id that still points at this character's entry.
///
/// The character may be registered under ids this DLL never configured, so the
/// teardown cannot work from a known key: it sweeps the mapping for entries that
/// are about to stop resolving and drops all of them. Returns how many ids were
/// removed.
pub(crate) fn release_event_ids_for_handle(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
) -> usize {
    let entry = match lookup_entry(world_chr_man, handle) {
        EntryLookup::Resolved(resolved) => resolved.entry,
        // An unreachable handle cannot own a mapping entry this DLL could safely
        // attribute, so there is nothing to sweep.
        _ => return 0,
    };
    let Some(chr_set) = chr_set_for_handle(world_chr_man, handle) else {
        return 0;
    };

    let owned = chr_set
        .entity_id_mapping
        .iter()
        .filter(|pair| pair.second == entry)
        .map(|pair| pair.first)
        .collect::<Vec<_>>();

    let mut removed = 0;
    for key in owned {
        if chr_set.entity_id_mapping.remove(&key).is_some() {
            removed += 1;
        }
    }
    removed
}

/// Looks up which character currently owns `event_entity_id` in the set holding `handle`.
pub(crate) fn entity_id_owner(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
    event_entity_id: i32,
) -> Option<FieldInsHandle> {
    if event_entity_id <= 0 {
        return None;
    }
    let chr_set = chr_set_for_handle(world_chr_man, handle)?;
    let entry = chr_set
        .entity_id_mapping
        .find(&(event_entity_id as u32))
        .copied()?;
    let chr_ins = unsafe { entry.as_ref() }.chr_ins?;
    Some(unsafe { chr_ins.as_ref() }.field_ins_handle)
}

/// Number of registered event ids and the capacity of the character set holding `handle`.
pub(crate) fn chr_set_usage(world_chr_man: &mut WorldChrMan, handle: FieldInsHandle) -> (usize, u32) {
    let Some(chr_set) = chr_set_for_handle(world_chr_man, handle) else {
        return (0, 0);
    };
    (chr_set.entity_id_mapping.len(), chr_set.capacity)
}

/// Drops the player's lock-on when it still points at `handle`.
///
/// Lock-on state is consumed by the camera and targeting systems every frame, so
/// clearing it before the character is freed keeps them from holding a handle
/// that is about to stop resolving.
fn clear_player_lock_on(world_chr_man: &mut WorldChrMan, handle: FieldInsHandle) {
    let Some(player) = world_chr_man.main_player.as_mut() else {
        return;
    };
    if !player.locked_on_enemy.is_empty() && player.locked_on_enemy == handle {
        player.locked_on_enemy = FieldInsHandle {
            selector: FieldInsSelector(u32::MAX),
            block_id: BlockId::none(),
        };
    }
}

/// Clears the character creator's reference to a character that is being freed.
fn clear_debug_creator_pointer(world_chr_man: &mut WorldChrMan, chr: *const ChrIns) {
    let creator = &mut world_chr_man.debug_chr_creator;
    if let Some(last) = creator.last_created_chr
        && std::ptr::eq(last.as_ptr().cast_const().cast::<ChrIns>(), chr)
    {
        creator.last_created_chr = None;
    }
}

/// Clears the owning `ChrSet` entry so the slot stops referencing the character.
fn clear_chr_set_entry(entry: NonNull<ChrSetEntry<ChrIns>>) {
    let entry = entry.as_ptr();
    unsafe {
        (*entry).chr_ins = None;
        (*entry).chr_load_status = ChrLoadStatus::Unloaded;
        (*entry).chr_update_type = ChrUpdateType::Local;
        (*entry).entry_flags = 0;
    }
}

/// Really removes a summon from game memory.
///
/// Order:
///
/// 1. drop every event id mapping that still points at the unit, so event scripts
///    can no longer resolve it,
/// 2. drop the player's lock-on if it still targets the unit,
/// 3. clear the character creator's reference to the unit,
/// 4. run the engine destructor through the character vtable with the delete flag,
///    which tears the character down and frees its 0x580 byte allocation,
/// 5. clear the owning `ChrSet` entry so the slot no longer aliases freed memory.
///
/// The caller must have made the character inert beforehand; see the staged
/// removal path in the main module.
pub(crate) fn destroy_summon(world_chr_man: &mut WorldChrMan, handle: FieldInsHandle) -> DestroyOutcome {
    let entry = match lookup_entry(world_chr_man, handle) {
        EntryLookup::Resolved(resolved) => resolved.entry,
        EntryLookup::Mismatch => return DestroyOutcome::EntryMismatch,
        EntryLookup::Unavailable => {
            // The handle no longer resolves. If a slot is still reachable it is a
            // leftover, so clear it rather than leave it aliasing freed memory.
            let Some(chr_set) = chr_set_for_handle(world_chr_man, handle) else {
                return DestroyOutcome::NotFound;
            };
            let Some(entry) = entry_from_index(chr_set, handle) else {
                return DestroyOutcome::NotFound;
            };
            if unsafe { entry.as_ref().chr_ins }.is_none() {
                return DestroyOutcome::NotFound;
            }
            clear_chr_set_entry(entry);
            return DestroyOutcome::EntryCleared;
        }
    };

    // Resolved means the engine hands this character out for this handle, so the
    // pointer below is the one the engine itself would act on.
    let Some(chr_ptr) = world_chr_man
        .chr_ins_by_handle_mut(&handle)
        .map(|chr| chr as *mut ChrIns)
    else {
        return DestroyOutcome::NotFound;
    };

    // The mapping sweep has to run while the character is still reachable, since
    // it is the character that tells us which entry the ids belong to.
    release_event_ids_for_handle(world_chr_man, handle);
    clear_player_lock_on(world_chr_man, handle);
    clear_debug_creator_pointer(world_chr_man, chr_ptr.cast_const());

    // Slot 1 of the ChrIns vtable is the engine destructor. The delete flag makes
    // it call operator delete with the verified 0x580 byte size; the exe links its
    // CRT statically and that free ends in HeapFree on the CRT heap, which is the
    // same memory `ChrSet::free_chr_list` releases.
    //
    // The reference taken here is explicit on purpose: reaching the vtable through
    // an implicit autoref of the raw pointer deref is what edition 2024 denies.
    let destructor = {
        let chr = unsafe { &*chr_ptr };
        chr.vftable.destructor
    };
    unsafe { destructor(&mut *chr_ptr, 1) };

    clear_chr_set_entry(entry);
    DestroyOutcome::Destroyed
}

/// Releases every event id of a summon that is on its way out.
///
/// Returns the number of ids removed and whether the unit is fully consistent, so
/// the caller can log a removal that had to work from a half-resolved entry.
pub(crate) fn release_summon_event_ids(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
) -> (usize, bool) {
    let consistent = match lookup_entry(world_chr_man, handle) {
        EntryLookup::Resolved(resolved) => resolved.is_fully_consistent(),
        EntryLookup::Mismatch => false,
        EntryLookup::Unavailable => return (0, false),
    };
    (release_event_ids_for_handle(world_chr_man, handle), consistent)
}
