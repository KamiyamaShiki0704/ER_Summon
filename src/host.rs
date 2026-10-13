//! Resident two-phase host. All native execution is experimental: callback identity
//! and copied marks do not establish complete graph leases, ABA safety or quiescence.
use crate::{
    AttackSummonConfig, AttackSummonSettings, HavokPosition, RemoveMode, character_model_name,
    creation, log_line, normalize_horizontal,
    presentation::{self, NativePresentation, Phase, Record, Transaction},
    retirement, settings, summon_position, uses_player_creation, yaw_from_orientation,
};
use shikifsrs_elden_ring::{
    TaskCallback, TaskInvocation, TaskPhase, Ww171File,
    character_sets::{CharacterIdentity, CharacterRegistrationObservation},
    character_spatial::NativePoseInput,
    creation_input_preparation::OrdinaryCreationInput,
    current_character_events::{
        CurrentCharacterEvents, EventEntityInsertion, EventEntityRelease, EventGroupRegistration,
    },
    current_control_links::CurrentControlLinks,
    current_debug_creator::CurrentDebugCreator,
    current_tasks::CurrentTaskEngine,
    debug_creation_fields::OrdinaryCreationRoute,
    effects::EffectId,
    summon_triggers::{SummonTriggerEffectConsumer, SummonTriggerEffectsReceipt},
    task_resident::ResidentCallbackGroupSlot,
};
use shikifsrs_runtime::{Error, Result, memory::CurrentProcess};
use std::os::windows::ffi::OsStringExt;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
static TASKS: ResidentCallbackGroupSlot<2> = ResidentCallbackGroupSlot::new();
static EPOCH: AtomicU64 = AtomicU64::new(1);
const LIMIT: usize = 65536;
#[derive(Default)]
struct Mailbox {
    scope: Option<CharacterIdentity>,
    requests: Vec<retirement::Request>,
    completed: Vec<CharacterRegistrationObservation>,
    outstanding: usize,
}
static MAILBOX: LazyLock<Mutex<Mailbox>> = LazyLock::new(|| Mutex::new(Mailbox::default()));
thread_local! {
 static CREATION:RefCell<CreationSlot>=RefCell::new(CreationSlot::default());
 static RETIREMENT:RefCell<RetirementSlot>=RefCell::new(RetirementSlot::default());
}
#[derive(Default)]
struct CreationSlot {
    current: Option<Core>,
    quarantined: Vec<Core>,
    error: Option<Error>,
}
#[derive(Default)]
struct RetirementSlot {
    current: Option<Frame>,
    quarantined: Vec<Frame>,
    error: Option<Error>,
}
/// The caller must advance this on known native scope reuse. Visible identity comparison
/// cannot detect same-address ABA; no storage is destroyed during this notification.
#[unsafe(no_mangle)]
pub extern "system" fn ShikiSummonNotifyEpoch(epoch: u64) {
    if epoch != 0 {
        let _ = EPOCH.fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
            (epoch > old).then_some(epoch)
        });
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Loading,
    Active,
    Hidden,
    Released,
    Retiring,
    Quarantined,
}
struct Instance {
    config_index: usize,
    actor: CharacterRegistrationObservation,
    creation: creation::Pending<creation::Attempt>,
    state: State,
    expires_at: Option<Instant>,
    destroy_at: Option<Instant>,
    pose: NativePoseInput,
    opacity: [f32; 4],
    present: Record,
    transaction: Option<Transaction>,
    insertion: Option<EventEntityInsertion>,
    groups: Option<EventGroupRegistration>,
    aliases: Option<EventEntityRelease>,
    failure: Option<Error>,
}
impl Instance {
    fn input(
        &self,
        phase: Phase,
        config: AttackSummonConfig,
        settings: &AttackSummonSettings,
    ) -> Result<presentation::Input> {
        Ok(presentation::Input {
            phase,
            pose: self.pose,
            animation: config.animation_id,
            team: config.generated_team_type(settings),
            marker: EffectId::new(config.marker_speffect(settings))?,
            vanish: EffectId::new(config.vanish_request_speffect(settings))?,
            disable_lock_on: config.disable_lock_on(settings),
            visible_opacity: self.opacity,
        })
    }
    fn transact(
        &mut self,
        native: &mut NativePresentation,
        phase: Phase,
        config: AttackSummonConfig,
        settings: &AttackSummonSettings,
    ) -> Result<()> {
        self.transaction = Some(Transaction::new(
            self.actor,
            &self.input(phase, config, settings)?,
        )?);
        let transaction = self.transaction.as_mut().expect("published transaction");
        transaction.advance(native)?;
        if transaction.phase != phase || !transaction.complete() {
            return Err(Error::InvalidProfile("incomplete presentation transaction"));
        }
        Ok(())
    }
}
#[derive(Default)]
struct Cooldowns {
    last_any: Option<Instant>,
    slots: Vec<Option<Instant>>,
}
impl Cooldowns {
    fn accept(&mut self, index: usize, now: Instant, settings: &AttackSummonSettings) -> bool {
        self.slots.resize(settings.summons.len(), None);
        if self.last_any.is_some_and(|last| {
            now.saturating_duration_since(last) < settings.any_trigger_cooldown()
        }) || self.slots[index].is_some_and(|last| {
            now.saturating_duration_since(last) < settings.same_trigger_cooldown()
        }) {
            return false;
        }
        self.last_any = Some(now);
        self.slots[index] = Some(now);
        true
    }
}
struct Core {
    epoch: u64,
    scope: Option<CharacterIdentity>,
    native: NativePresentation,
    links: CurrentControlLinks,
    events: CurrentCharacterEvents,
    creator: creation::NativeDriver,
    queue: creation::Queue<creation::Attempt>,
    triggers: Option<SummonTriggerEffectConsumer>,
    last_trigger: Option<SummonTriggerEffectsReceipt>,
    instances: Vec<Instance>,
    finished: VecDeque<Instance>,
    cooldowns: Cooldowns,
    failure: Option<Error>,
}
impl Core {
    unsafe fn bind(bytes: Arc<[u8]>, base: u64, epoch: u64) -> Result<Self> {
        // SAFETY: Resident actual callback host is responsible for each transitive graph/phase contract.
        unsafe {
            Ok(Self {
                epoch,
                scope: None,
                native: NativePresentation::bind(bytes.clone(), base, epoch, LIMIT)?,
                links: CurrentControlLinks::bind(bytes.clone(), base, epoch, None)?,
                events: CurrentCharacterEvents::bind(bytes.clone(), base, epoch, None)?,
                creator: creation::NativeDriver {
                    creator: CurrentDebugCreator::bind(bytes, base, epoch, None)?,
                    set_limit: LIMIT,
                },
                queue: creation::Queue::new(LIMIT)?,
                triggers: None,
                last_trigger: None,
                instances: Vec::new(),
                finished: VecDeque::new(),
                cooldowns: Cooldowns::default(),
                failure: None,
            })
        }
    }
    fn quarantine(&mut self) {
        self.queue.quarantine_epoch();
        for instance in &mut self.instances {
            instance.state = State::Quarantined;
        }
    }
    fn tick(&mut self, now: Instant) -> Result<()> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        let primary = self.links.observe_primary_identity()?;
        if self.scope.is_some() && primary != self.scope {
            self.quarantine();
            EPOCH.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut mailbox) = MAILBOX.lock() {
                mailbox.scope = None;
            }
            log_line("summon scope changed; native obligations retained without stale cleanup");
            return Ok(());
        }
        let Some(primary) = primary else {
            return Ok(());
        };
        if self.scope.is_none() {
            let effects = settings()
                .summons
                .iter()
                .map(|s| EffectId::new(s.trigger_speffect))
                .collect::<Result<Vec<_>>>()?;
            self.triggers = Some(SummonTriggerEffectConsumer::new(
                primary.world_address,
                self.epoch,
                &effects,
                LIMIT,
                4096,
            )?);
            self.scope = Some(primary);
        }
        let outstanding = {
            let mut mailbox = MAILBOX
                .lock()
                .map_err(|_| Error::NativeOperationRejected("retirement mailbox poisoned"))?;
            if mailbox.scope != Some(primary) {
                mailbox.requests.retain(|r| r.primary == primary);
                mailbox.outstanding = mailbox.requests.len();
            }
            mailbox.scope = Some(primary);
            for actor in mailbox.completed.drain(..) {
                if let Some(index) = self
                    .instances
                    .iter()
                    .position(|v| v.actor == actor && v.state == State::Retiring)
                {
                    self.finished.push_back(self.instances.swap_remove(index));
                    if self.finished.len() > 64 {
                        self.finished.pop_front();
                    }
                }
            }
            mailbox.outstanding
        };
        if let Some(completed) =
            self.queue
                .poll(&mut self.creator, now, settings().unbound_timeout())?
        {
            let actor = completed.registration;
            let pose = NativePoseInput {
                position_bits: completed.pending.request.input.position_bits,
                angle_bits: std::array::from_fn(|i| {
                    completed.pending.request.input.rotation_bits[i]
                }),
            };
            let expires_at = completed.pending.request.expires_at;
            self.instances.push(Instance {
                config_index: completed.pending.request.config_index,
                actor,
                creation: completed.pending,
                state: State::Loading,
                expires_at,
                destroy_at: None,
                pose,
                opacity: [1.0; 4],
                present: Record::new(actor),
                transaction: None,
                insertion: None,
                groups: None,
                aliases: None,
                failure: None,
            });
            // Published unique attributed allocation before requesting backread.
            let instance = self.instances.last_mut().expect("published allocation");
            if let Err(e) = unsafe { self.native.request_present(&mut instance.present) } {
                instance.failure = Some(e.clone());
                return Err(e);
            }
        }
        for instance in &mut self.instances {
            if instance.failure.is_some()
                || matches!(
                    instance.state,
                    State::Retiring | State::Quarantined | State::Released | State::Hidden
                )
            {
                continue;
            }
            let config = settings().summons[instance.config_index];
            let result = Self::update_instance(
                &mut self.native,
                &self.events,
                instance,
                primary,
                config,
                now,
                settings(),
            );
            if let Err(e) = result {
                instance.failure = Some(e.clone());
                return Err(e);
            }
        }
        // Preserve consumption-before-admission semantics, including matching aliases and cap rejection.
        let receipt = unsafe {
            self.triggers
                .as_mut()
                .expect("scoped consumer")
                .consume(&self.native.effects, TaskPhase::CHR_POST_PHYSICS)
        }?;
        let triggered = if receipt.completed {
            receipt.triggered.clone()
        } else {
            Vec::new()
        };
        let failed = receipt.failure.clone();
        self.last_trigger = Some(receipt);
        if let Some(error) = failed {
            return Err(error);
        }
        if outstanding > 0 || triggered.is_empty() {
            return Ok(());
        }
        let spatial =
            self.native
                .spatial
                .observe(primary)?
                .ok_or(Error::NativeOperationRejected(
                    "primary spatial unavailable",
                ))?;
        let pos = spatial.finite_position()?;
        let origin = HavokPosition(
            pos[0],
            pos[1],
            pos[2],
            f32::from_bits(spatial.position_bits[3]),
        );
        let yaw = yaw_from_orientation(spatial.primary_rotation_bits.map(f32::from_bits));
        let axes = match self.native.spatial.observe_perspective(0)? {
            Some(camera) => camera_axes(camera.matrix_bits),
            None => ((0.0, 0.0, 1.0), (1.0, 0.0, 0.0)),
        };
        for index in triggered {
            if self.active_count() >= settings().max_active_summons {
                break;
            }
            if !self.cooldowns.accept(index, now, settings()) {
                continue;
            }
            let config = settings().summons[index];
            let position = summon_position(origin, axes.0, axes.1, config, settings());
            let pose = NativePoseInput {
                position_bits: [
                    position.0.to_bits(),
                    position.1.to_bits(),
                    position.2.to_bits(),
                    origin.3.to_bits(),
                ],
                angle_bits: [0, yaw.to_bits(), 0],
            };
            if settings().reuses_units() {
                if self.queue.has_configuration(index) {
                    continue;
                }
                if let Some(instance) = self
                    .instances
                    .iter_mut()
                    .find(|v| v.config_index == index && v.state != State::Released)
                {
                    if instance.state != State::Hidden {
                        continue;
                    }
                    instance.pose = pose;
                    instance.expires_at = config.expires_at(settings(), now);
                    instance.creation.request.requested_at = now;
                    if let Err(e) =
                        instance.transact(&mut self.native, Phase::Reactivating, config, settings())
                    {
                        instance.failure = Some(e.clone());
                        return Err(e);
                    }
                    instance.state = State::Active;
                    continue;
                }
            }
            if self.instances.len() + self.queue.pending_count() >= LIMIT {
                return Err(Error::NativeOperationRejected(
                    "resident summon bound reached",
                ));
            }
            let route = if uses_player_creation(config.chr_id, config.is_player) {
                OrdinaryCreationRoute::Player
            } else {
                OrdinaryCreationRoute::Enemy
            };
            self.queue.enqueue(creation::Request {
                config_index: index,
                requested_at: now,
                expires_at: config.expires_at(settings(), now),
                input: OrdinaryCreationInput {
                    name_units: character_model_name(config.chr_id).encode_utf16().collect(),
                    position_bits: pose.position_bits,
                    rotation_bits: [0, yaw.to_bits(), 0, 0],
                    npc_param_id: config.npc_param_id,
                    npc_think_param_id: config.npc_think_param_id,
                    event_entity_id: config.event_entity_id as u32,
                    talk_id: config.talk_id,
                    chara_init_param_id: config.chara_init_param_id,
                    animation_id: config.animation_id,
                    route,
                },
            })?;
        }
        Ok(())
    }
    fn active_count(&self) -> usize {
        self.queue.pending_count()
            + self
                .instances
                .iter()
                .filter(|v| matches!(v.state, State::Loading | State::Active))
                .count()
    }
    fn update_instance(
        native: &mut NativePresentation,
        events: &CurrentCharacterEvents,
        instance: &mut Instance,
        primary: CharacterIdentity,
        config: AttackSummonConfig,
        now: Instant,
        settings: &AttackSummonSettings,
    ) -> Result<()> {
        if instance.state == State::Loading {
            let Some(resource) = native.resource_marks(instance.actor)? else {
                return Ok(());
            };
            // Marks are merely the waiting predicate; the host separately owes the complete ready graph.
            if !resource.native_presentation_marks() {
                return Ok(());
            }
            instance.opacity =
                presentation::manual_opacity(native.observe_opacity(instance.actor)?);
            if settings.register_event_entity_id && config.event_entity_id > 0 {
                let observed = events.observe(instance.actor, LIMIT)?.ok_or(
                    Error::NativeOperationRejected("new summon event maps absent"),
                )?;
                instance.insertion = Some(unsafe { events.insert_stored_id(observed, LIMIT) }?);
                if let EventEntityInsertion::Submitted { after, .. } =
                    instance.insertion.as_ref().expect("published insertion")
                {
                    after.as_ref().map_err(Clone::clone)?;
                }
                let observed = events
                    .observe(instance.actor, LIMIT)?
                    .ok_or(Error::NativeOperationRejected("summon event maps vanished"))?;
                if observed.groups.is_empty() {
                    // First external registration is an experimental host assumption; empty trees alone
                    // do not establish it. Retain the actual one-time external manager callback receipt.
                    instance.groups = Some(unsafe { events.register_groups(observed, LIMIT) }?);
                    instance
                        .groups
                        .as_ref()
                        .expect("published groups")
                        .after
                        .as_ref()
                        .map_err(Clone::clone)?;
                }
            }
            instance.transact(native, Phase::Binding, config, settings)?;
            instance.state = State::Active;
            log_line(format_args!(
                "summon bound: config={} native_id={:#x} ordinary_route_verified=true runtime=experimental",
                instance.config_index, instance.actor.character.native_identifier
            ));
        }
        if retirement_due(instance.destroy_at, now) {
            return Self::enqueue_retirement(instance, primary, now);
        }
        if instance.destroy_at.is_some() {
            return Ok(());
        }
        let vitals = native
            .vitals
            .observe(instance.actor.character)?
            .ok_or(Error::NativeOperationRejected("summon vitals absent"))?;
        let flags = native
            .flags
            .observe(instance.actor.character)?
            .ok_or(Error::NativeOperationRejected("summon flags absent"))?;
        let disappear = instance.creation.timed_out
            || instance.expires_at.is_some_and(|at| now >= at)
            || vitals.hp.current <= 0
            || flags.native_death_marked()
            || native.has_effect(
                instance.actor.character,
                EffectId::new(config.vanish_request_speffect(settings))?,
            )?;
        if !disappear {
            return instance.transact(native, Phase::Updating, config, settings);
        }
        if settings.remove_mode == RemoveMode::Destroy {
            instance.transact(native, Phase::Releasing, config, settings)?;
            instance.destroy_at = Some(now.checked_add(settings.destroy_delay()).unwrap_or(now));
            if retirement_due(instance.destroy_at, now) {
                Self::enqueue_retirement(instance, primary, now)?;
            }
        } else if settings.reuses_units() {
            instance.opacity =
                presentation::manual_opacity(native.observe_opacity(instance.actor)?);
            instance.transact(native, Phase::Parking, config, settings)?;
            instance.state = State::Hidden;
        } else {
            let observed = events
                .observe(instance.actor, LIMIT)?
                .ok_or(Error::NativeOperationRejected("hidden aliases absent"))?;
            instance.aliases = Some(unsafe { events.release_entity_aliases(observed, LIMIT) }?);
            instance
                .aliases
                .as_ref()
                .expect("published alias release")
                .after
                .as_ref()
                .map_err(Clone::clone)?;
            instance.transact(native, Phase::Releasing, config, settings)?;
            instance.state = State::Released;
        }
        Ok(())
    }
    fn enqueue_retirement(
        instance: &mut Instance,
        primary: CharacterIdentity,
        now: Instant,
    ) -> Result<()> {
        let mut mailbox = MAILBOX
            .lock()
            .map_err(|_| Error::NativeOperationRejected("retirement mailbox poisoned"))?;
        if mailbox.scope != Some(primary) || mailbox.requests.len() >= LIMIT {
            return Err(Error::NativeOperationRejected(
                "retirement mailbox scope/bound",
            ));
        }
        mailbox.requests.push(retirement::Request {
            actor: instance.actor,
            primary,
            not_before: now,
        });
        mailbox.outstanding += 1;
        instance.state = State::Retiring;
        Ok(())
    }
}
pub fn retirement_due(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|at| now >= at)
}
/// Consumer camera convention, experimental until runtime acceptance. Matrix storage is
/// independently verified; row-to-gameplay basis selection is not a claimed SDK native fact.
fn camera_axes(bits: [[u32; 4]; 4]) -> ((f32, f32, f32), (f32, f32, f32)) {
    let m = bits.map(|row| row.map(f32::from_bits));
    (
        normalize_horizontal((m[2][0], m[2][1], m[2][2]), (0.0, 0.0, 1.0)),
        normalize_horizontal((m[0][0], m[0][1], m[0][2]), (1.0, 0.0, 0.0)),
    )
}
struct Frame {
    epoch: u64,
    access: retirement::NativeAccess,
    pending: Vec<retirement::Pending>,
    finished: VecDeque<retirement::Pending>,
}
impl Frame {
    unsafe fn bind(bytes: Arc<[u8]>, base: u64, epoch: u64) -> Result<Self> {
        Ok(Self {
            epoch,
            access: unsafe { retirement::NativeAccess::bind(bytes, base, epoch) }?,
            pending: Vec::new(),
            finished: VecDeque::new(),
        })
    }
    fn quarantine(&mut self) {
        for item in &mut self.pending {
            item.record.quarantine();
        }
    }
    fn tick(&mut self, now: Instant) -> Result<()> {
        let primary = self.access.links.observe_primary_identity()?;
        let requests = {
            let mut mailbox = MAILBOX
                .lock()
                .map_err(|_| Error::NativeOperationRejected("retirement mailbox poisoned"))?;
            if primary.is_none() || primary != mailbox.scope {
                self.quarantine();
                return Ok(());
            }
            std::mem::take(&mut mailbox.requests)
        };
        for request in requests {
            self.pending.push(retirement::Pending::new(request));
        }
        let mut index = 0;
        while index < self.pending.len() {
            let item = &mut self.pending[index];
            if primary != Some(item.record.request.primary) {
                item.record.quarantine();
                index += 1;
                continue;
            }
            if now < item.record.request.not_before || item.protocol.failure.is_some() {
                index += 1;
                continue;
            }
            if let Err(error) = item.advance(&self.access) {
                log_line(format_args!(
                    "summon retirement blocked: native_id={:#x} error={error}; actual ticket retained",
                    item.record.request.actor.character.native_identifier
                ));
                index += 1;
                continue;
            }
            if item.protocol.complete {
                let actor = item.record.request.actor;
                let mut mailbox = MAILBOX
                    .lock()
                    .map_err(|_| Error::NativeOperationRejected("retirement mailbox poisoned"))?;
                mailbox.completed.push(actor);
                mailbox.outstanding = mailbox.outstanding.saturating_sub(1);
                self.finished.push_back(self.pending.swap_remove(index));
                if self.finished.len() > 64 {
                    self.finished.pop_front();
                }
                log_line(format_args!(
                    "summon removed: native_id={:#x} detach=confirmed destructor_flags=0 original_allocator_free=returned runtime_acceptance=pending",
                    actor.character.native_identifier
                ));
            } else {
                index += 1;
            }
        }
        Ok(())
    }
}
fn callback_creation(bytes: Arc<[u8]>, base: u64, data: TaskInvocation) {
    if !matches!(data.group_id, 113 | 0x90000071) {
        return;
    }
    CREATION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.error.is_some() {
            return;
        }
        let epoch = EPOCH.load(Ordering::Acquire);
        if slot.current.as_ref().is_some_and(|v| v.epoch != epoch) {
            let mut old = slot.current.take().unwrap();
            old.quarantine();
            slot.quarantined.push(old);
        }
        if slot.current.is_none() {
            if slot.quarantined.len() >= 64 {
                slot.error = Some(Error::NativeOperationRejected(
                    "quarantine generation bound",
                ));
                return;
            }
            match unsafe { Core::bind(bytes, base, epoch) } {
                Ok(core) => slot.current = Some(core),
                Err(e) => {
                    log_line(format_args!("summon bind blocked: {e}"));
                    slot.error = Some(e);
                    return;
                }
            }
        }
        let core = slot.current.as_mut().unwrap();
        if core.failure.is_some() {
            return;
        }
        if let Err(e) = core.tick(Instant::now()) {
            if let Some(attempt) = core.queue.active_ticket() {
                log_line(format_args!(
                    "creation partial input_operations={} queue_error={:?}",
                    attempt.operations.len(),
                    core.queue.blocked()
                ));
            }
            for instance in &core.instances {
                if instance.failure.is_some() {
                    log_line(format_args!(
                        "actor={:#x} presentation={} partial={}",
                        instance.actor.character.native_identifier,
                        instance.present.summary(),
                        instance
                            .transaction
                            .as_ref()
                            .map(|t| t.record.summary())
                            .unwrap_or_default()
                    ));
                }
            }
            log_line(format_args!(
                "summon update blocked: {e}; partial operations retained"
            ));
            core.failure = Some(e);
        }
    });
}
fn callback_retirement(bytes: Arc<[u8]>, base: u64, data: TaskInvocation) {
    // Reject the invocation before any observation, queue ingestion or cleanup.
    if !retirement::accepts_frame_end(data.group_id) {
        return;
    }
    RETIREMENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.error.is_some() {
            return;
        }
        let epoch = EPOCH.load(Ordering::Acquire);
        if slot.current.as_ref().is_some_and(|v| v.epoch != epoch) {
            let mut old = slot.current.take().unwrap();
            old.quarantine();
            slot.quarantined.push(old);
        }
        if slot.current.is_none() {
            if slot.quarantined.len() >= 64 {
                slot.error = Some(Error::NativeOperationRejected(
                    "retirement quarantine bound",
                ));
                return;
            }
            match unsafe { Frame::bind(bytes, base, epoch) } {
                Ok(frame) => slot.current = Some(frame),
                Err(e) => {
                    log_line(format_args!("retirement bind blocked: {e}"));
                    slot.error = Some(e);
                    return;
                }
            }
        }
        if let Err(e) = slot.current.as_mut().unwrap().tick(Instant::now()) {
            log_line(format_args!("retirement phase blocked: {e}"));
            slot.error = Some(e);
        }
    });
}
pub fn install() -> Result<()> {
    if settings().summons.len() > LIMIT || settings().max_active_summons > LIMIT {
        return Err(Error::InvalidProfile("summon configuration bound"));
    }
    let mut path = [0u16; 32768];
    let count = unsafe { GetModuleFileNameW(None, &mut path) };
    if count == 0 || count as usize >= path.len() {
        return Err(Error::InvalidProfile("game module path unavailable"));
    }
    let bytes: Arc<[u8]> = fs::read(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &path[..count as usize],
    )))
    .map_err(|_| Error::InvalidProfile("cannot read game executable"))?
    .into();
    let file = Ww171File::verify(&bytes)?;
    let base = unsafe { GetModuleHandleW(None) }
        .map_err(|_| Error::InvalidProfile("game module unavailable"))?
        .0 as u64;
    let started = Instant::now();
    loop {
        let catalog = file.bind(&CurrentProcess, base)?;
        let mut ready = true;
        for phase in [TaskPhase::CHR_POST_PHYSICS, TaskPhase::FRAME_END] {
            let registered = match catalog
                .observe_phase(phase)?
                .and_then(|v| v.native_group_id())
            {
                Some(id) => catalog.registered_group(id, 4096)?.is_some(),
                None => false,
            };
            ready &= registered;
        }
        if ready {
            break;
        }
        if started.elapsed() >= Duration::from_secs(30) {
            return Err(Error::NativeOperationRejected(
                "summon task startup timeout",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let engine = unsafe { CurrentTaskEngine::bind(bytes.clone(), base) }?;
    let read = bytes.clone();
    let create = TaskCallback::new(move |data| callback_creation(read.clone(), base, data));
    let retire = TaskCallback::new(move |data| callback_retirement(bytes.clone(), base, data));
    // SAFETY: Static process-resident owner pins this DLL before native registration; both
    // partial registration outcomes survive errors. Runtime graph/phase contracts remain experimental.
    let installed = unsafe {
        TASKS.install(
            engine,
            [TaskPhase::CHR_POST_PHYSICS, TaskPhase::FRAME_END],
            [create, retire],
        )
    }?;
    let control = installed.registration?;
    if !control.registered_all() {
        return Err(Error::NativeOperationRejected(
            "partial summon task installation retained",
        ));
    }
    log_line(
        "summon initialized: ShikiFSRS WW1.17.1 two-phase resident owner; runtime capabilities experimental",
    );
    Ok(())
}
#[cfg(test)]
pub fn presentation_input_for_test(phase: Phase) -> presentation::Input {
    presentation::Input {
        phase,
        pose: NativePoseInput {
            position_bits: [0; 4],
            angle_bits: [0; 3],
        },
        animation: 20012,
        team: 47,
        marker: EffectId::new(1).unwrap(),
        vanish: EffectId::new(2).unwrap(),
        disable_lock_on: true,
        visible_opacity: [0.5, -1.0, 0.8, 0.9],
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cooldowns_preserve_global_per_slot_and_configuration_order() {
        let settings = AttackSummonSettings {
            any_trigger_cooldown_ms: 5,
            same_trigger_cooldown_ms: 10,
            ..Default::default()
        };
        let mut gate = Cooldowns::default();
        let now = Instant::now();
        assert!(gate.accept(0, now, &settings));
        assert!(!gate.accept(1, now + Duration::from_millis(4), &settings));
        assert!(gate.accept(1, now + Duration::from_millis(5), &settings));
        assert!(!gate.accept(0, now + Duration::from_millis(9), &settings));
        assert!(gate.accept(0, now + Duration::from_millis(10), &settings));
    }
    #[test]
    fn camera_mapping_preserves_horizontal_rotation_and_degenerate_fallback() {
        let mut matrix = [[0u32; 4]; 4];
        matrix[0][2] = (-1.0f32).to_bits();
        matrix[2][0] = 1.0f32.to_bits();
        assert_eq!(camera_axes(matrix), ((1.0, 0.0, 0.0), (0.0, 0.0, -1.0)));
        assert_eq!(camera_axes([[0; 4]; 4]), ((0.0, 0.0, 1.0), (1.0, 0.0, 0.0)));
    }
}
