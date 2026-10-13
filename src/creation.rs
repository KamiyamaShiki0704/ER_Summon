//! Serialized ordinary creation. Unique native tickets stay on their callback thread.
use shikifsrs_elden_ring::{
    character_sets::CharacterRegistrationObservation,
    creation_input_preparation::{CreationInputOperation, OrdinaryCreationInput},
    current_debug_creator::{
        CurrentDebugCreator, DebugCreationResultClear, NativeCreationSchedule,
        NativeScheduledDispatchCall, PreparedDebugCreation, ScheduledCreationPoll,
        ScheduledCreationResult,
    },
};
use shikifsrs_runtime::{Error, Result};
use std::{collections::VecDeque, time::Instant};

pub struct Request {
    pub config_index: usize,
    pub requested_at: Instant,
    pub expires_at: Option<Instant>,
    pub input: OrdinaryCreationInput,
}
pub struct Attempt {
    pub operations: Vec<CreationInputOperation>,
    pub prepared: Option<PreparedDebugCreation>,
    pub schedule: Option<NativeCreationSchedule>,
    pub dispatch: Option<NativeScheduledDispatchCall>,
    pub result: Option<Box<ScheduledCreationResult>>,
    pub clear: Option<DebugCreationResultClear>,
    pub failure: Option<Error>,
}
pub enum Begin<T> {
    Deferred,
    Started(T),
}
pub trait Driver {
    type Ticket;
    fn begin(&mut self, request: &Request) -> Result<Begin<Self::Ticket>>;
    fn progress(
        &mut self,
        ticket: &mut Self::Ticket,
    ) -> Result<Option<CharacterRegistrationObservation>>;
}
pub struct NativeDriver {
    pub creator: CurrentDebugCreator,
    pub set_limit: usize,
}
impl Driver for NativeDriver {
    type Ticket = Attempt;
    fn begin(&mut self, request: &Request) -> Result<Begin<Attempt>> {
        let Some(source) = self.creator.observe()? else {
            return Ok(Begin::Deferred);
        };
        // A foreign outstanding result/schedule is never overwritten or cleared.
        if source.last_created_address != 0
            || source.schedule.current_step != 0
            || source.schedule.next_step != 0
            || source.schedule.advance_byte != 0
        {
            return Ok(Begin::Deferred);
        }
        // SAFETY: The retained consumer callback supplies the exclusive current ordinary
        // creator/world/PARAM/resource/module graph. Native execution remains experimental.
        let report = unsafe {
            self.creator
                .prepare_single_input(source, request.input.clone(), self.set_limit)
        }?;
        let (prepared, failure) = match report.completion {
            Ok(ticket) => (Some(ticket), None),
            Err(error) => (None, Some(error)),
        };
        let mut attempt = Attempt {
            operations: report.operations,
            prepared,
            schedule: None,
            dispatch: None,
            result: None,
            clear: None,
            failure,
        };
        if attempt.failure.is_none() {
            let prepared = attempt
                .prepared
                .as_mut()
                .expect("successful preparation owns ticket");
            // SAFETY: Same exclusive request ownership/full graph; one scheduling attempt.
            match unsafe { self.creator.schedule_prepared_single(prepared) } {
                Ok(receipt) => {
                    if let Err(error) = receipt.selection.postflight.as_ref() {
                        attempt.failure = Some(error.clone());
                    }
                    match &receipt.advance {
                        Ok(receipt) => {
                            if let Err(error) = &receipt.postflight {
                                attempt.failure = Some(error.clone());
                            }
                        }
                        Err(error) => attempt.failure = Some(error.clone()),
                    }
                    attempt.schedule = Some(receipt);
                }
                Err(error) => attempt.failure = Some(error),
            }
        }
        if attempt.failure.is_none() {
            // SAFETY: Same owned scheduled request; dispatcher checks zero native dependency
            // queue and complete descriptor/code/header before its one actual attempt.
            match unsafe {
                self.creator
                    .dispatch_scheduled_single(attempt.prepared.as_mut().expect("prepared ticket"))
            } {
                Ok(receipt) => {
                    if let Err(error) = &receipt.postflight {
                        attempt.failure = Some(error.clone());
                    }
                    attempt.dispatch = Some(receipt);
                }
                Err(error) => attempt.failure = Some(error),
            }
        }
        Ok(Begin::Started(attempt))
    }
    fn progress(
        &mut self,
        attempt: &mut Attempt,
    ) -> Result<Option<CharacterRegistrationObservation>> {
        if let Some(error) = &attempt.failure {
            return Err(error.clone());
        }
        let prepared = attempt
            .prepared
            .as_mut()
            .ok_or(Error::InvalidProfile("creation attempt has no ticket"))?;
        if attempt.result.is_none() {
            match self.creator.poll_scheduled_result(prepared) {
                Ok(ScheduledCreationPoll::Pending(_)) => return Ok(None),
                Ok(ScheduledCreationPoll::Registered(result)) => attempt.result = Some(result),
                Err(error) => {
                    attempt.failure = Some(error.clone());
                    return Err(error);
                }
            }
        }
        let result = attempt.result.as_ref().expect("published admission");
        let registration = result
            .observation()
            .registration
            .ok_or(Error::InvalidProfile("admission lacks registration"))?;
        // SAFETY: Exclusively owned admitted result and pinned registration. The SDK
        // clears only its attributed exact result, and never repeats failed stores.
        match unsafe { self.creator.clear_scheduled_result(prepared, result) } {
            Ok(receipt) => {
                let completion = receipt
                    .postflight
                    .as_ref()
                    .map(|_| ())
                    .map_err(Clone::clone);
                attempt.clear = Some(receipt);
                if let Err(error) = completion {
                    attempt.failure = Some(error.clone());
                    return Err(error);
                }
            }
            Err(error) => {
                attempt.failure = Some(error.clone());
                return Err(error);
            }
        }
        Ok(Some(registration))
    }
}
pub struct Pending<T> {
    pub request: Request,
    pub ticket: T,
    pub timed_out: bool,
}
pub struct Completed<T> {
    pub pending: Pending<T>,
    pub registration: CharacterRegistrationObservation,
}
pub struct Queue<T> {
    requests: VecDeque<Request>,
    active: Option<Pending<T>>,
    /// Retain native tickets after known epoch changes; no stale graph access or Drop cleanup.
    quarantined: Vec<Pending<T>>,
    blocked: Option<Error>,
    limit: usize,
}
impl<T> Queue<T> {
    pub fn new(limit: usize) -> Result<Self> {
        if !(1..=65536).contains(&limit) {
            return Err(Error::InvalidProfile("invalid creation queue limit"));
        }
        Ok(Self {
            requests: VecDeque::new(),
            active: None,
            quarantined: Vec::new(),
            blocked: None,
            limit,
        })
    }
    pub fn pending_count(&self) -> usize {
        self.requests.len() + usize::from(self.active.is_some())
    }
    pub fn has_configuration(&self, index: usize) -> bool {
        self.requests.iter().any(|r| r.config_index == index)
            || self
                .active
                .as_ref()
                .is_some_and(|r| r.request.config_index == index)
    }
    pub fn active_ticket(&self) -> Option<&T> {
        self.active.as_ref().map(|pending| &pending.ticket)
    }
    pub fn enqueue(&mut self, request: Request) -> Result<()> {
        if self.pending_count() >= self.limit {
            return Err(Error::NativeOperationRejected("creation queue full"));
        }
        self.requests.push_back(request);
        Ok(())
    }
    /// Timeouts mark already attempted requests without releasing native tickets or
    /// treating a zero result as failure. Expired unattempted requests may be dropped.
    pub fn poll(
        &mut self,
        driver: &mut impl Driver<Ticket = T>,
        now: Instant,
        unbound_timeout: std::time::Duration,
    ) -> Result<Option<Completed<T>>> {
        if let Some(error) = &self.blocked {
            return Err(error.clone());
        }
        while self.active.is_none() {
            let Some(request) = self.requests.front() else {
                return Ok(None);
            };
            if request.expires_at.is_some_and(|deadline| now >= deadline)
                || now.saturating_duration_since(request.requested_at) >= unbound_timeout
            {
                self.requests.pop_front();
                continue;
            }
            match driver.begin(request)? {
                Begin::Deferred => return Ok(None),
                Begin::Started(ticket) => {
                    self.active = Some(Pending {
                        request: self.requests.pop_front().expect("front request retained"),
                        ticket,
                        timed_out: false,
                    })
                }
            }
        }
        let active = self.active.as_mut().expect("one published native attempt");
        active.timed_out |=
            now.saturating_duration_since(active.request.requested_at) >= unbound_timeout;
        match driver.progress(&mut active.ticket) {
            Ok(Some(registration)) => Ok(Some(Completed {
                pending: self.active.take().expect("completed attempt"),
                registration,
            })),
            Ok(None) => Ok(None),
            Err(error) => {
                self.blocked = Some(error.clone());
                Err(error)
            }
        }
    }
    pub fn quarantine_epoch(&mut self) {
        if let Some(active) = self.active.take() {
            self.quarantined.push(active);
        }
        self.requests.clear();
        self.blocked = None;
    }
    pub fn blocked(&self) -> Option<&Error> {
        self.blocked.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shikifsrs_elden_ring::{
        character_sets::CharacterIdentity, debug_creation_fields::OrdinaryCreationRoute,
    };
    use std::time::Duration;
    struct Mock {
        started: Vec<usize>,
        progress: usize,
        ready: bool,
        fail: bool,
        defer: bool,
    }
    impl Driver for Mock {
        type Ticket = usize;
        fn begin(&mut self, request: &Request) -> Result<Begin<usize>> {
            if self.defer {
                return Ok(Begin::Deferred);
            }
            self.started.push(request.config_index);
            Ok(Begin::Started(request.config_index))
        }
        fn progress(
            &mut self,
            ticket: &mut usize,
        ) -> Result<Option<CharacterRegistrationObservation>> {
            self.progress += 1;
            if self.fail {
                return Err(Error::InvalidProfile("uncertain native postflight"));
            }
            Ok(self.ready.then_some(CharacterRegistrationObservation {
                character: CharacterIdentity {
                    address: 0x1000,
                    world_address: 0x2000,
                    native_identifier: *ticket as u64,
                    epoch: 1,
                },
                set_address: 0x3000,
                entry_address: 0x4000,
                ordinal: *ticket as u32,
            }))
        }
    }
    fn request(index: usize, now: Instant) -> Request {
        Request {
            config_index: index,
            requested_at: now,
            expires_at: None,
            input: OrdinaryCreationInput {
                name_units: vec![99],
                position_bits: [0; 4],
                rotation_bits: [0; 4],
                npc_param_id: 1,
                npc_think_param_id: 2,
                event_entity_id: 3,
                talk_id: -1,
                chara_init_param_id: 4,
                animation_id: -1,
                route: OrdinaryCreationRoute::Enemy,
            },
        }
    }
    fn mock() -> Mock {
        Mock {
            started: vec![],
            progress: 0,
            ready: false,
            fail: false,
            defer: false,
        }
    }
    #[test]
    fn simultaneous_configurations_are_serialized_and_pending_timeout_never_overwrites() {
        let now = Instant::now();
        let mut queue = Queue::new(2).unwrap();
        let mut mock = mock();
        queue.enqueue(request(7, now)).unwrap();
        queue.enqueue(request(8, now)).unwrap();
        assert!(queue.enqueue(request(9, now)).is_err());
        assert!(
            queue
                .poll(&mut mock, now, Duration::from_secs(1))
                .unwrap()
                .is_none()
        );
        assert!(
            queue
                .poll(
                    &mut mock,
                    now + Duration::from_secs(2),
                    Duration::from_secs(1)
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(mock.started, vec![7]);
        assert!(queue.active.as_ref().unwrap().timed_out);
        mock.ready = true;
        let completion = queue
            .poll(
                &mut mock,
                now + Duration::from_secs(3),
                Duration::from_secs(1),
            )
            .unwrap()
            .unwrap();
        assert_eq!(completion.pending.request.config_index, 7);
        assert!(completion.pending.timed_out);
        assert_eq!(completion.registration.character.native_identifier, 7);
        assert!(
            queue
                .poll(
                    &mut mock,
                    now + Duration::from_secs(3),
                    Duration::from_secs(1)
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(mock.started, vec![7]); // queued-but-unattempted expired request is discarded
    }
    #[test]
    fn partial_failure_keeps_unique_ticket_and_blocks_later_requests_until_epoch_quarantine() {
        let now = Instant::now();
        let mut queue = Queue::new(2).unwrap();
        let mut mock = mock();
        mock.fail = true;
        queue.enqueue(request(0, now)).unwrap();
        queue.enqueue(request(1, now)).unwrap();
        assert!(queue.poll(&mut mock, now, Duration::from_secs(1)).is_err());
        assert!(queue.blocked().is_some());
        assert!(queue.poll(&mut mock, now, Duration::from_secs(1)).is_err());
        assert_eq!(mock.progress, 1);
        assert_eq!(mock.started, vec![0]);
        queue.quarantine_epoch();
        assert_eq!(queue.quarantined.len(), 1);
        assert_eq!(queue.quarantined[0].ticket, 0);
        mock.fail = false;
        mock.ready = true;
        queue.enqueue(request(2, now)).unwrap();
        assert!(
            queue
                .poll(&mut mock, now, Duration::from_secs(1))
                .unwrap()
                .is_some()
        );
        assert_eq!(mock.started, vec![0, 2]);
    }
    #[test]
    fn deferred_creator_keeps_order_and_never_starts_two_native_attempts() {
        let now = Instant::now();
        let mut queue = Queue::new(2).unwrap();
        let mut mock = mock();
        mock.defer = true;
        queue.enqueue(request(2, now)).unwrap();
        queue.enqueue(request(3, now)).unwrap();
        assert!(
            queue
                .poll(&mut mock, now, Duration::from_secs(1))
                .unwrap()
                .is_none()
        );
        assert!(mock.started.is_empty());
        mock.defer = false;
        mock.ready = true;
        assert_eq!(
            queue
                .poll(&mut mock, now, Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .pending
                .request
                .config_index,
            2
        );
        assert_eq!(
            queue
                .poll(&mut mock, now, Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .pending
                .request
                .config_index,
            3
        );
        assert_eq!(mock.started, vec![2, 3]);
    }
}
