//! Explicit FrameEnd ownership retirement. Known-reader drain is advisory, not authority.
use shikifsrs_elden_ring::{
    character_handles::PrimaryPlayerTargetClear,
    character_sets::{CharacterIdentity, CharacterRegistrationObservation},
    current_character_events::{CurrentCharacterEvents, EventEntityRelease},
    current_character_lifecycle::{
        CharacterRetirementObservation, CurrentCharacterLifecycle, NativeCharacterDestruction,
        NativeCharacterDetach, NativeCharacterOwnerQuery,
    },
    current_character_tasks::{CharacterTaskClear, CurrentCharacterTasks},
    current_control_links::CurrentControlLinks,
};
use shikifsrs_runtime::{Error, Result};
use std::{sync::Arc, time::Instant};
pub fn accepts_frame_end(group: u32) -> bool {
    matches!(group, 168 | 0x9000_00a8)
}
#[derive(Default)]
pub struct DrainGate {
    quiet_frames: u8,
}
impl DrainGate {
    pub fn observe(&mut self, references: Option<usize>, clear: bool) -> bool {
        if references != Some(0) || !clear {
            self.quiet_frames = 0;
            return false;
        }
        self.quiet_frames = self.quiet_frames.saturating_add(1);
        self.quiet_frames >= 2
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Prepare,
    Selection,
    Aliases,
    ClearTask(usize),
    Detach,
    Destroy,
    Free,
}
pub trait Driver {
    fn perform(&mut self, step: Step) -> Result<()>;
    fn observe(&mut self) -> Result<(usize, bool)>;
}
#[derive(Default)]
pub struct Protocol {
    next: usize,
    pub failure: Option<Error>,
    gate: DrainGate,
    pub complete: bool,
}
impl Protocol {
    /// Publish the actual native driver record before advancing. Failure blocks all retries.
    pub fn advance(&mut self, driver: &mut impl Driver) -> Result<()> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        if self.complete {
            return Ok(());
        }
        let result: Result<()> = (|| {
            while self.next < 10 {
                let step = match self.next {
                    0 => Step::Prepare,
                    1 => Step::Selection,
                    2 => Step::Aliases,
                    3..=8 => Step::ClearTask(self.next - 3),
                    _ => Step::Detach,
                };
                driver.perform(step)?;
                self.next += 1;
            }
            if self.next == 10 {
                let (refs, clear) = driver.observe()?;
                if !self.gate.observe(Some(refs), clear) {
                    return Ok(());
                }
                // Additional guard only: native destruction still requires independent complete quiescence.
                driver.perform(Step::Destroy)?;
                self.next += 1;
            }
            if self.next == 11 {
                driver.perform(Step::Free)?;
                self.next += 1;
            }
            self.complete = true;
            Ok(())
        })();
        if let Err(e) = &result {
            self.failure = Some(e.clone())
        }
        result
    }
}
#[derive(Clone, Copy)]
pub struct Request {
    pub actor: CharacterRegistrationObservation,
    pub primary: CharacterIdentity,
    pub not_before: Instant,
}
pub struct NativeRecord {
    pub request: Request,
    pub owner_query: Option<NativeCharacterOwnerQuery>,
    pub selection: Option<PrimaryPlayerTargetClear>,
    pub aliases: Option<EventEntityRelease>,
    pub tasks: Vec<CharacterTaskClear>,
    pub detach: Option<NativeCharacterDetach>,
    pub last_observation: Option<CharacterRetirementObservation>,
    pub destruction: Option<NativeCharacterDestruction>,
    pub free: Option<Result<()>>,
}
impl NativeRecord {
    pub fn new(request: Request) -> Self {
        Self {
            request,
            owner_query: None,
            selection: None,
            aliases: None,
            tasks: Vec::with_capacity(6),
            detach: None,
            last_observation: None,
            destruction: None,
            free: None,
        }
    }
    pub fn quarantine(&mut self) {
        if let Some(query) = &mut self.owner_query
            && let Ok(ticket) = &mut query.prepared
        {
            ticket.quarantine()
        }
    }
}
pub struct NativeAccess {
    pub lifecycle: CurrentCharacterLifecycle,
    pub events: CurrentCharacterEvents,
    pub tasks: CurrentCharacterTasks,
    pub links: CurrentControlLinks,
    limit: usize,
}
impl NativeAccess {
    /// # Safety
    /// Each SDK's full graph/phase/heap/ownership/residency contract holds. In particular,
    /// complete reader/task quiescence is an experimental live-host obligation, never acquired here.
    pub unsafe fn bind(bytes: Arc<[u8]>, base: u64, epoch: u64) -> Result<Self> {
        unsafe {
            Ok(Self {
                lifecycle: CurrentCharacterLifecycle::bind(bytes.clone(), base, epoch, None)?,
                events: CurrentCharacterEvents::bind(bytes.clone(), base, epoch, None)?,
                tasks: CurrentCharacterTasks::bind(bytes.clone(), base, epoch)?,
                links: CurrentControlLinks::bind(bytes, base, epoch, None)?,
                limit: 65536,
            })
        }
    }
}
pub struct NativeDriver<'a> {
    pub access: &'a NativeAccess,
    pub record: &'a mut NativeRecord,
}
impl Driver for NativeDriver<'_> {
    fn perform(&mut self, step: Step) -> Result<()> {
        let access = self.access;
        let record = &mut self.record;
        let actor = record.request.actor;
        // SAFETY: The resident FrameEnd host owes all transitive SDK obligations and complete
        // quiescence. Neither these receipts nor the copied reader subset proves them.
        unsafe {
            match step {
                Step::Prepare => {
                    record.owner_query = Some(access.lifecycle.prepare(actor, access.limit)?);
                    record
                        .owner_query
                        .as_ref()
                        .expect("published query")
                        .prepared
                        .as_ref()
                        .map(|_| ())
                        .map_err(Clone::clone)
                }
                Step::Selection => {
                    let source = access.links.observe_primary_target()?.ok_or(
                        Error::NativeOperationRejected("primary selection unavailable"),
                    )?;
                    if source.player != record.request.primary {
                        return Err(Error::NativeOperationRejected("retirement primary changed"));
                    }
                    record.selection =
                        Some(access.links.clear_primary_target(source, actor.character)?);
                    record
                        .selection
                        .as_ref()
                        .expect("published selection")
                        .postflight
                        .as_ref()
                        .map(|_| ())
                        .map_err(Clone::clone)
                }
                Step::Aliases => {
                    let source = access.events.observe(actor, access.limit)?.ok_or(
                        Error::NativeOperationRejected("retirement mappings unavailable"),
                    )?;
                    record.aliases =
                        Some(access.events.release_entity_aliases(source, access.limit)?);
                    record
                        .aliases
                        .as_ref()
                        .expect("published aliases")
                        .after
                        .as_ref()
                        .map(|_| ())
                        .map_err(Clone::clone)
                }
                Step::ClearTask(ordinal) => {
                    let receipt = access.tasks.clear(actor.character, ordinal as u8)?;
                    let result = match &receipt {
                        CharacterTaskClear::AlreadyClear => Ok(()),
                        CharacterTaskClear::Requested { slot_clear, .. } => slot_clear.clone(),
                    };
                    record.tasks.push(receipt);
                    result
                }
                Step::Detach | Step::Destroy | Step::Free => {
                    let ticket = record
                        .owner_query
                        .as_mut()
                        .ok_or(Error::InvalidProfile("missing owner query"))?
                        .prepared
                        .as_mut()
                        .map_err(|e| e.clone())?;
                    match step {
                        Step::Detach => {
                            record.detach = Some(access.lifecycle.detach(ticket, access.limit)?);
                            record
                                .detach
                                .as_ref()
                                .expect("published detach")
                                .postflight
                                .clone()
                        }
                        Step::Destroy => {
                            record.destruction = Some(
                                access
                                    .lifecycle
                                    .destroy_without_free(ticket, access.limit)?,
                            );
                            let receipt =
                                record.destruction.as_ref().expect("published destructor");
                            receipt.postflight.clone()?;
                            if let Some(pool) = &receipt.npc_pool_after {
                                pool.as_ref().map_err(Clone::clone)?;
                            }
                            Ok(())
                        }
                        Step::Free => {
                            let result = access.lifecycle.release_storage(ticket);
                            record.free = Some(result.clone());
                            result
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
    fn observe(&mut self) -> Result<(usize, bool)> {
        let ticket = self
            .record
            .owner_query
            .as_ref()
            .ok_or(Error::InvalidProfile("missing retirement ticket"))?
            .prepared
            .as_ref()
            .map_err(Clone::clone)?;
        let observed = self
            .access
            .lifecycle
            .observe_retirement(ticket, self.access.limit)?;
        let refs = usize::from(observed.references.primary_slot_matches)
            + observed
                .references
                .collections
                .iter()
                .map(|v| v.matches)
                .sum::<usize>();
        let clear = observed
            .tasks
            .tasks
            .iter()
            .all(|v| v.registration.is_none());
        self.record.last_observation = Some(observed);
        Ok((refs, clear))
    }
}
pub struct Pending {
    pub record: NativeRecord,
    pub protocol: Protocol,
}
impl Pending {
    pub fn new(request: Request) -> Self {
        Self {
            record: NativeRecord::new(request),
            protocol: Protocol::default(),
        }
    }
    pub fn advance(&mut self, access: &NativeAccess) -> Result<()> {
        self.protocol.advance(&mut NativeDriver {
            access,
            record: &mut self.record,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Mock {
        calls: Vec<Step>,
        fail: Option<usize>,
        observations: Vec<(usize, bool)>,
    }
    impl Driver for Mock {
        fn perform(&mut self, step: Step) -> Result<()> {
            self.calls.push(step);
            if self.fail == Some(self.calls.len() - 1) {
                Err(Error::NativeOperationRejected("postflight"))
            } else {
                Ok(())
            }
        }
        fn observe(&mut self) -> Result<(usize, bool)> {
            Ok(self.observations.remove(0))
        }
    }
    #[test]
    fn stale_reader_blocks_free_until_two_quiet_frame_ends() {
        let mut gate = DrainGate::default();
        for (refs, clear) in [
            (Some(1), true),
            (Some(0), false),
            (Some(0), true),
            (None, true),
            (Some(0), true),
        ] {
            assert!(!gate.observe(refs, clear))
        }
        assert!(gate.observe(Some(0), true));
    }
    #[test]
    fn cancellation_detach_destruction_and_original_allocator_are_ordered() {
        let mut driver = Mock {
            calls: Vec::new(),
            fail: None,
            observations: vec![(0, true); 2],
        };
        let mut protocol = Protocol::default();
        protocol.advance(&mut driver).unwrap();
        assert_eq!(driver.calls.last(), Some(&Step::Detach));
        assert!(!protocol.complete);
        protocol.advance(&mut driver).unwrap();
        assert_eq!(&driver.calls[10..], &[Step::Destroy, Step::Free]);
        assert!(protocol.complete);
        protocol.advance(&mut driver).unwrap();
        assert_eq!(driver.calls.len(), 12);
    }
    #[test]
    fn every_partial_attempt_blocks_retry_and_later_free() {
        for fail in 0..12 {
            let mut driver = Mock {
                calls: Vec::new(),
                fail: Some(fail),
                observations: vec![(0, true); 2],
            };
            let mut protocol = Protocol::default();
            if protocol.advance(&mut driver).is_ok() {
                assert!(protocol.advance(&mut driver).is_err())
            }
            let count = driver.calls.len();
            assert_eq!(count, fail + 1);
            assert!(protocol.advance(&mut driver).is_err());
            assert_eq!(driver.calls.len(), count);
            assert!(!protocol.complete);
        }
    }
    #[test]
    fn accepts_observed_frame_end_id_but_rejects_other_groups() {
        assert!(accepts_frame_end(2415919272));
        assert!(accepts_frame_end(168));
        for group in [0, 167, 169, 0x900000a7, 0x900000a9, 0x800000a8, u32::MAX] {
            assert!(!accepts_frame_end(group))
        }
    }
}
