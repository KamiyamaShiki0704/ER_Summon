mod chr_lifecycle;
mod retirement;

use std::{
    ffi::{OsString, c_void},
    fs,
    io::Write,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, Once, OnceLock,
        atomic::{AtomicIsize, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use chr_lifecycle::{
    DestroyOutcome, EntityIdOutcome, character_reports_event_entity_id, chr_set_usage,
    destroy_summon, entity_id_owner, register_event_entity_id, release_summon_event_ids,
};

use eldenring::{
    cs::{
        BlockId, CSCamExt, CSCamera, CSTaskGroupIndex, CSTaskImp, ChrIns, ChrInsExt,
        FieldInsHandle, FieldInsSelector, WorldChrMan,
    },
    fd4::FD4TaskData,
    position::HavokPosition,
    rotation::Quaternion,
    util::system::wait_for_system_init,
};
use fromsoftware_shared::{
    F32Vector4, FromStatic, GameVersion, LANG_ID_EN, LANG_ID_JP, SharedTaskImpExt, program::Program,
};
use serde::Deserialize;
use windows::Win32::{
    Foundation::{HINSTANCE, HMODULE},
    System::LibraryLoader::GetModuleFileNameW,
};

const DLL_PROCESS_ATTACH: u32 = 1;
const CONFIG_FILE_NAME: &str = "summon.toml";
const LOG_FILE_NAME: &str = "summon.log";
const DEBUG_CREATOR_INIT_DATA_SIZE: usize = 0x100;
const DEBUG_CREATOR_SPAWN_ROTATION_OFFSET: usize = 0x10;
const DEBUG_CREATOR_ENEMY_TYPE_OFFSET: usize = 0xc8;

static INSTALL_TASKS: Once = Once::new();
static DLL_MODULE: AtomicIsize = AtomicIsize::new(0);
static SETTINGS: OnceLock<AttackSummonSettings> = OnceLock::new();
static LOG: LazyLock<Mutex<Option<fs::File>>> = LazyLock::new(|| Mutex::new(None));
static STATE: LazyLock<Mutex<AttackSummonState>> =
    LazyLock::new(|| Mutex::new(AttackSummonState::default()));
// Temporary, bounded diagnostics for the stalled FrameEnd path. These counters
// observe dispatch only; they must never authorize destruction on another group.
static RETIREMENT_CALLBACKS: AtomicU64 = AtomicU64::new(0);
static RETIREMENT_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static RETIREMENT_GROUP: AtomicU32 = AtomicU32::new(u32::MAX);
static RETIREMENT_STATUS: AtomicU32 = AtomicU32::new(0);
static RETIREMENT_REPORT_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Appends a line to `summon.log` next to the DLL.
///
/// Logging exists so the lifecycle claims shipped with this mod are verifiable in
/// a live session without a debugger: every bind reports the entity id the engine
/// and this DLL settled on, and every removal reports whether the character set
/// entry was actually released.
pub(crate) fn log_line(message: impl std::fmt::Display) {
    if !settings().log_enabled {
        return;
    }
    let Ok(mut guard) = LOG.lock() else {
        return;
    };
    if guard.is_none() {
        let Some(path) = module_dir().map(|dir| dir.join(LOG_FILE_NAME)) else {
            return;
        };
        *guard = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
    }
    if let Some(file) = guard.as_mut() {
        let _ = writeln!(file, "{message}");
        let _ = file.flush();
    }
}

#[unsafe(no_mangle)]
/// # Safety
pub unsafe extern "C" fn DllMain(hmodule: HINSTANCE, reason: u32, _: *mut c_void) -> i32 {
    DLL_MODULE.store(hmodule.0 as isize, Ordering::Relaxed);
    if reason != DLL_PROCESS_ATTACH {
        return 1;
    }

    thread::spawn(|| {
        thread::sleep(settings().start_delay());
        let program = Program::current();
        if supported_executable(program) && wait_for_system_init(&program, Duration::MAX).is_ok() {
            install_tasks_once();
        }
    });

    1
}

#[derive(Clone, Copy)]
enum SupportedErVersion {
    Ww2700,
    Ww2710,
    Jp2701,
}

impl GameVersion for SupportedErVersion {
    const NAME: &'static str = "elden ring";

    fn from_lang_version(lang_id: u16, version: &str) -> Option<Self> {
        match (lang_id, version) {
            (LANG_ID_EN, "2.7.0.0") => Some(Self::Ww2700),
            (LANG_ID_EN, "2.7.1.0") => Some(Self::Ww2710),
            (LANG_ID_JP, "2.7.0.1") => Some(Self::Jp2701),
            _ => None,
        }
    }
}

fn supported_executable(program: Program<'_>) -> bool {
    let Program::Mapping(module) = program else {
        return false;
    };
    SupportedErVersion::detect(&module).is_ok()
}

fn install_tasks_once() {
    INSTALL_TASKS.call_once(|| {
        let Ok(task_imp) = CSTaskImp::wait_for_instance(Duration::from_secs(30)) else {
            return;
        };

        let attack_summon = task_imp.run_recurring(
            attack_summon_task as fn(&FD4TaskData),
            CSTaskGroupIndex::ChrIns_PostPhysics,
        );
        std::mem::forget(attack_summon);
        let retirement_task = task_imp.run_recurring(
            retirement_frame_end as fn(&FD4TaskData), CSTaskGroupIndex::FrameEnd,
        );
        std::mem::forget(retirement_task);

        let settings = settings();
        log_line(format_args!(
            "summon initialised: retirement=frame-end-v3 entity_registration=native-groups-v1 remove_mode={:?} register_event_entity_id={} \
             effective_reuse={} destroy_delay_ms={} summons={}",
            settings.remove_mode,
            settings.register_event_entity_id,
            settings.reuses_units(),
            settings.destroy_delay_ms,
            settings.summons.len(),
        ));
    });
}

fn is_retirement_frame_end(group: u32) -> bool {
    // Registration takes an ordinal, but the pinned WW2.7.1.0 game supplies
    // 0x900000A8 to this FrameEnd callback (diag1 live evidence). Accept only
    // that observed encoding and the plain ordinal; do not broadly mask bits
    // whose meaning has not been established for other scheduler groups.
    group == CSTaskGroupIndex::FrameEnd as u32 || group == 0x9000_00a8
}

#[cfg(test)]
mod retirement_dispatch_tests {
    #[test]
    fn accepts_observed_frame_end_id_but_rejects_other_groups() {
        // Exact FD4TaskData value from the failed diag1 live session.
        assert!(super::is_retirement_frame_end(2_415_919_272));
        assert!(super::is_retirement_frame_end(168));
        for group in [0, 167, 169, 0x9000_00a7, 0x9000_00a9, 0x8000_00a8, u32::MAX] {
            assert!(!super::is_retirement_frame_end(group));
        }
    }
}

fn retirement_frame_end(data: &FD4TaskData) {
    let first = RETIREMENT_CALLBACKS.fetch_add(1, Ordering::Relaxed) == 0;
    RETIREMENT_GROUP.store(data.task_group_id, Ordering::Relaxed);
    if first {
        log_line(format_args!(
            "[DEBUG-retirement-dispatch] first-callback observed_group={} expected_group={}",
            data.task_group_id, CSTaskGroupIndex::FrameEnd as u32
        ));
    }
    if !is_retirement_frame_end(data.task_group_id) {
        RETIREMENT_STATUS.store(1, Ordering::Relaxed);
        return;
    }
    RETIREMENT_ACCEPTED.fetch_add(1, Ordering::Relaxed);
    let Ok(world) = (unsafe { WorldChrMan::instance_mut() }) else {
        RETIREMENT_STATUS.store(2, Ordering::Relaxed);
        return;
    };
    if world.main_player.is_none() {
        RETIREMENT_STATUS.store(3, Ordering::Relaxed);
        retirement::quarantine_all("player-unavailable");
        return;
    }
    let Ok(mut state) = STATE.lock() else {
        RETIREMENT_STATUS.store(4, Ordering::Relaxed);
        return;
    };
    RETIREMENT_STATUS.store(5, Ordering::Relaxed);
    retirement::poll(world);
    state.finish_pending_destruction(world, Instant::now());
    RETIREMENT_STATUS.store(6, Ordering::Relaxed);
}

fn attack_summon_task(_: &FD4TaskData) {
    let settings = settings();
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance_mut() }) else {
        retirement::quarantine_all("world-unavailable");
        if let Ok(mut state) = STATE.lock() {
            state.summons.clear();
        }
        return;
    };

    let now = Instant::now();
    let Ok(mut state) = STATE.lock() else {
        return;
    };

    state.ensure_trigger_slots(settings.summons.len());
    state.update_existing(world_chr_man, now, settings);
    let staged = state.summons.iter().filter(|s| s.destroy_at.is_some()).count();
    if staged > 0 {
        if let Ok(mut report_at) = RETIREMENT_REPORT_AT.lock() {
            if report_at.is_none_or(|at| now >= at) {
                *report_at = Some(now + Duration::from_secs(5));
                log_line(format_args!(
                    "[DEBUG-retirement-dispatch] staged={} callbacks={} accepted={} observed_group={} expected_group={} status={} (0=not-called,1=group-rejected,2=no-world,3=no-player,4=lock-poisoned,5=processing,6=completed)",
                    staged, RETIREMENT_CALLBACKS.load(Ordering::Relaxed),
                    RETIREMENT_ACCEPTED.load(Ordering::Relaxed),
                    RETIREMENT_GROUP.load(Ordering::Relaxed), CSTaskGroupIndex::FrameEnd as u32,
                    RETIREMENT_STATUS.load(Ordering::Relaxed),
                ));
            }
        }
    }

    let Some((player_pos, player_yaw, triggers)) = consume_player_triggers(world_chr_man, settings)
    else {
        state.release_all(world_chr_man, settings);
        return;
    };
    if triggers.is_empty() || retirement::pending() {
        return;
    }

    let (forward, right) = camera_horizontal_axes();
    for config_index in triggers {
        if state.active_count() >= settings.max_active_summons {
            break;
        }
        if !state.accept_trigger(config_index, now, settings) {
            continue;
        }
        state.activate_or_spawn(
            world_chr_man,
            config_index,
            now,
            player_pos,
            player_yaw,
            forward,
            right,
            settings,
        );
    }
}

fn consume_player_triggers(
    world_chr_man: &mut WorldChrMan,
    settings: &AttackSummonSettings,
) -> Option<(HavokPosition, f32, Vec<usize>)> {
    let player = world_chr_man.main_player.as_mut()?;
    let active_effects = player
        .chr_ins
        .special_effect
        .entries()
        .map(|entry| entry.param_id)
        .collect::<Vec<_>>();

    let triggers = settings
        .summons
        .iter()
        .enumerate()
        .filter_map(|(index, config)| {
            active_effects
                .contains(&config.trigger_speffect)
                .then_some(index)
        })
        .collect::<Vec<_>>();

    for index in &triggers {
        player
            .chr_ins
            .remove_speffect(settings.summons[*index].trigger_speffect);
    }

    let physics = player.chr_ins.modules.as_ref().physics.as_ref();
    Some((
        physics.position,
        yaw_from_orientation(physics.orientation),
        triggers,
    ))
}

fn yaw_from_orientation(Quaternion(qx, qy, qz, qw): Quaternion) -> f32 {
    let forward_x = 2.0 * (qx * qz + qw * qy);
    let forward_z = 1.0 - 2.0 * (qx * qx + qy * qy);
    forward_x.atan2(forward_z)
}

fn camera_horizontal_axes() -> ((f32, f32, f32), (f32, f32, f32)) {
    let Ok(camera) = (unsafe { CSCamera::instance() }) else {
        return ((0.0, 0.0, 1.0), (1.0, 0.0, 0.0));
    };

    let forward = camera.pers_cam_1.forward();
    let right = camera.pers_cam_1.right();
    (
        normalize_horizontal((forward.0, forward.1, forward.2), (0.0, 0.0, 1.0)),
        normalize_horizontal((right.0, right.1, right.2), (1.0, 0.0, 0.0)),
    )
}

fn normalize_horizontal(vector: (f32, f32, f32), fallback: (f32, f32, f32)) -> (f32, f32, f32) {
    let len = (vector.0 * vector.0 + vector.2 * vector.2).sqrt();
    if len <= 0.001 || !len.is_finite() {
        return fallback;
    }
    (vector.0 / len, 0.0, vector.2 / len)
}

/// What happens to a summon that has to disappear.
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum RemoveMode {
    /// Only field writes: the unit stops rendering and updating but stays resident.
    Hide,
    /// Real removal: the event id is released, the engine destructor runs and the
    /// character set entry is cleared, so nothing of the unit is left in memory.
    #[default]
    Destroy,
}

#[derive(Clone, Deserialize)]
struct AttackSummonSettings {
    #[serde(default = "default_marker_speffect")]
    marker_speffect: i32,
    #[serde(default = "default_vanish_request_speffect")]
    vanish_request_speffect: i32,
    #[serde(default = "default_generated_team_type")]
    generated_team_type: u8,
    #[serde(default = "default_max_active_summons")]
    max_active_summons: usize,
    #[serde(default = "default_start_delay_ms")]
    start_delay_ms: u64,
    #[serde(default = "default_unbound_timeout_ms")]
    unbound_timeout_ms: u64,
    #[serde(default = "default_fallback_lifetime_ms")]
    fallback_lifetime_ms: i64,
    #[serde(default = "default_offset_forward", alias = "spawn_forward_distance")]
    offset_forward: f32,
    #[serde(default = "default_offset_right")]
    offset_right: f32,
    #[serde(default = "default_offset_up", alias = "spawn_height_offset")]
    offset_up: f32,
    #[serde(default = "default_same_trigger_cooldown_ms")]
    same_trigger_cooldown_ms: u64,
    #[serde(default = "default_any_trigger_cooldown_ms")]
    any_trigger_cooldown_ms: u64,
    #[serde(default = "default_reuse_spawned_units")]
    reuse_spawned_units: bool,
    #[serde(default = "default_disable_lock_on")]
    disable_lock_on: bool,
    #[serde(default)]
    remove_mode: RemoveMode,
    #[serde(default = "default_destroy_delay_ms")]
    destroy_delay_ms: u64,
    #[serde(default = "default_register_event_entity_id")]
    register_event_entity_id: bool,
    #[serde(default = "default_log_enabled")]
    log_enabled: bool,
    #[serde(default = "default_summons")]
    summons: Vec<AttackSummonConfig>,
}

impl Default for AttackSummonSettings {
    fn default() -> Self {
        Self {
            marker_speffect: default_marker_speffect(),
            vanish_request_speffect: default_vanish_request_speffect(),
            generated_team_type: default_generated_team_type(),
            max_active_summons: default_max_active_summons(),
            start_delay_ms: default_start_delay_ms(),
            unbound_timeout_ms: default_unbound_timeout_ms(),
            fallback_lifetime_ms: default_fallback_lifetime_ms(),
            offset_forward: default_offset_forward(),
            offset_right: default_offset_right(),
            offset_up: default_offset_up(),
            same_trigger_cooldown_ms: default_same_trigger_cooldown_ms(),
            any_trigger_cooldown_ms: default_any_trigger_cooldown_ms(),
            reuse_spawned_units: default_reuse_spawned_units(),
            disable_lock_on: default_disable_lock_on(),
            remove_mode: RemoveMode::default(),
            destroy_delay_ms: default_destroy_delay_ms(),
            register_event_entity_id: default_register_event_entity_id(),
            log_enabled: default_log_enabled(),
            summons: default_summons(),
        }
    }
}

impl AttackSummonSettings {
    fn start_delay(&self) -> Duration {
        Duration::from_millis(self.start_delay_ms)
    }

    fn unbound_timeout(&self) -> Duration {
        Duration::from_millis(self.unbound_timeout_ms.max(1))
    }

    fn same_trigger_cooldown(&self) -> Duration {
        Duration::from_millis(self.same_trigger_cooldown_ms)
    }

    fn any_trigger_cooldown(&self) -> Duration {
        Duration::from_millis(self.any_trigger_cooldown_ms)
    }

    fn destroy_delay(&self) -> Duration {
        Duration::from_millis(self.destroy_delay_ms)
    }

    /// Unit reuse and real removal are mutually exclusive: a destroyed character
    /// cannot be reactivated, so `reuse_spawned_units` is ignored while
    /// `remove_mode = "destroy"`.
    fn reuses_units(&self) -> bool {
        self.reuse_spawned_units && self.remove_mode == RemoveMode::Hide
    }
}

fn settings() -> &'static AttackSummonSettings {
    SETTINGS.get_or_init(load_settings)
}

fn load_settings() -> AttackSummonSettings {
    let Some(path) = config_path() else {
        return AttackSummonSettings::default();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return AttackSummonSettings::default();
    };
    let Ok(mut settings) = toml::from_str::<AttackSummonSettings>(&text) else {
        return AttackSummonSettings::default();
    };
    settings.summons.retain(|config| {
        config.trigger_speffect > 0 && config.chr_id > 0 && config.npc_param_id > 0
    });
    if settings.summons.is_empty() {
        settings.summons = default_summons();
    }
    settings
}

fn config_path() -> Option<PathBuf> {
    module_dir().map(|dir| dir.join(CONFIG_FILE_NAME))
}

fn module_dir() -> Option<PathBuf> {
    let module = DLL_MODULE.load(Ordering::Relaxed);
    if module == 0 {
        return None;
    }

    let mut buffer = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(HMODULE(module as *mut _)), &mut buffer) };
    if len == 0 {
        return None;
    }

    let path = PathBuf::from(OsString::from_wide(&buffer[..len as usize]));
    path.parent().map(Path::to_path_buf)
}

#[derive(Clone, Copy, Deserialize)]
struct AttackSummonConfig {
    trigger_speffect: i32,
    #[serde(default = "default_config_chr_id")]
    chr_id: i32,
    #[serde(default)]
    chara_init_param_id: i32,
    npc_param_id: i32,
    #[serde(default)]
    npc_think_param_id: i32,
    #[serde(default)]
    event_entity_id: i32,
    #[serde(default)]
    talk_id: i32,
    animation_id: i32,
    #[serde(default)]
    is_player: bool,
    #[serde(default)]
    marker_speffect: Option<i32>,
    #[serde(default)]
    vanish_request_speffect: Option<i32>,
    #[serde(default)]
    generated_team_type: Option<u8>,
    #[serde(default)]
    fallback_lifetime_ms: Option<i64>,
    #[serde(default, alias = "spawn_forward_distance")]
    offset_forward: Option<f32>,
    #[serde(default, alias = "side_offset")]
    offset_right: Option<f32>,
    #[serde(default, alias = "spawn_height_offset")]
    offset_up: Option<f32>,
    #[serde(default)]
    disable_lock_on: Option<bool>,
}

impl AttackSummonConfig {
    fn marker_speffect(&self, settings: &AttackSummonSettings) -> i32 {
        self.marker_speffect.unwrap_or(settings.marker_speffect)
    }

    fn vanish_request_speffect(&self, settings: &AttackSummonSettings) -> i32 {
        self.vanish_request_speffect
            .unwrap_or(settings.vanish_request_speffect)
    }

    fn generated_team_type(&self, settings: &AttackSummonSettings) -> u8 {
        self.generated_team_type
            .unwrap_or(settings.generated_team_type)
    }

    fn expires_at(&self, settings: &AttackSummonSettings, now: Instant) -> Option<Instant> {
        let millis = self
            .fallback_lifetime_ms
            .unwrap_or(settings.fallback_lifetime_ms);
        if millis == -1 {
            None
        } else {
            // Preserve the previous zero -> 1 ms behavior. Other negatives
            // are not an unlimited-lifetime sentinel.
            Some(now + Duration::from_millis(millis.max(1) as u64))
        }
    }

    fn offset_forward(&self, settings: &AttackSummonSettings) -> f32 {
        self.offset_forward.unwrap_or(settings.offset_forward)
    }

    fn offset_right(&self, settings: &AttackSummonSettings) -> f32 {
        self.offset_right.unwrap_or(settings.offset_right)
    }

    fn offset_up(&self, settings: &AttackSummonSettings) -> f32 {
        self.offset_up.unwrap_or(settings.offset_up)
    }

    fn disable_lock_on(&self, settings: &AttackSummonSettings) -> bool {
        self.disable_lock_on.unwrap_or(settings.disable_lock_on)
    }
}

#[derive(Clone, Copy)]
struct AttackSummonInstance {
    config_index: usize,
    requested_at: Instant,
    expires_at: Option<Instant>,
    handle: Option<FieldInsHandle>,
    active: bool,
    pending_bind: bool,
    /// Set once the unit has been made inert and is waiting for real removal.
    /// The unit is always hidden for at least one tick before it is destroyed.
    destroy_at: Option<Instant>,
}

impl AttackSummonInstance {
    fn removal_started(&self) -> bool {
        self.destroy_at.is_some()
    }
}

#[derive(Default)]
struct AttackSummonState {
    summons: Vec<AttackSummonInstance>,
    last_any_trigger: Option<Instant>,
    last_trigger_by_slot: Vec<Option<Instant>>,
}

impl AttackSummonState {
    fn ensure_trigger_slots(&mut self, count: usize) {
        if self.last_trigger_by_slot.len() != count {
            self.last_trigger_by_slot.resize(count, None);
        }
    }

    fn accept_trigger(
        &mut self,
        config_index: usize,
        now: Instant,
        settings: &AttackSummonSettings,
    ) -> bool {
        if self
            .last_any_trigger
            .is_some_and(|last| now.duration_since(last) < settings.any_trigger_cooldown())
        {
            return false;
        }
        if self.last_trigger_by_slot[config_index]
            .is_some_and(|last| now.duration_since(last) < settings.same_trigger_cooldown())
        {
            return false;
        }

        self.last_any_trigger = Some(now);
        self.last_trigger_by_slot[config_index] = Some(now);
        true
    }

    fn active_count(&self) -> usize {
        self.summons
            .iter()
            .filter(|summon| summon.active || summon.pending_bind)
            .count()
    }

    fn activate_or_spawn(
        &mut self,
        world_chr_man: &mut WorldChrMan,
        config_index: usize,
        now: Instant,
        player_pos: HavokPosition,
        player_yaw: f32,
        forward: (f32, f32, f32),
        right: (f32, f32, f32),
        settings: &AttackSummonSettings,
    ) {
        let config = settings.summons[config_index];
        let position = summon_position(player_pos, forward, right, config, settings);

        if settings.reuses_units()
            && let Some(index) = self
                .summons
                .iter()
                .position(|summon| summon.config_index == config_index)
        {
            if self.summons[index].active || self.summons[index].pending_bind {
                return;
            }
            if self.summons[index].destroy_at.is_some() {
                // Staged for real removal; never bring it back.
                self.summons.swap_remove(index);
            } else if let Some(handle) = self.summons[index].handle {
                if let Some(chr) = chr_by_handle_mut(world_chr_man, handle) {
                    activate_existing_summon(chr, position, player_yaw, config, settings);
                    self.summons[index].requested_at = now;
                    self.summons[index].expires_at = config.expires_at(settings, now);
                    self.summons[index].active = true;
                    self.summons[index].pending_bind = false;
                    log_line(format_args!(
                        "summon reactivated: handle={handle} npc_param_id={} \
                         event_entity_id={}",
                        config.npc_param_id, config.event_entity_id
                    ));
                    return;
                }

                self.summons.swap_remove(index);
            }
        }

        self.spawn_new(
            world_chr_man,
            config_index,
            now,
            position,
            player_yaw,
            settings,
        );
    }

    fn spawn_new(
        &mut self,
        world_chr_man: &mut WorldChrMan,
        config_index: usize,
        now: Instant,
        position: HavokPosition,
        player_yaw: f32,
        settings: &AttackSummonSettings,
    ) {
        let config = settings.summons[config_index];
        world_chr_man.debug_chr_creator.last_created_chr = None;
        if !spawn_debug_character(
            world_chr_man,
            &AttackSummonSpawnRequest {
                chr_id: config.chr_id,
                chara_init_param_id: config.chara_init_param_id,
                npc_param_id: config.npc_param_id,
                npc_think_param_id: config.npc_think_param_id,
                event_entity_id: config.event_entity_id,
                talk_id: config.talk_id,
                is_player: config.is_player,
                pos_x: position.0,
                pos_y: position.1,
                pos_z: position.2,
                rot_x: 0.0,
                rot_y: player_yaw,
                rot_z: 0.0,
            },
        ) {
            return;
        }

        self.summons.push(AttackSummonInstance {
            config_index,
            requested_at: now,
            expires_at: config.expires_at(settings, now),
            handle: None,
            active: true,
            pending_bind: true,
            destroy_at: None,
        });
    }

    fn update_existing(
        &mut self,
        world_chr_man: &mut WorldChrMan,
        now: Instant,
        settings: &AttackSummonSettings,
    ) {
        self.bind_last_created(world_chr_man, settings);

        let mut index = 0;
        while index < self.summons.len() {
            let should_remove = {
                let summon = &mut self.summons[index];
                if summon.removal_started() {
                    true
                } else if !summon.active && !summon.pending_bind {
                    false
                } else if summon.handle.is_none() {
                    now.duration_since(summon.requested_at) >= settings.unbound_timeout()
                } else {
                    summon_should_disappear(world_chr_man, summon, now, settings)
                }
            };

            if !should_remove {
                index += 1;
                continue;
            }

            if settings.remove_mode == RemoveMode::Destroy {
                // Stage the unit first: make it inert and release its event ids,
                // then let at least one tick pass before the character is torn
                // down, so no other system is mid-iteration over it.
                if self.summons[index].destroy_at.is_none() {
                    let summon = &mut self.summons[index];
                    let handle_text = match summon.handle {
                        Some(handle) => handle.to_string(),
                        None => "none".to_string(),
                    };
                    let (released, consistent) = match summon.handle {
                        Some(handle) => release_summon_event_ids(world_chr_man, handle),
                        None => (0, true),
                    };
                    release_summon(world_chr_man, summon);
                    summon.destroy_at =
                        Some(now.checked_add(settings.destroy_delay()).unwrap_or(now));
                    let config = settings.summons[summon.config_index];
                    log_line(format_args!(
                        "summon staged for removal: handle={handle_text} npc_param_id={} \
                         configured_event_entity_id={} released_event_ids={released} \
                         entry_consistent={consistent}",
                        config.npc_param_id, config.event_entity_id
                    ));
                }

                // Physical removal is exclusively owned by the FrameEnd task.
                index += 1;
            } else if settings.reuses_units() && self.summons[index].handle.is_some() {
                let summon = &mut self.summons[index];
                park_summon(world_chr_man, summon, settings);
                index += 1;
            } else {
                let summon = self.summons.swap_remove(index);
                if let Some(handle) = summon.handle {
                    release_summon_event_ids(world_chr_man, handle);
                }
                release_summon(world_chr_man, &summon);
            }
        }
    }

    fn finish_pending_destruction(&mut self, world: &mut WorldChrMan, now: Instant) {
        let mut index = 0;
        while index < self.summons.len() {
            if !self.summons[index].destroy_at.is_some_and(|at| now >= at) {
                index += 1;
                continue;
            }
            let summon = self.summons[index];
            let outcome = summon.handle.map(|handle| destroy_summon(world, handle));
            if matches!(
                outcome,
                None | Some(DestroyOutcome::RetirementStarted) | Some(DestroyOutcome::NotFound)
            ) {
                self.summons.swap_remove(index);
            } else {
                log_line(format_args!(
                    "summon retirement refused: outcome={}",
                    outcome.unwrap()
                ));
                // Retry at a bounded rate; never discard an unresolved live instance.
                self.summons[index].destroy_at = Some(now + Duration::from_secs(1));
                index += 1;
            }
        }
    }

    fn bind_last_created(
        &mut self,
        world_chr_man: &mut WorldChrMan,
        settings: &AttackSummonSettings,
    ) {
        let Some(mut chr_ptr) = world_chr_man.debug_chr_creator.last_created_chr else {
            return;
        };
        let chr = unsafe { chr_ptr.as_mut() };
        let Some((summon_index, config)) =
            self.summons.iter().enumerate().find_map(|(index, summon)| {
                let config = settings.summons[summon.config_index];
                (summon.handle.is_none() && chr.npc_param_id == config.npc_param_id)
                    .then_some((index, config))
            })
        else {
            return;
        };

        prepare_summon(chr, config, settings);
        chr.modules.as_mut().event.as_mut().request_animation_id = config.animation_id;
        let handle = chr.field_ins_handle;
        self.summons[summon_index].handle = Some(handle);
        self.summons[summon_index].active = true;
        self.summons[summon_index].pending_bind = false;
        world_chr_man.debug_chr_creator.last_created_chr = None;

        // Make the configured entity id real and report what actually happened, so
        // the mapping can be confirmed from the log instead of from a debugger.
        let outcome = register_event_entity_id(world_chr_man, handle, config.event_entity_id);
        let chr_reports_id =
            character_reports_event_entity_id(world_chr_man, handle, config.event_entity_id)
                .unwrap_or(false);
        // Local map check only. The actual EMEVD consumer must be tested separately.
        let id_leads_back =
            entity_id_owner(world_chr_man, handle, config.event_entity_id) == Some(handle);
        let (ids, capacity) = chr_set_usage(world_chr_man, handle);
        log_line(format_args!(
            "summon bound: handle={handle} npc_param_id={} configured_event_entity_id={} \
             chr_reports_id={chr_reports_id} local_set_id_resolves_to_this_unit={id_leads_back} \
             entity_id_outcome={outcome} set_event_ids={ids} set_capacity={capacity}",
            config.npc_param_id, config.event_entity_id,
        ));
        match outcome {
            EntityIdOutcome::Conflict => log_line(format_args!(
                "summon warning: event_entity_id={} is already owned by another character in \
                 this character set; the configured id was NOT applied",
                config.event_entity_id
            )),
            EntityIdOutcome::EntryMismatch => log_line(format_args!(
                "summon warning: handle={handle} and its character set entry disagree; the \
                 configured event_entity_id={} was NOT applied",
                config.event_entity_id
            )),
            _ if config.event_entity_id > 0 && !id_leads_back => log_line(format_args!(
                "summon warning: event_entity_id={} does not resolve back to handle={handle}; \
                 event scripts will not find this unit through that id",
                config.event_entity_id
            )),
            _ => {}
        }
    }

    /// Drops every summon, for instance when the local player leaves the session.
    ///
    /// The removal path stays the same on purpose: it clears the character set
    /// entry as its last step, which turns a later engine-side character set
    /// teardown into a no-op for that slot instead of a second free. When the
    /// engine has already torn the set down the handle stops resolving, so
    /// nothing is freed and the outcome says so.
    fn release_all(&mut self, world_chr_man: &mut WorldChrMan, settings: &AttackSummonSettings) {
        for summon in &mut self.summons {
            if summon.destroy_at.is_some() {
                continue;
            }
            if let Some(handle) = summon.handle {
                release_summon_event_ids(world_chr_man, handle);
            }
            release_summon(world_chr_man, summon);
            if settings.remove_mode == RemoveMode::Destroy {
                summon.destroy_at = Some(Instant::now() + settings.destroy_delay());
            }
        }
        if settings.remove_mode != RemoveMode::Destroy {
            self.summons.clear();
        }
    }
}

struct AttackSummonSpawnRequest {
    chr_id: i32,
    chara_init_param_id: i32,
    npc_param_id: i32,
    npc_think_param_id: i32,
    event_entity_id: i32,
    talk_id: i32,
    is_player: bool,
    pos_x: f32,
    pos_y: f32,
    pos_z: f32,
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
}

fn spawn_debug_character(
    world_chr_man: &mut WorldChrMan,
    request: &AttackSummonSpawnRequest,
) -> bool {
    let creator = &mut world_chr_man.debug_chr_creator;
    let init_data = &mut creator.init_data;
    if std::mem::size_of_val(init_data) != DEBUG_CREATOR_INIT_DATA_SIZE {
        return false;
    }

    let mut name_bytes = format!("c{:0>4}", request.chr_id)
        .encode_utf16()
        .collect::<Vec<_>>();
    name_bytes.resize(0x20, 0);

    init_data.name.clone_from_slice(&name_bytes);
    init_data.chara_init_param_id = request.chara_init_param_id;
    init_data.npc_param_id = request.npc_param_id;
    init_data.npc_think_param_id = request.npc_think_param_id;
    init_data.event_entity_id = request.event_entity_id;
    init_data.talk_id = request.talk_id;
    init_data.spawn_position = F32Vector4(request.pos_x, request.pos_y, request.pos_z, 0.0);

    // These fields are private in the pinned fsrs revision. The exact revision and
    // init-data size guard keep this small bridge tied to its verified layout.
    unsafe {
        let base = std::ptr::from_mut(init_data).cast::<u8>();
        base.add(DEBUG_CREATOR_SPAWN_ROTATION_OFFSET)
            .cast::<F32Vector4>()
            .write(F32Vector4(request.rot_x, request.rot_y, request.rot_z, 0.0));
        base.add(DEBUG_CREATOR_ENEMY_TYPE_OFFSET)
            .write(u8::from(request.is_player));
    }

    creator.spawn = true;
    true
}

fn summon_should_disappear(
    world_chr_man: &mut WorldChrMan,
    summon: &AttackSummonInstance,
    now: Instant,
    settings: &AttackSummonSettings,
) -> bool {
    if summon.expires_at.is_some_and(|deadline| now >= deadline) {
        return true;
    }
    let config = settings.summons[summon.config_index];

    let Some(handle) = summon.handle else {
        return false;
    };
    let Some(chr) = chr_by_handle_mut(world_chr_man, handle) else {
        return true;
    };

    if chr_dead(chr) || chr_has_speffect(chr, config.vanish_request_speffect(settings)) {
        return true;
    }

    prepare_summon(chr, config, settings);
    false
}

fn summon_position(
    player_pos: HavokPosition,
    forward: (f32, f32, f32),
    right: (f32, f32, f32),
    config: AttackSummonConfig,
    settings: &AttackSummonSettings,
) -> HavokPosition {
    HavokPosition(
        player_pos.0
            + forward.0 * config.offset_forward(settings)
            + right.0 * config.offset_right(settings),
        player_pos.1 + config.offset_up(settings),
        player_pos.2
            + forward.2 * config.offset_forward(settings)
            + right.2 * config.offset_right(settings),
        0.0,
    )
}

fn activate_existing_summon(
    chr: &mut ChrIns,
    position: HavokPosition,
    player_yaw: f32,
    config: AttackSummonConfig,
    settings: &AttackSummonSettings,
) {
    chr.remove_speffect(config.vanish_request_speffect(settings));
    prepare_summon(chr, config, settings);
    move_summon(chr, position, player_yaw);
    restore_summon_hp(chr);
    chr.modules.as_mut().event.as_mut().request_animation_id = -1;
    chr.modules.as_mut().event.as_mut().request_animation_id = config.animation_id;
}

fn move_summon(chr: &mut ChrIns, position: HavokPosition, yaw: f32) {
    let half_yaw = yaw * 0.5;
    let orientation = Quaternion(0.0, half_yaw.sin(), 0.0, half_yaw.cos());
    let physics = chr.modules.as_mut().physics.as_mut();
    physics.position = position;
    physics.last_update_position = position;
    physics.orientation = orientation;
    physics.interpolated_orientation = orientation;
    physics.orientation_euler = F32Vector4(0.0, yaw, 0.0, 0.0);
    chr.initial_position = position;
    chr.initial_orientation_euler = F32Vector4(0.0, yaw, 0.0, 0.0);
    chr.chunk_position = F32Vector4(position.0, position.1, position.2, 0.0);
    chr.chr_ctrl
        .as_mut()
        .chr_proxy_flags
        .set_rotation_sync_requested(true);
}

fn prepare_summon(chr: &mut ChrIns, config: AttackSummonConfig, settings: &AttackSummonSettings) {
    let marker_speffect = config.marker_speffect(settings);
    if !chr_has_speffect(chr, marker_speffect) {
        chr.apply_speffect(marker_speffect, true);
    }

    chr.team_type = config.generated_team_type(settings);
    chr.chr_ctrl
        .as_mut()
        .modifier
        .as_mut()
        .data
        .action_flags
        .set_disable_lock_on(config.disable_lock_on(settings));
    chr.last_hit_by = FieldInsHandle {
        selector: FieldInsSelector(u32::MAX),
        block_id: BlockId::none(),
    };
    chr.load_state.set_extinction_death(false);
    chr.chr_flags1c4.set_is_render_group_enabled(true);
    chr.chr_flags1c5.set_death_flag(false);
    chr.chr_flags1c5.set_enable_render(true);
    chr.chr_flags1c8.set_is_active(true);
    chr.chr_activation_flags.set_activation_enabled(true);
    let debug_flags = &mut chr.debug_flags;
    debug_flags.set_force_unloaded(false);
    debug_flags.set_force_loaded(true);
    debug_flags.set_character_disabled(false);
    debug_flags.set_disabled_updates(false);
    debug_flags.set_disabled_hit(false);
    debug_flags.set_disabled_movement(false);
    debug_flags.set_disabled_secondary_actions(false);
    chr.opacity_keyframes_multiplier = 1.0;
    chr.opacity_keyframes_multiplier_previous = 1.0;
    chr.tint_alpha_multiplier = 1.0;
    chr.tint_alpha_multiplier_modifier = 1.0;
    chr.camouflage_transparency = 1.0;
    chr.base_transparency = 1.0;
    chr.base_transparency_modifier = 1.0;
}

fn restore_summon_hp(chr: &mut ChrIns) {
    let data = chr.modules.as_mut().data.as_mut();
    data.hp = data.max_hp.max(1);
}

fn park_summon(
    world_chr_man: &mut WorldChrMan,
    summon: &mut AttackSummonInstance,
    settings: &AttackSummonSettings,
) {
    let Some(handle) = summon.handle else {
        summon.active = false;
        summon.pending_bind = false;
        return;
    };
    let Some(chr) = chr_by_handle_mut(world_chr_man, handle) else {
        summon.handle = None;
        summon.active = false;
        summon.pending_bind = false;
        return;
    };
    let config = settings.summons[summon.config_index];

    chr.remove_speffect(config.vanish_request_speffect(settings));
    restore_summon_hp(chr);
    chr.modules.as_mut().event.as_mut().request_animation_id = -1;
    chr.chr_flags1c5.set_death_flag(false);
    chr.chr_flags1c5.set_enable_render(false);
    chr.chr_flags1c8.set_is_active(true);
    let debug_flags = &mut chr.debug_flags;
    debug_flags.set_force_loaded(true);
    debug_flags.set_force_unloaded(false);
    debug_flags.set_character_disabled(false);
    debug_flags.set_disabled_updates(false);
    debug_flags.set_disabled_hit(true);
    debug_flags.set_disabled_movement(false);
    debug_flags.set_disabled_secondary_actions(false);
    chr.base_transparency = 0.0;
    chr.base_transparency_modifier = 0.0;
    chr.tint_alpha_multiplier = 0.0;
    chr.tint_alpha_multiplier_modifier = 0.0;
    summon.active = false;
    summon.pending_bind = false;
}

fn release_summon(world_chr_man: &mut WorldChrMan, summon: &AttackSummonInstance) {
    let Some(handle) = summon.handle else {
        return;
    };
    let Some(chr) = chr_by_handle_mut(world_chr_man, handle) else {
        return;
    };

    let data = chr.modules.as_mut().data.as_mut();
    data.hp = 0;
    chr.chr_flags1c5.set_death_flag(true);
    chr.chr_flags1c5.set_enable_render(false);
    chr.chr_flags1c8.set_is_active(false);
    let debug_flags = &mut chr.debug_flags;
    debug_flags.set_force_loaded(false);
    debug_flags.set_force_unloaded(true);
    debug_flags.set_character_disabled(true);
    debug_flags.set_disabled_updates(true);
    debug_flags.set_disabled_hit(true);
    debug_flags.set_disabled_movement(true);
    debug_flags.set_disabled_secondary_actions(true);
    chr.base_transparency = 0.0;
    chr.base_transparency_modifier = 0.0;
    chr.tint_alpha_multiplier = 0.0;
    chr.tint_alpha_multiplier_modifier = 0.0;
}

fn chr_dead(chr: &ChrIns) -> bool {
    let data = chr.modules.as_ref().data.as_ref();
    data.hp <= 0 || chr.chr_flags1c5.death_flag()
}

fn chr_has_speffect(chr: &ChrIns, sp_effect: i32) -> bool {
    chr.special_effect
        .entries()
        .any(|entry| entry.param_id == sp_effect)
}

fn chr_by_handle_mut(
    world_chr_man: &mut WorldChrMan,
    handle: FieldInsHandle,
) -> Option<&mut ChrIns> {
    if let Some(mut chr_ptr) = world_chr_man.debug_chr_creator.last_created_chr {
        let chr = unsafe { chr_ptr.as_mut() };
        if chr.field_ins_handle == handle {
            return Some(chr);
        }
    }

    world_chr_man.chr_ins_by_handle_mut(&handle)
}

fn default_marker_speffect() -> i32 {
    14200128
}

fn default_vanish_request_speffect() -> i32 {
    14200129
}

fn default_generated_team_type() -> u8 {
    47
}

fn default_max_active_summons() -> usize {
    8
}

fn default_start_delay_ms() -> u64 {
    5_000
}

fn default_unbound_timeout_ms() -> u64 {
    2000
}

fn default_fallback_lifetime_ms() -> i64 {
    10_000
}

fn default_offset_forward() -> f32 {
    2.8
}

fn default_offset_right() -> f32 {
    0.0
}

fn default_offset_up() -> f32 {
    0.05
}

fn default_same_trigger_cooldown_ms() -> u64 {
    350
}

fn default_any_trigger_cooldown_ms() -> u64 {
    120
}

fn default_reuse_spawned_units() -> bool {
    true
}

fn default_disable_lock_on() -> bool {
    false
}

fn default_destroy_delay_ms() -> u64 {
    0
}

fn default_register_event_entity_id() -> bool {
    true
}

fn default_log_enabled() -> bool {
    true
}

fn default_config_chr_id() -> i32 {
    4205
}

fn default_summons() -> Vec<AttackSummonConfig> {
    vec![
        AttackSummonConfig {
            trigger_speffect: 14200120,
            chr_id: 4205,
            chara_init_param_id: 0,
            npc_param_id: 42050100,
            npc_think_param_id: 42050000,
            event_entity_id: 0,
            talk_id: 0,
            animation_id: 20010,
            offset_right: Some(-1.25),
            is_player: false,
            marker_speffect: None,
            vanish_request_speffect: None,
            generated_team_type: None,
            fallback_lifetime_ms: None,
            offset_forward: None,
            offset_up: None,
            disable_lock_on: None,
        },
        AttackSummonConfig {
            trigger_speffect: 14200121,
            chr_id: 4205,
            chara_init_param_id: 0,
            npc_param_id: 42050101,
            npc_think_param_id: 42050000,
            event_entity_id: 0,
            talk_id: 0,
            animation_id: 20011,
            offset_right: Some(-0.75),
            is_player: false,
            marker_speffect: None,
            vanish_request_speffect: None,
            generated_team_type: None,
            fallback_lifetime_ms: None,
            offset_forward: None,
            offset_up: None,
            disable_lock_on: None,
        },
        AttackSummonConfig {
            trigger_speffect: 14200122,
            chr_id: 4205,
            chara_init_param_id: 0,
            npc_param_id: 42050102,
            npc_think_param_id: 42050000,
            event_entity_id: 0,
            talk_id: 0,
            animation_id: 20012,
            offset_right: Some(-0.25),
            is_player: false,
            marker_speffect: None,
            vanish_request_speffect: None,
            generated_team_type: None,
            fallback_lifetime_ms: None,
            offset_forward: None,
            offset_up: None,
            disable_lock_on: None,
        },
        AttackSummonConfig {
            trigger_speffect: 14200123,
            chr_id: 4205,
            chara_init_param_id: 0,
            npc_param_id: 42050103,
            npc_think_param_id: 42050000,
            event_entity_id: 0,
            talk_id: 0,
            animation_id: 20013,
            offset_right: Some(0.25),
            is_player: false,
            marker_speffect: None,
            vanish_request_speffect: None,
            generated_team_type: None,
            fallback_lifetime_ms: None,
            offset_forward: None,
            offset_up: None,
            disable_lock_on: None,
        },
        AttackSummonConfig {
            trigger_speffect: 14200124,
            chr_id: 4205,
            chara_init_param_id: 0,
            npc_param_id: 42050104,
            npc_think_param_id: 42050000,
            event_entity_id: 0,
            talk_id: 0,
            animation_id: 20017,
            offset_right: Some(0.75),
            is_player: false,
            marker_speffect: None,
            vanish_request_speffect: None,
            generated_team_type: None,
            fallback_lifetime_ms: None,
            offset_forward: None,
            offset_up: None,
            disable_lock_on: None,
        },
        AttackSummonConfig {
            trigger_speffect: 14200125,
            chr_id: 4205,
            chara_init_param_id: 0,
            npc_param_id: 42050105,
            npc_think_param_id: 42050000,
            event_entity_id: 0,
            talk_id: 0,
            animation_id: 20018,
            offset_right: Some(1.25),
            is_player: false,
            marker_speffect: None,
            vanish_request_speffect: None,
            generated_team_type: None,
            fallback_lifetime_ms: None,
            offset_forward: None,
            offset_up: None,
            disable_lock_on: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_lifetime_and_entry_overrides() {
        let settings: AttackSummonSettings = toml::from_str(
            r#"
fallback_lifetime_ms = -1
[[summons]]
trigger_speffect = 1
npc_param_id = 2
animation_id = 3
[[summons]]
trigger_speffect = 2
npc_param_id = 2
animation_id = 3
fallback_lifetime_ms = 10000
"#,
        )
        .unwrap();
        let now = Instant::now();
        assert_eq!(settings.summons[0].expires_at(&settings, now), None);
        assert_eq!(
            settings.summons[1].expires_at(&settings, now),
            Some(now + Duration::from_secs(10))
        );
        let mut finite = settings.clone();
        finite.fallback_lifetime_ms = 10000;
        finite.summons[0].fallback_lifetime_ms = Some(-1);
        assert_eq!(finite.summons[0].expires_at(&finite, now), None);
        finite.summons[0].fallback_lifetime_ms = Some(0);
        assert_eq!(
            finite.summons[0].expires_at(&finite, now),
            Some(now + Duration::from_millis(1))
        );
        assert_eq!(AttackSummonSettings::default().fallback_lifetime_ms, 10000);
    }

    #[test]
    fn unlimited_staged_removal_survives_cleared_trigger_and_inactive_state() {
        let now = Instant::now();
        let summon = AttackSummonInstance {
            config_index: 0,
            requested_at: now,
            expires_at: None,
            handle: None,
            active: false,
            pending_bind: false,
            destroy_at: Some(now + Duration::from_millis(100)),
        };
        assert!(summon.removal_started());
        assert!(!summon.destroy_at.is_some_and(|at| now >= at));
        assert!(
            summon
                .destroy_at
                .is_some_and(|at| now + Duration::from_millis(100) >= at)
        );
    }

    #[test]
    fn supports_only_the_pinned_fsrs_game_versions() {
        assert!(SupportedErVersion::from_lang_version(LANG_ID_EN, "2.7.1.0").is_some());
        assert!(SupportedErVersion::from_lang_version(LANG_ID_JP, "2.7.1.1").is_none());
        assert!(SupportedErVersion::from_lang_version(LANG_ID_EN, "2.8.0.0").is_none());
        assert!(SupportedErVersion::from_lang_version(LANG_ID_EN, "2.7.0.0").is_some());
        assert!(SupportedErVersion::from_lang_version(LANG_ID_JP, "2.7.0.1").is_some());
        assert!(SupportedErVersion::from_lang_version(LANG_ID_EN, "2.6.2.0").is_none());
        assert!(SupportedErVersion::from_lang_version(LANG_ID_JP, "2.6.2.1").is_none());
    }

    #[test]
    fn pinned_debug_creator_layout_matches_private_field_bridge() {
        assert_eq!(std::mem::size_of::<ChrIns>(), 0x580);
        assert_eq!(std::mem::offset_of!(ChrIns, debug_flags), 0x538);
        assert_eq!(std::mem::offset_of!(ChrIns, debug_role_param_id), 0x54c);
        assert_eq!(
            std::mem::size_of::<eldenring::cs::CSDebugChrCreatorInitData>(),
            DEBUG_CREATOR_INIT_DATA_SIZE
        );
        assert_eq!(DEBUG_CREATOR_SPAWN_ROTATION_OFFSET, 0x10);
        assert_eq!(DEBUG_CREATOR_ENEMY_TYPE_OFFSET, 0xc8);
    }

    /// Pins the offsets the lifecycle module was written against.
    ///
    /// Each value below comes from disassembling WW 2.7.1.0, so the assertions
    /// fail loudly if a future binding revision moves a field the removal path
    /// depends on. Native byte fixtures are retained under `tests/fixtures/`.
    #[test]
    fn pinned_chr_lifecycle_offsets_match_disassembly() {
        use eldenring::cs::{ChrSet, ChrSetEntry, PlayerIns};
        use std::mem::{offset_of, size_of};

        // ChrIns: vftable +0x00, handle +0x08, chr_set_entry +0x10, event id +0x1e8.
        assert_eq!(offset_of!(ChrIns, vftable), 0x00);
        assert_eq!(offset_of!(ChrIns, field_ins_handle), 0x08);
        assert_eq!(offset_of!(ChrIns, chr_set_entry), 0x10);
        assert_eq!(offset_of!(ChrIns, event_entity_id), 0x1e8);

        // ChrSet: capacity +0x10, entries +0x18, entity id map +0x28, group map +0x40.
        assert_eq!(offset_of!(ChrSet<ChrIns>, capacity), 0x10);
        assert_eq!(offset_of!(ChrSet<ChrIns>, entries), 0x18);
        assert_eq!(offset_of!(ChrSet<ChrIns>, entity_id_mapping), 0x28);
        assert_eq!(offset_of!(ChrSet<ChrIns>, group_id_mapping), 0x40);

        // ChrSetEntry is the 0x10 byte stride free_chr_list walks the array with.
        assert_eq!(size_of::<ChrSetEntry<ChrIns>>(), 0x10);
        assert_eq!(offset_of!(ChrSetEntry<ChrIns>, chr_ins), 0x00);
        assert_eq!(offset_of!(ChrSetEntry<ChrIns>, chr_load_status), 0x08);
        assert_eq!(offset_of!(ChrSetEntry<ChrIns>, chr_update_type), 0x09);
        assert_eq!(offset_of!(ChrSetEntry<ChrIns>, entry_flags), 0x0a);

        // DLMap: allocator +0x00, node head +0x08, size +0x10, so 0x18 bytes. The
        // map is the last member of ChrSet, so the tail of the structure measures
        // it without the private `eldenring::stl` module having to be nameable.
        assert_eq!(
            size_of::<ChrSet<ChrIns>>() - offset_of!(ChrSet<ChrIns>, group_id_mapping),
            0x18
        );

        // Lock-on has to be cleared before the character is freed.
        assert_eq!(offset_of!(PlayerIns, locked_on_enemy), 0x6b0);
    }

    #[test]
    fn accepts_legacy_position_field_names() {
        let settings = toml::from_str::<AttackSummonSettings>(
            r#"
spawn_forward_distance = 3.5
spawn_height_offset = 0.25

[[summons]]
trigger_speffect = 1
npc_param_id = 2
animation_id = 3
side_offset = -1.5
"#,
        )
        .expect("legacy position fields should remain valid");

        assert_eq!(settings.offset_forward, 3.5);
        assert_eq!(settings.offset_right, 0.0);
        assert_eq!(settings.offset_up, 0.25);
        assert_eq!(settings.summons[0].offset_right, Some(-1.5));
    }

    #[test]
    fn distributed_config_uses_valid_position_fields() {
        let settings = toml::from_str::<AttackSummonSettings>(include_str!("../summon.toml"))
            .expect("distributed summon.toml should remain valid");

        assert_eq!(settings.offset_forward, 2.8);
        assert_eq!(settings.offset_right, 0.0);
        assert_eq!(settings.offset_up, 0.05);
        assert_eq!(settings.summons.len(), 6);
    }
}
