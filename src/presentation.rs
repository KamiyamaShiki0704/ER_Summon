//! Actual SDK operations for resident presentation. Each partial operation stays recorded.
use shikifsrs_elden_ring::{
    character_animation::{AnimationOperation, NativeAnimationCall},
    character_flags::{CharacterFlagByte, CharacterFlagWrite},
    character_handles::EffectSourceClear,
    character_opacity::{CharacterOpacityObservation, ManualOpacityWrite},
    character_resources::{
        CharacterBackreadRequest, CharacterBackreadWrite, CharacterResourcesObservation,
    },
    character_sets::{
        CharacterIdentity, CharacterRegistrationObservation, WorldCollectionSelection,
    },
    character_spatial::{NativeCharacterPlacementCall, NativePoseInput},
    character_units::{CharacterUnitReadPlan, UnitComponent},
    character_vitals::{CharacterVital, CharacterVitalWrite},
    controller_policy::ControllerPolicyWrite,
    current_character_animation::CurrentCharacterAnimation,
    current_character_flags::CurrentCharacterFlags,
    current_character_reset::{CurrentCharacterReset, NativeCharacterResetCall},
    current_character_resources::CurrentCharacterResources,
    current_character_scalars::CurrentCharacterScalarAccess,
    current_character_spatial::CurrentCharacterSpatial,
    current_character_units::CurrentCharacterUnits,
    current_character_vitals::CurrentCharacterVitals,
    current_control_links::CurrentControlLinks,
    current_controller_policy::CurrentControllerPolicy,
    current_effects::CurrentEffectEngine,
    effects::{EffectApplication, EffectEngine, EffectId},
};
use shikifsrs_runtime::{Error, Result};
use std::sync::Arc;

pub enum Operation {
    Backread(CharacterBackreadWrite),
    Flags(CharacterFlagWrite),
    Policy(ControllerPolicyWrite),
    Opacity(ManualOpacityWrite),
    Vitals(CharacterVitalWrite),
    Animation(NativeAnimationCall),
    Placement(Box<NativeCharacterPlacementCall>),
    Reset(NativeCharacterResetCall),
    EffectSource(EffectSourceClear),
    ApplyEffect {
        target: CharacterIdentity,
        id: EffectId,
        returned: Option<Result<EffectApplication>>,
    },
    RemoveEffect {
        target: CharacterIdentity,
        id: EffectId,
        returned: Option<Result<bool>>,
    },
}
/// Store this record with its actor before any operation, including one whose native
/// return is uncertain. Records grant neither native ownership nor a destructor barrier.
pub struct Record {
    pub actor: CharacterRegistrationObservation,
    pub operations: Vec<Operation>,
    pub failure: Option<Error>,
}
impl Record {
    /// Diagnostic reads of the retained actual receipts, only used after a failure.
    pub fn summary(&self) -> String {
        self.operations
            .iter()
            .map(|operation| match operation {
                Operation::Backread(v) => format!("backread={v:?}"),
                Operation::Flags(v) => format!("flags={v:?}"),
                Operation::Policy(v) => format!("policy={v:?}"),
                Operation::Opacity(v) => format!("opacity={v:?}"),
                Operation::Vitals(v) => format!("vitals={v:?}"),
                Operation::Animation(v) => format!("animation={v:?}"),
                Operation::Placement(v) => format!("placement={v:?}"),
                Operation::Reset(v) => format!("reset={v:?}"),
                Operation::EffectSource(v) => format!("effect-source={v:?}"),
                Operation::ApplyEffect {
                    target,
                    id,
                    returned,
                } => format!("apply target={target:?} id={id:?} result={returned:?}"),
                Operation::RemoveEffect {
                    target,
                    id,
                    returned,
                } => format!("remove target={target:?} id={id:?} result={returned:?}"),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
    pub fn new(actor: CharacterRegistrationObservation) -> Self {
        Self {
            actor,
            operations: Vec::with_capacity(32),
            failure: None,
        }
    }
    fn check(&self) -> Result<()> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.operations.len() >= 32 {
            return Err(Error::InvalidProfile("presentation record full"));
        }
        Ok(())
    }
    fn commit(&mut self, operation: Operation, completion: Result<()>) -> Result<()> {
        self.operations.push(operation);
        if let Err(error) = &completion {
            self.failure = Some(error.clone());
        }
        completion
    }
}
pub struct NativePresentation {
    units: CurrentCharacterUnits,
    pub resources: CurrentCharacterResources,
    pub flags: CurrentCharacterFlags,
    pub opacity: CurrentCharacterScalarAccess,
    pub vitals: CurrentCharacterVitals,
    pub animation: CurrentCharacterAnimation,
    pub spatial: CurrentCharacterSpatial,
    pub reset: CurrentCharacterReset,
    pub effects: CurrentEffectEngine,
    pub policy: CurrentControllerPolicy,
    pub links: CurrentControlLinks,
    pub set_limit: usize,
}
impl NativePresentation {
    pub fn has_effect(&self, actor: CharacterIdentity, id: EffectId) -> Result<bool> {
        let unit = self
            .units
            .unit(actor)?
            .ok_or(Error::NativeOperationRejected("summon identity absent"))?;
        let UnitComponent::Present(effects) = unit.effects else {
            return Err(Error::NativeOperationRejected("summon effect chain absent"));
        };
        Ok(effects
            .entries
            .iter()
            .any(|node| node.native_id == id.value()))
    }
    /// # Safety
    /// Host independently establishes every component's full graph, writable phase,
    /// heap/resource/callback lifetime and module residency contracts. Thread-local
    /// bindings and visible identity checks do not acquire synchronization or a lease.
    pub unsafe fn bind(bytes: Arc<[u8]>, base: u64, epoch: u64, set_limit: usize) -> Result<Self> {
        if !(1..=65536).contains(&set_limit) {
            return Err(Error::InvalidProfile("invalid presentation set limit"));
        }
        // SAFETY: The caller supplies all eight component binding contracts on this actual thread.
        unsafe {
            Ok(Self {
                units: CurrentCharacterUnits::bind(
                    Arc::clone(&bytes),
                    base,
                    None,
                    CharacterUnitReadPlan {
                        effects_limit: Some(4096),
                        ..Default::default()
                    },
                )?,
                resources: CurrentCharacterResources::bind(Arc::clone(&bytes), base, epoch, None)?,
                flags: CurrentCharacterFlags::bind(Arc::clone(&bytes), base, epoch, None)?,
                opacity: CurrentCharacterScalarAccess::bind(Arc::clone(&bytes), base, epoch)?,
                vitals: CurrentCharacterVitals::bind(Arc::clone(&bytes), base, epoch, None)?,
                animation: CurrentCharacterAnimation::bind(Arc::clone(&bytes), base, epoch, None)?,
                spatial: CurrentCharacterSpatial::bind(Arc::clone(&bytes), base, epoch, None)?,
                reset: CurrentCharacterReset::bind(Arc::clone(&bytes), base, epoch, None)?,
                policy: CurrentControllerPolicy::bind(Arc::clone(&bytes), base, epoch, None)?,
                links: CurrentControlLinks::bind(Arc::clone(&bytes), base, epoch, None)?,
                effects: CurrentEffectEngine::bind(bytes, base, 4096, epoch)?,
                set_limit,
            })
        }
    }
    pub fn resource_marks(
        &self,
        actor: CharacterRegistrationObservation,
    ) -> Result<Option<CharacterResourcesObservation>> {
        let value = self.resources.observe(
            actor.character,
            WorldCollectionSelection::Creation,
            self.set_limit,
        )?;
        if value.is_some_and(|value| value.member.registration != actor) {
            return Err(Error::NativeOperationRejected(
                "presentation actor registration superseded",
            ));
        }
        Ok(value)
    }
    /// # Safety
    /// Same full native graph/exclusive host phase contract; record is retained with actor.
    pub unsafe fn request_present(&self, record: &mut Record) -> Result<()> {
        record.check()?;
        let resource = self
            .resource_marks(record.actor)?
            .ok_or(Error::NativeOperationRejected(
                "presentation resources absent",
            ))?;
        // SAFETY: Caller exclusively owns this registered actor's request byte, not a primary.
        let receipt = unsafe {
            self.resources.request(
                resource.member,
                CharacterBackreadRequest::Present,
                self.set_limit,
            )
        }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Backread(receipt), completion)
    }
    /// # Safety
    /// Same pinned exclusive actor/full graph contract. Stored team is not full hostility.
    pub unsafe fn set_team(&self, record: &mut Record, team: u8) -> Result<()> {
        record.check()?;
        let before = self
            .flags
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("team actor absent"))?;
        // SAFETY: Caller owns exactly the selected initialized byte in this native phase.
        let receipt = unsafe {
            self.flags
                .compare_mask(before, CharacterFlagByte::StoredTeam, u8::MAX, team)
        }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Flags(receipt), completion)
    }
    /// # Safety
    /// Same full writable graph/phase contract. This model request is not an activation flag.
    pub unsafe fn set_model(&self, record: &mut Record, enabled: bool) -> Result<()> {
        record.check()?;
        let before = self
            .flags
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("model actor absent"))?;
        // SAFETY: Caller owns this exact actor's selected request byte.
        let receipt = unsafe { self.flags.set_model_enable_requested(before, enabled) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Flags(receipt), completion)
    }
    /// # Safety
    /// Exact pinned actor and its original initialized flag byte, exclusive native phase.
    pub unsafe fn set_hit_suppressed(&self, record: &mut Record, suppressed: bool) -> Result<()> {
        record.check()?;
        let before = self
            .flags
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("hit actor absent"))?;
        // SAFETY: Same exclusive original selected byte contract.
        let receipt = unsafe { self.flags.set_hit_dispatch_suppressed(before, suppressed) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Flags(receipt), completion)
    }
    /// # Safety
    /// Exact pinned controller/modifier owner and selected word, exclusive native phase.
    pub unsafe fn set_lock_suppressed(&self, record: &mut Record, suppressed: bool) -> Result<()> {
        record.check()?;
        let before = self
            .policy
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("lock policy absent"))?;
        // SAFETY: Same full controller/modifier graph and exclusive word ownership.
        let receipt = unsafe { self.policy.set_lock_target_suppressed(before, suppressed) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Policy(receipt), completion)
    }
    /// # Safety
    /// Exact pinned controller/modifier and status bytes, exclusive permitted phase. Clearing
    /// these observed marks alone is not revival; reset, HP, resources and presentation are separate.
    pub unsafe fn clear_dead_marks(&self, record: &mut Record) -> Result<()> {
        record.check()?;
        let source = self
            .links
            .observe_effect_source(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("effect source actor absent"))?;
        // SAFETY: Host owns the exact initialized source handle word through postflight.
        let receipt = unsafe { self.links.clear_effect_source(source) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::EffectSource(receipt), completion)?;
        let policy = self
            .policy
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("death policy absent"))?;
        // SAFETY: Caller owns the initialized native event-death mark word.
        let receipt = unsafe { self.policy.set_event_dead_marked(policy, false) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Policy(receipt), completion)?;
        record.check()?;
        let before = self
            .flags
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("death actor absent"))?;
        // SAFETY: Native3ee730/3f9140 set death-path Status1 bit7; reset3f59c0
        // clears it. Input request bit1 is separate and is preserved.
        let receipt = unsafe { self.flags.set_native_death_marked(before, false) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Flags(receipt), completion)
    }
    /// # Safety
    /// Same live initialized status byte and exclusive actor graph/phase.
    pub unsafe fn enable_updates(&self, record: &mut Record, enabled: bool) -> Result<()> {
        record.check()?;
        let before = self
            .flags
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("update actor absent"))?;
        // SAFETY: Same exact native per-character disable-update request.
        let receipt = unsafe { self.flags.set_debug_update_disabled(before, !enabled) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Flags(receipt), completion)
    }
    pub fn observe_opacity(
        &self,
        actor: CharacterRegistrationObservation,
    ) -> Result<CharacterOpacityObservation> {
        self.opacity
            .observe_opacity(actor.character)?
            .ok_or(Error::NativeOperationRejected("opacity actor absent"))
    }
    /// # Safety
    /// Same full pinned writable graph/exclusive phase. Native control words are distinct
    /// from multipliers; engine animation238/camouflage248/word23c are preserved.
    pub unsafe fn set_opacity(&self, record: &mut Record, values: [f32; 4]) -> Result<()> {
        record.check()?;
        let before = self.observe_opacity(record.actor)?;
        // SAFETY: Caller owns all four selected original initialized fields, retaining partial stores.
        let receipt = unsafe {
            self.opacity
                .compare_manual_opacity(before, values[0], values[1], values[2], values[3])
        }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Opacity(receipt), completion)
    }
    /// # Safety
    /// Same pinned actor/PARAM/vitals graph in exclusive writable phase. Restoring HP alone
    /// does not clear corpse/event state or make a hidden actor active.
    pub unsafe fn restore_hp(&self, record: &mut Record, dying: bool) -> Result<()> {
        record.check()?;
        let before = self
            .vitals
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("vitals actor absent"))?;
        let value = if dying { 0 } else { before.hp.maximum.max(1) };
        // SAFETY: Caller owns this initialized HP word; no implicit clamping elsewhere.
        let receipt = unsafe {
            self.vitals
                .compare_current(before, CharacterVital::Hp, value)
        }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Vitals(receipt), completion)
    }
    /// # Safety
    /// Same complete event/module/animation resource graph and exclusive allowed native phase.
    pub unsafe fn animation(&self, record: &mut Record, id: i32, reused: bool) -> Result<()> {
        // Config -1 means no attempt, including preserving another system's pending request.
        if id == -1 {
            return Ok(());
        }
        let ids: &[i32] = if reused { &[-1, id] } else { &[id] };
        for &id in ids {
            record.check()?;
            let before = self
                .animation
                .observe(record.actor.character)?
                .ok_or(Error::NativeOperationRejected("animation actor absent"))?;
            // SAFETY: Caller owns this actor's actual event request and initialized native graph.
            let receipt = unsafe {
                self.animation
                    .execute(before, AnimationOperation::Request(id))
            }?;
            let completion = receipt
                .postflight
                .as_ref()
                .map(|_| ())
                .map_err(Clone::clone);
            record.commit(Operation::Animation(receipt), completion)?;
        }
        Ok(())
    }
    /// # Safety
    /// Same complete native animation request graph/exclusive phase. This explicit park
    /// operation differs from config animation -1 (which means no attempt at all).
    pub unsafe fn clear_animation(&self, record: &mut Record) -> Result<()> {
        record.check()?;
        let before = self
            .animation
            .observe(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("park animation absent"))?;
        // SAFETY: Caller owns actual event request and full native animation graph.
        let receipt = unsafe {
            self.animation
                .execute(before, AnimationOperation::Request(-1))
        }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Animation(receipt), completion)
    }
    /// # Safety
    /// Complete native setter/motion/controller graph and exclusive permitted phase.
    pub unsafe fn place_pose(&self, record: &mut Record, input: NativePoseInput) -> Result<()> {
        record.check()?;
        let before = self
            .spatial
            .observe_placement(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("placement actor absent"))?;
        // SAFETY: Caller owns all native placement input and transitive live graph.
        let receipt = unsafe { self.spatial.place_character(before, input) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Placement(Box::new(receipt)), completion)
    }
    /// # Safety
    /// Complete native reset and pose graph/phase contracts hold; resource marks alone do
    /// not prove them. Called once for a reused actor, never as a substitute for readiness.
    pub unsafe fn reset_pose(&self, record: &mut Record, input: NativePoseInput) -> Result<()> {
        record.check()?;
        let before = self
            .spatial
            .observe_placement(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("placement actor absent"))?;
        // SAFETY: Caller owns this non-primary actor's complete native reset graph and phase.
        let receipt = unsafe { self.reset.execute(before, input, false) }?;
        let completion = receipt
            .postflight
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone);
        record.commit(Operation::Reset(receipt), completion)
    }
    /// # Safety
    /// Caller pins actor/effect/PARAM/unlink/allocator and complete transitive graph in an
    /// exclusive native phase. Journal publishes the attempt before entry, including panic.
    pub unsafe fn remove_effect(&self, record: &mut Record, id: EffectId) -> Result<()> {
        record.check()?;
        record.operations.push(Operation::RemoveEffect {
            target: record.actor.character,
            id,
            returned: None,
        });
        // SAFETY: Same pinned actual identity and native unlink graph.
        let returned = unsafe { self.effects.remove(record.actor.character, id) };
        let completion = returned.as_ref().map(|_| ()).map_err(Clone::clone);
        let Operation::RemoveEffect {
            returned: stored, ..
        } = record.operations.last_mut().expect("published attempt")
        else {
            unreachable!()
        };
        *stored = Some(returned);
        if let Err(error) = &completion {
            record.failure = Some(error.clone());
        }
        completion
    }
    /// # Safety
    /// Same complete effect application/PARAM/allocator graph/exclusive phase contract.
    pub unsafe fn ensure_effect(&self, record: &mut Record, id: EffectId) -> Result<()> {
        record.check()?;
        let unit = self
            .units
            .unit(record.actor.character)?
            .ok_or(Error::NativeOperationRejected("summon identity absent"))?;
        let UnitComponent::Present(effects) = unit.effects else {
            return Err(Error::NativeOperationRejected("summon effect chain absent"));
        };
        if effects
            .entries
            .iter()
            .any(|node| node.native_id == id.value())
        {
            return Ok(());
        }
        record.operations.push(Operation::ApplyEffect {
            target: record.actor.character,
            id,
            returned: None,
        });
        // SAFETY: Same retained summon/full graph. Submitted means an attempt, not gameplay acceptance.
        let returned = unsafe { self.effects.apply(record.actor.character, id, true) };
        let completion = returned.as_ref().map(|_| ()).map_err(Clone::clone);
        let Operation::ApplyEffect {
            returned: stored, ..
        } = record.operations.last_mut().expect("published attempt")
        else {
            unreachable!()
        };
        *stored = Some(returned);
        if let Err(error) = &completion {
            record.failure = Some(error.clone());
        }
        completion
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Binding,
    Updating,
    Reactivating,
    Parking,
    Releasing,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Present,
    Model(bool),
    HitSuppressed(bool),
    LockSuppressed(bool),
    Updates(bool),
    ClearDeadMarks,
    Hp(bool),
    ClearAnimation,
    Animation { id: i32, reused: bool },
    Team(u8),
    Opacity([f32; 4]),
    Place(NativePoseInput),
    Reset(NativePoseInput),
    EnsureEffect(EffectId),
    RemoveEffect(EffectId),
}
pub struct Input {
    pub phase: Phase,
    pub pose: NativePoseInput,
    pub animation: i32,
    pub team: u8,
    pub marker: EffectId,
    pub vanish: EffectId,
    pub disable_lock_on: bool,
    /// Captured before hiding, preserving the actual two native controls and two factors.
    pub visible_opacity: [f32; 4],
}
pub fn manual_opacity(source: CharacterOpacityObservation) -> [f32; 4] {
    [
        source.control_bits[1],
        source.control_bits[2],
        source.factor_bits[2],
        source.factor_bits[0],
    ]
    .map(f32::from_bits)
}
pub fn plan(input: &Input) -> Result<Vec<Step>> {
    if input.visible_opacity.iter().any(|value| !value.is_finite())
        || input.visible_opacity[2..].iter().any(|value| *value < 0.0)
    {
        return Err(Error::InvalidProfile("invalid stored summon visibility"));
    }
    use Phase::*;
    use Step::*;
    let steps = match input.phase {
        Updating => vec![
            EnsureEffect(input.marker),
            LockSuppressed(input.disable_lock_on),
        ],
        Binding | Reactivating => {
            let reused = input.phase == Reactivating;
            let mut steps = vec![Present];
            if reused {
                steps.extend([
                    RemoveEffect(input.vanish),
                    Reset(input.pose),
                    Opacity(input.visible_opacity),
                ]);
            }
            steps.extend([
                ClearDeadMarks,
                Updates(true),
                HitSuppressed(false),
                Model(true),
                LockSuppressed(input.disable_lock_on),
                Team(input.team),
                EnsureEffect(input.marker),
            ]);
            if !reused {
                steps.push(Place(input.pose));
            }
            steps.push(Hp(false));
            if input.animation != -1 {
                steps.push(Animation {
                    id: input.animation,
                    reused,
                });
            }
            steps
        }
        Parking => vec![
            RemoveEffect(input.vanish),
            Hp(false),
            ClearDeadMarks,
            ClearAnimation,
            Present,
            Updates(true),
            HitSuppressed(true),
            LockSuppressed(true),
            Model(false),
            Opacity([0.0; 4]),
        ],
        // Resource Unload is deliberately not requested ahead of explicit engine ownership
        // retirement: that would authorize an independent native teardown against our ticket.
        Releasing => vec![
            Hp(true),
            HitSuppressed(true),
            LockSuppressed(true),
            Model(false),
            Opacity([0.0; 4]),
            Updates(false),
        ],
    };
    Ok(steps)
}
pub trait Driver {
    fn apply(&mut self, step: Step, record: &mut Record) -> Result<()>;
}
impl Driver for NativePresentation {
    fn apply(&mut self, step: Step, record: &mut Record) -> Result<()> {
        // SAFETY: The resident host provides the independently established complete native
        // graph/phase/heap/epoch contracts for every operation in this actual callback.
        unsafe {
            match step {
                Step::Present => self.request_present(record),
                Step::Model(v) => self.set_model(record, v),
                Step::HitSuppressed(v) => self.set_hit_suppressed(record, v),
                Step::LockSuppressed(v) => self.set_lock_suppressed(record, v),
                Step::Updates(v) => self.enable_updates(record, v),
                Step::ClearDeadMarks => self.clear_dead_marks(record),
                Step::Hp(dying) => self.restore_hp(record, dying),
                Step::ClearAnimation => self.clear_animation(record),
                Step::Animation { id, reused } => self.animation(record, id, reused),
                Step::Team(v) => self.set_team(record, v),
                Step::Opacity(v) => self.set_opacity(record, v),
                Step::Place(pose) => self.place_pose(record, pose),
                Step::Reset(pose) => self.reset_pose(record, pose),
                Step::EnsureEffect(id) => self.ensure_effect(record, id),
                Step::RemoveEffect(id) => self.remove_effect(record, id),
            }
        }
    }
}
pub struct Transaction {
    pub phase: Phase,
    pub record: Record,
    steps: Vec<Step>,
    next: usize,
}
impl Transaction {
    pub fn new(actor: CharacterRegistrationObservation, input: &Input) -> Result<Self> {
        Ok(Self {
            phase: input.phase,
            record: Record::new(actor),
            steps: plan(input)?,
            next: 0,
        })
    }
    pub fn complete(&self) -> bool {
        self.next == self.steps.len() && self.record.failure.is_none()
    }
    /// Publish the transaction on its resident instance before advancing. A failed step
    /// stays at that ordinal with its actual receipts; later calls do not repeat it.
    pub fn advance(&mut self, driver: &mut impl Driver) -> Result<()> {
        self.record.check()?;
        while let Some(&step) = self.steps.get(self.next) {
            if let Err(error) = driver.apply(step, &mut self.record) {
                self.record.failure = Some(error.clone());
                return Err(error);
            }
            self.next += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(phase: Phase, animation: i32) -> Input {
        Input {
            phase,
            pose: NativePoseInput {
                position_bits: [0; 4],
                angle_bits: [0; 3],
            },
            animation,
            team: 47,
            marker: EffectId::new(50).unwrap(),
            vanish: EffectId::new(60).unwrap(),
            disable_lock_on: false,
            visible_opacity: [0.25, -1.0, 0.8, 0.9],
        }
    }
    fn actor() -> CharacterRegistrationObservation {
        CharacterRegistrationObservation {
            character: CharacterIdentity {
                address: 0x1000,
                world_address: 0x2000,
                native_identifier: 3,
                epoch: 4,
            },
            set_address: 0x3000,
            entry_address: 0x4000,
            ordinal: 0,
        }
    }
    #[test]
    fn config_no_animation_is_no_attempt_and_reuse_restores_captured_native_values() {
        for phase in [Phase::Binding, Phase::Reactivating] {
            let p = plan(&input(phase, -1)).unwrap();
            assert!(
                !p.iter()
                    .any(|s| matches!(s, Step::Animation { .. } | Step::ClearAnimation))
            );
            assert!(p.contains(&Step::HitSuppressed(false)));
            assert!(p.contains(&Step::LockSuppressed(false)));
        }
        let p = plan(&input(Phase::Reactivating, 100)).unwrap();
        assert!(p.contains(&Step::Opacity([0.25, -1.0, 0.8, 0.9])));
        assert!(p.contains(&Step::Animation {
            id: 100,
            reused: true
        }));
        assert!(
            p.iter().position(|s| matches!(s, Step::Reset(_))).unwrap()
                < p.iter().position(|s| matches!(s, Step::Hp(_))).unwrap()
        );
        let parking = plan(&input(Phase::Parking, -1)).unwrap();
        assert!(parking.contains(&Step::ClearAnimation));
        assert!(parking.contains(&Step::Opacity([0.0; 4])));
        assert!(parking.contains(&Step::HitSuppressed(true)));
        assert_eq!(
            plan(&input(Phase::Updating, 100)).unwrap(),
            [
                Step::EnsureEffect(EffectId::new(50).unwrap()),
                Step::LockSuppressed(false)
            ]
        );
    }
    struct Mock {
        calls: Vec<Step>,
        fail_at: Option<usize>,
    }
    impl Driver for Mock {
        fn apply(&mut self, step: Step, record: &mut Record) -> Result<()> {
            self.calls.push(step);
            if self.fail_at == Some(self.calls.len() - 1) {
                record.operations.push(Operation::RemoveEffect {
                    target: record.actor.character,
                    id: EffectId::new(60).unwrap(),
                    returned: Some(Err(Error::NativeOperationRejected(
                        "uncertain actual attempt",
                    ))),
                });
                Err(Error::NativeOperationRejected("uncertain actual attempt"))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn every_partial_stage_retains_receipts_and_blocks_repeat_or_later_operations() {
        let input = input(Phase::Reactivating, 100);
        let expected = plan(&input).unwrap();
        for fail_at in 0..expected.len() {
            let mut transaction = Transaction::new(actor(), &input).unwrap();
            let mut driver = Mock {
                calls: Vec::new(),
                fail_at: Some(fail_at),
            };
            assert!(transaction.advance(&mut driver).is_err());
            assert!(!transaction.complete());
            assert_eq!(driver.calls, &expected[..=fail_at]);
            assert_eq!(transaction.record.operations.len(), 1);
            assert!(transaction.advance(&mut driver).is_err());
            assert_eq!(driver.calls, &expected[..=fail_at]);
        }
        let mut transaction = Transaction::new(actor(), &input).unwrap();
        let mut driver = Mock {
            calls: Vec::new(),
            fail_at: None,
        };
        transaction.advance(&mut driver).unwrap();
        assert!(transaction.complete());
        transaction.advance(&mut driver).unwrap();
        assert_eq!(driver.calls, expected);
    }
    #[test]
    fn manual_visibility_keeps_native_animation_camouflage_and_negative_control() {
        let observation = CharacterOpacityObservation {
            character: actor().character,
            kind: shikifsrs_elden_ring::objects::ObjectKind::Enemy,
            factor_bits: [
                0.9f32.to_bits(),
                0.3f32.to_bits(),
                0.8f32.to_bits(),
                0.2f32.to_bits(),
            ],
            control_bits: [0xffff_ffff, 0.25f32.to_bits(), (-1.0f32).to_bits()],
        };
        assert_eq!(manual_opacity(observation), [0.25, -1.0, 0.8, 0.9]);
        let mut i = input(Phase::Parking, -1);
        i.visible_opacity[0] = f32::NAN;
        assert!(plan(&i).is_err());
    }
}
