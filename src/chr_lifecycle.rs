//! Verified engine-level character lifecycle operations.
//!
//! Everything here is grounded in disassembly of the supported executables rather
//! than in field-name guesses. Native byte fixtures are retained under
//! `tests/fixtures/` for the verified paths.
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

use eldenring::dlkr::DLAllocator;
use std::fmt;
use std::ptr::NonNull;
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, VirtualQuery,
};

use eldenring::cs::{
    BlockId, ChrIns, ChrSet, ChrSetEntry, FieldInsHandle, FieldInsSelector, WorldChrMan,
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
    /// Detached and queued; not yet freed.
    RetirementStarted,
    /// Native allocator-owner lookup could not be verified; leave the unit inert.
    AllocatorUnavailable,

    /// The handle and the character disagree about the owning entry, so nothing
    /// was freed.
    EntryMismatch,
    /// The handle does not resolve and no entry was reachable.
    NotFound,
}

impl DestroyOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RetirementStarted => "retirement-started",
            Self::AllocatorUnavailable => "allocator-unavailable",
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
fn entry_from_index(
    chr_set: &ChrSet<ChrIns>,
    handle: FieldInsHandle,
) -> Option<NonNull<ChrSetEntry<ChrIns>>> {
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

    let mut already_registered = false;
    {
        let Some(chr_set) = chr_set_for_handle(world_chr_man, handle) else {
            return EntityIdOutcome::Unavailable;
        };
        if let Some(existing) = chr_set.entity_id_mapping.find(&key).copied() {
            if existing == entry {
                already_registered = true;
            } else {
                return EntityIdOutcome::Conflict;
            }
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
    if !already_registered {
        chr_set.entity_id_mapping.insert(key, entry);
    }
    // Native registration also builds the group map and notifies the event
    // manager. A round-trip through entity_id_mapping alone is not an EMEVD test.
    let group_before = chr_set.group_id_mapping.iter()
        .filter(|p| p.first == key && p.second == entry).count();
    let character = unsafe { entry.as_ref() }.chr_ins;
    let mut native_called = false;
    if group_before == 0 {
        if let (Some(register), Some(character)) = (native_event_groups(chr_set), character) {
            unsafe { register(chr_set, character.as_ptr(), entry.as_ptr()); }
            native_called = true;
        }
    }
    let group_after = chr_set.group_id_mapping.iter()
        .filter(|p| p.first == key && p.second == entry).count();
    crate::log_line(format_args!(
        "summon event registration: handle={handle} entity_id={key} native_groups_called={native_called} group_before={group_before} group_after={group_after} emevd_lookup=unverified"
    ));
    if already_registered { EntityIdOutcome::AlreadyRegisteredByEngine }
    else { EntityIdOutcome::RegisteredByDll }
}

type RegisterEventGroups = unsafe extern "C" fn(*mut ChrSet<ChrIns>, *mut ChrIns, *mut ChrSetEntry<ChrIns>) -> u8;

fn event_groups_code_matches(code: &[u8]) -> bool {
    code == include_bytes!("../tests/fixtures/ww271-register-event-groups.bin")
}

fn native_event_groups(set: &ChrSet<ChrIns>) -> Option<RegisterEventGroups> {
    // Exact-image candidate: verify both the known free routine and the complete
    // group registration body before using this RVA. Never call on a near match.
    let vtable = unsafe { *(set as *const _ as *const *const usize) };
    let free = unsafe { *vtable.add(8) };
    let region = executable_region(free, 0x100)?;
    let base = region.AllocationBase as usize;
    if free.checked_sub(base)? != 0x495890 { return None; }
    let free_code = unsafe { std::slice::from_raw_parts(free as *const u8, 0x100) };
    if free_code != include_bytes!("../tests/fixtures/ww271-chrset-free-prefix.bin") { return None; }
    let address = base.checked_add(0x494800)?;
    if executable_region(address, 0x198)?.AllocationBase != region.AllocationBase { return None; }
    let code = unsafe { std::slice::from_raw_parts(address as *const u8, 0x198) };
    if !event_groups_code_matches(code) { return None; }
    Some(unsafe { std::mem::transmute::<usize, RegisterEventGroups>(address) })
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
pub(crate) fn chr_set_usage(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
) -> (usize, u32) {
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

/// Starts native detachment, exclusively from FrameEnd. No storage is freed
/// here: the retirement queue waits for native readers and task proxies to drain.
/// Ownership transfers to that queue after native detach succeeds.
pub(crate) fn destroy_summon(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
) -> DestroyOutcome {
    let entry = match lookup_entry(world_chr_man, handle) {
        EntryLookup::Resolved(resolved) => resolved.entry,
        EntryLookup::Mismatch => return DestroyOutcome::EntryMismatch,
        // An unresolved handle is not proof that its index still belongs to us.
        EntryLookup::Unavailable => return DestroyOutcome::NotFound,
    };

    // Resolved means the engine hands this character out for this handle, so the
    // pointer below is the one the engine itself would act on.
    let Some(chr_ptr) = world_chr_man
        .chr_ins_by_handle_mut(&handle)
        .map(|chr| chr as *mut ChrIns)
    else {
        return DestroyOutcome::NotFound;
    };

    // Resolve ownership BEFORE destruction, exactly as ChrSet::free_chr_list does.
    // The deleting destructor uses CRT delete, which is not valid for every
    // character allocator (see the 2026-09-23 heap-corruption dump).
    let Some(owner_lookup) = native_owner_lookup(world_chr_man, handle) else {
        return DestroyOutcome::AllocatorUnavailable;
    };
    let Some(owner) = NonNull::new(unsafe { owner_lookup(chr_ptr.cast()) }) else {
        return DestroyOutcome::AllocatorUnavailable;
    };

    let Some(detach) = native_detach(world_chr_man, handle) else {
        return DestroyOutcome::AllocatorUnavailable;
    };
    let Some(task_cleanup) = verified_task_cleanup(chr_ptr as usize) else {
        return DestroyOutcome::AllocatorUnavailable;
    };
    let destructor = unsafe { (&*chr_ptr).vftable.destructor } as usize;
    let world_address = world_chr_man as *mut _ as usize;
    let player = crate::retirement::player_address(world_chr_man);
    if player == 0 {
        return DestroyOutcome::AllocatorUnavailable;
    }
    release_event_ids_for_handle(world_chr_man, handle);
    clear_player_lock_on(world_chr_man, handle);
    clear_debug_creator_pointer(world_chr_man, chr_ptr.cast_const());

    // Native free_task nulls the proxy's subject and queues its unregistration.
    // Do this while the character and all modules are still alive.
    for offset in crate::retirement::TASK_OFFSETS {
        unsafe { task_cleanup((chr_ptr as usize + offset) as *mut u8) };
    }
    let set = chr_set_for_handle(world_chr_man, handle).expect("validated set");
    let detached = unsafe { detach(set, handle.selector.index()) };
    if detached != chr_ptr {
        crate::log_line(format_args!(
            "summon retirement warning: native detach returned unexpected pointer for {handle}; no free attempted"
        ));
        return DestroyOutcome::EntryMismatch;
    }
    // Native detach cleared the entry and id/group mappings. Do not write the
    // slot again, especially after it can be reused by a subsequent summon.
    let _ = entry;
    crate::retirement::enqueue(crate::retirement::Retired {
        world: world_address,
        player,
        character: chr_ptr as usize,
        owner: owner.as_ptr() as usize,
        destructor,
        handle,
        gate: Default::default(),
        last_observation: None,
    });
    crate::log_line(format_args!(
        "summon retirement: handle={handle} phase=detached character={chr_ptr:p} tasks_unregistered=6"
    ));
    DestroyOutcome::RetirementStarted
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
    (
        release_event_ids_for_handle(world_chr_man, handle),
        consistent,
    )
}

// Resolve the same owner lookup called by this character set's native cleanup.
// The exact instruction context includes destructor(flag=0) and deallocate(+0x68).
// Unknown or patched code fails closed; no guessed RVA or CRT allocator fallback.
const OWNER_CALL_PREFIX: &[u8] = &[0x48, 0x8b, 0xc8, 0xe8];
const OWNER_CALL_SUFFIX: &[u8] = &[
    0x4d, 0x8b, 0x06, 0x33, 0xd2, 0x49, 0x8b, 0xce, 0x48, 0x8b, 0xf8, 0x41, 0xff, 0x50, 0x08, 0x4c,
    0x8b, 0x07, 0x49, 0x8b, 0xd6, 0x48, 0x8b, 0xcf, 0x41, 0xff, 0x50, 0x68,
];

type OwnerLookup = unsafe extern "C" fn(*const u8) -> *mut DLAllocator;

fn owner_lookup_address(code: &[u8], base: usize) -> Option<usize> {
    let mut found = None;
    for (offset, window) in code.windows(8 + OWNER_CALL_SUFFIX.len()).enumerate() {
        if window.starts_with(OWNER_CALL_PREFIX) && &window[8..] == OWNER_CALL_SUFFIX {
            if found.is_some() {
                return None;
            }
            let delta = i32::from_le_bytes(window[4..8].try_into().ok()?);
            found = Some(
                base.checked_add(offset + 8)?
                    .checked_add_signed(delta as isize)?,
            );
        }
    }
    found
}

fn executable_region(address: usize, size: usize) -> Option<MEMORY_BASIC_INFORMATION> {
    let mut region = MEMORY_BASIC_INFORMATION::default();
    if unsafe {
        VirtualQuery(
            Some(address as *const _),
            &mut region,
            std::mem::size_of_val(&region),
        )
    } == 0
        || region.State != MEM_COMMIT
        || region.Protect.0 & 0xf0 == 0
        || region.Protect.0 & PAGE_GUARD.0 != 0
        || address.checked_add(size)?
            > (region.BaseAddress as usize).checked_add(region.RegionSize)?
    {
        return None;
    }
    Some(region)
}

fn native_owner_lookup(world: &mut WorldChrMan, handle: FieldInsHandle) -> Option<OwnerLookup> {
    let set = chr_set_for_handle(world, handle)?;
    // ChrSet vtable slot 8 is free_chr_list; the binding keeps this vtable private.
    let vtable = unsafe { *(set as *const ChrSet<ChrIns> as *const *const usize) };
    let native_free = unsafe { *vtable.add(8) };
    let region = executable_region(native_free, 0x100)?;
    let code = unsafe { std::slice::from_raw_parts(native_free as *const u8, 0x100) };
    let address = owner_lookup_address(code, native_free)?;
    let target_region = executable_region(address, 1)?;
    if target_region.AllocationBase != region.AllocationBase {
        return None;
    }
    Some(unsafe { std::mem::transmute::<usize, OwnerLookup>(address) })
}

// Shared by production teardown and the allocator protocol regression test.
pub(crate) fn teardown_with_owner(destruct: impl FnOnce(bool), deallocate: impl FnOnce()) {
    destruct(false);
    deallocate();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn event_groups_binding_rejects_modified_or_truncated_native_body() {
        let code = include_bytes!("../tests/fixtures/ww271-register-event-groups.bin");
        assert!(event_groups_code_matches(code));
        assert!(!event_groups_code_matches(&code[..code.len()-1]));
        let mut modified = *code;
        modified[0x15e] ^= 1; // event-manager notification call must stay intact
        assert!(!event_groups_code_matches(&modified));
    }

    #[test]
    fn native_owner_lookup_matches_shipped_ww271_code_and_rejects_changes() {
        let code = include_bytes!("../tests/fixtures/ww271-chrset-free-prefix.bin");
        // Fixture is from the verified WW 2.7.1.0 executable, RVA 0x495890.
        assert_eq!(owner_lookup_address(code, 0x495890), Some(0xe1c080));
        let mut modified = *code;
        modified[0x65] ^= 1; // change xor edx,edx: destructor no longer verified
        assert_eq!(owner_lookup_address(&modified, 0x495890), None);
        let duplicate = [code.as_slice(), code.as_slice()].concat();
        assert_eq!(owner_lookup_address(&duplicate, 0x495890), None);
        assert_eq!(owner_lookup_address(&code[..0x70], 0x495890), None);
    }

    #[test]
    fn teardown_does_not_crt_delete_an_allocator_owned_character() {
        let calls = RefCell::new(Vec::new());
        teardown_with_owner(
            |delete| {
                assert!(
                    !delete,
                    "CRT delete on an owner-allocated character causes heap corruption"
                );
                calls.borrow_mut().push("destruct");
            },
            || calls.borrow_mut().push("owner-deallocate"),
        );
        assert_eq!(*calls.borrow(), ["destruct", "owner-deallocate"]);
    }
}

type Detach = unsafe extern "C" fn(*mut ChrSet<ChrIns>, u32) -> *mut ChrIns;
type TaskCleanup = unsafe extern "C" fn(*mut u8);
const DETACH_PREFIX: &[u8] = &[0x8b, 0xd6, 0x48, 0x8b, 0xcb, 0xe8];
const DETACH_SUFFIX: &[u8] = &[0x4c, 0x8b, 0xf0, 0x48, 0x85, 0xc0, 0x74];
fn detach_address(code: &[u8], base: usize) -> Option<usize> {
    let mut found = None;
    for (i, w) in code.windows(10 + DETACH_SUFFIX.len()).enumerate() {
        if w.starts_with(DETACH_PREFIX) && &w[10..] == DETACH_SUFFIX {
            if found.is_some() {
                return None;
            }
            let delta = i32::from_le_bytes(w[6..10].try_into().ok()?);
            found = Some(
                base.checked_add(i + 10)?
                    .checked_add_signed(delta as isize)?,
            );
        }
    }
    found
}
fn native_detach(world: &mut WorldChrMan, handle: FieldInsHandle) -> Option<Detach> {
    let set = chr_set_for_handle(world, handle)?;
    let vtable = unsafe { *(set as *const _ as *const *const usize) };
    let native_free = unsafe { *vtable.add(8) };
    let region = executable_region(native_free, 0x100)?;
    let code = unsafe { std::slice::from_raw_parts(native_free as *const u8, 0x100) };
    let target = detach_address(code, native_free)?;
    if executable_region(target, 1)?.AllocationBase != region.AllocationBase {
        return None;
    }
    Some(unsafe { std::mem::transmute::<usize, Detach>(target) })
}
fn verified_task_cleanup(character: usize) -> Option<TaskCleanup> {
    let mut common = None;
    for offset in crate::retirement::TASK_OFFSETS {
        let vtable = unsafe { *((character + offset) as *const *const usize) };
        let function = unsafe { *vtable.add(5) };
        executable_region(function, 0x25)?;
        let code = unsafe { std::slice::from_raw_parts(function as *const u8, 0x25) };
        // Exact verified body except the rel32 call displacement. Different
        // builds are refused rather than executing a vaguely similar function.
        if !task_cleanup_matches(code) {
            return None;
        }
        let delta = i32::from_le_bytes(code[0x13..0x17].try_into().ok()?);
        let unregister = function
            .checked_add(0x17)?
            .checked_add_signed(delta as isize)?;
        if executable_region(unregister, 1)?.AllocationBase
            != executable_region(function, 1)?.AllocationBase
        {
            return None;
        }
        if common.is_some_and(|previous| previous != function) {
            return None;
        }
        common = Some(function);
    }
    Some(unsafe { std::mem::transmute::<usize, TaskCleanup>(common?) })
}

fn task_cleanup_matches(code: &[u8]) -> bool {
    let expected = include_bytes!("../tests/fixtures/ww271-task-free.bin");
    code.len() == expected.len()
        && code[..0x13] == expected[..0x13]
        && code[0x17..] == expected[0x17..]
}

#[cfg(test)]
mod retirement_binding_tests {
    use super::*;
    #[test]
    fn native_detach_and_task_free_are_verified_before_mutation() {
        let code = include_bytes!("../tests/fixtures/ww271-chrset-free-prefix.bin");
        assert_eq!(detach_address(code, 0x495890), Some(0x494f10));
        let mut bad = *code;
        bad[0x46] ^= 1;
        assert_eq!(detach_address(&bad, 0x495890), None);
        let tasks = include_bytes!("../tests/fixtures/ww271-task-free.bin");
        assert!(task_cleanup_matches(tasks));
        let mut bad_task = *tasks;
        bad_task[0x1a] ^= 1;
        assert!(!task_cleanup_matches(&bad_task));
    }
}
