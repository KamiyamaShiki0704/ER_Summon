//! Deferred lifetime retirement: detach first, observe native readers drain,
//! then destroy only from FrameEnd. Raw addresses remain owned by this queue.
use eldenring::cs::{ChrIns, FieldInsHandle, WorldChrMan};
use std::sync::Mutex;
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
};

pub(crate) const TASK_OFFSETS: [usize; 6] = [0x418, 0x448, 0x478, 0x4a8, 0x4d8, 0x508];
pub(crate) const VECTOR_OFFSETS: [usize; 3] = [0x1f1b0, 0x1f1d0, 0x1f200];

#[derive(Default)]
pub(crate) struct DrainGate {
    quiet_frames: u8,
}
impl DrainGate {
    fn observe(&mut self, references: Option<usize>, proxies_clear: bool) -> bool {
        if references != Some(0) || !proxies_clear {
            self.quiet_frames = 0;
            return false;
        }
        self.quiet_frames = self.quiet_frames.saturating_add(1);
        self.quiet_frames >= 2
    }
}

pub(crate) struct Retired {
    pub world: usize,
    pub player: usize,
    pub character: usize,
    pub owner: usize,
    pub destructor: usize,
    pub handle: FieldInsHandle,
    pub gate: DrainGate,
    pub last_observation: Option<(Option<usize>, bool)>,
}
static QUEUE: Mutex<Vec<Retired>> = Mutex::new(Vec::new());

pub(crate) fn pending() -> bool {
    QUEUE.lock().map_or(true, |q| !q.is_empty())
}
pub(crate) fn enqueue(value: Retired) {
    // Poisoning is not recoverable by freeing a potentially live engine object.
    if let Ok(mut queue) = QUEUE.lock() {
        queue.push(value);
    }
}

/// World transitions can invalidate resources even when this allocation was
/// detached. Leak quarantined storage rather than invoking a stale destructor.
pub(crate) fn quarantine_all(reason: &str) {
    if let Ok(mut queue) = QUEUE.lock() {
        if !queue.is_empty() {
            crate::log_line(format_args!(
                "summon retirement: phase=quarantined count={} reason={reason}; allocations retained until process exit",
                queue.len()
            ));
            queue.clear();
        }
    }
}

pub(crate) fn player_address(world: &WorldChrMan) -> usize {
    world
        .main_player
        .as_ref()
        .map_or(0, |p| p.as_ref() as *const _ as usize)
}

fn readable(address: usize, size: usize) -> bool {
    if address == 0 {
        return false;
    }
    let mut region = MEMORY_BASIC_INFORMATION::default();
    (unsafe {
        VirtualQuery(
            Some(address as *const _),
            &mut region,
            std::mem::size_of_val(&region),
        )
    }) != 0
        && region.State == MEM_COMMIT
        && region.Protect.0 & (PAGE_GUARD.0 | PAGE_NOACCESS.0) == 0
        && address.checked_add(size).is_some_and(|end| {
            (region.BaseAddress as usize)
                .checked_add(region.RegionSize)
                .is_some_and(|limit| end <= limit)
        })
}

fn word(address: usize) -> Option<usize> {
    readable(address, 8).then(|| unsafe { (address as *const usize).read_unaligned() })
}

fn count_vector(world: usize, offset: usize, stride: usize, pointer: usize) -> Option<usize> {
    let first = word(world + offset + 8)?;
    let last = word(world + offset + 16)?;
    let end = word(world + offset + 24)?;
    let bytes = last.checked_sub(first)?;
    if last > end || bytes % stride != 0 || bytes / stride > 65536 {
        return None;
    }
    if bytes == 0 {
        return Some(0);
    }
    if !readable(first, bytes) {
        return None;
    }
    Some(
        (0..bytes / stride)
            .filter(|i| unsafe {
                ((first + i * stride) as *const usize).read_unaligned() == pointer
            })
            .count(),
    )
}

fn references(world: usize, pointer: usize) -> Option<usize> {
    let mut count = 0;
    for (offset, stride) in VECTOR_OFFSETS.into_iter().zip([8, 16, 8]) {
        count += count_vector(world, offset, stride, pointer)?;
    }
    // WorldChrMan_PostPhysics also traverses this (character,next) chain.
    let mut node = word(world + 0x1e6a8)?;
    let mut visited = 0;
    while node != 0 {
        if visited >= 65536 {
            return None;
        }
        count += usize::from(word(node)? == pointer);
        node = word(node + 8)?;
        visited += 1;
    }
    Some(count)
}

/// Must only be called by the FrameEnd callback, after native frame work.
/// No shared engine list is edited; the engine rebuilds lists after detachment.
pub(crate) fn poll(world: &mut WorldChrMan) {
    let Ok(mut queue) = QUEUE.lock() else {
        return;
    };
    let current_world = world as *mut _ as usize;
    let current_player = player_address(world);
    let mut i = 0;
    while i < queue.len() {
        let item = &mut queue[i];
        // Do not dereference an orphan after a world/player transition. Keep its
        // storage quarantined rather than risking a second free during teardown.
        if item.world != current_world || item.player != current_player || current_player == 0 {
            crate::log_line(format_args!(
                "summon retirement: handle={} phase=quarantined reason=world-or-player-changed; allocation retained until process exit",
                item.handle
            ));
            queue.swap_remove(i);
            continue;
        }
        let refs = references(current_world, item.character);
        let proxies_clear = TASK_OFFSETS
            .iter()
            .all(|offset| word(item.character + offset + 0x10) == Some(0));
        let observation = (refs, proxies_clear);
        if item.last_observation != Some(observation) {
            crate::log_line(format_args!(
                "summon retirement: handle={} phase=draining references={refs:?} task_proxies_clear={proxies_clear}",
                item.handle
            ));
            item.last_observation = Some(observation);
        }
        if !item.gate.observe(refs, proxies_clear) {
            i += 1;
            continue;
        }
        let item = queue.swap_remove(i);
        crate::log_line(format_args!(
            "summon retirement: handle={} phase=before-destructor character={:#x} quiet_frame_ends=2",
            item.handle, item.character
        ));
        // The lookup and both vtables were verified before detaching ownership.
        let destructor: unsafe extern "C" fn(*mut ChrIns, u32) =
            unsafe { std::mem::transmute(item.destructor) };
        let owner = unsafe { &*(item.owner as *const eldenring::dlkr::DLAllocator) };
        crate::chr_lifecycle::teardown_with_owner(
            |delete| unsafe { destructor(item.character as *mut ChrIns, delete as u32) },
            || (owner.vftable.deallocate)(owner, item.character as *const u8),
        );
        // The slot was detached earlier and may now be reused; NEVER clear it here.
        crate::log_line(format_args!(
            "summon removed: handle={} outcome=destroyed retirement=frame-end-drained",
            item.handle
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_reader_blocks_free_until_two_quiet_frame_ends() {
        let mut gate = DrainGate::default();
        assert!(!gate.observe(Some(1), true)); // exact failure pattern: old list owns pointer
        assert!(!gate.observe(Some(0), false)); // still registered task
        assert!(!gate.observe(Some(0), true));
        assert!(!gate.observe(None, true)); // failed observation resets progress
        assert!(!gate.observe(Some(0), true));
        assert!(gate.observe(Some(0), true));
    }
    #[test]
    fn bound_world_vectors_match_native_consumers() {
        assert_eq!(
            std::mem::offset_of!(WorldChrMan, chr_inses_by_distance),
            0x1f1d0
        );
        assert_eq!(
            std::mem::offset_of!(WorldChrMan, chr_inses_by_update_priority),
            0x1f200
        );
        assert_eq!(
            std::mem::size_of::<eldenring::cs::ChrInsDistanceEntry>(),
            16
        );
    }
}
