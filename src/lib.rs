use std::{
    ffi::{OsString, c_void},
    fs,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, Once, OnceLock,
        atomic::{AtomicIsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use eldenring::{
    cs::{
        CSCamExt, CSCamera, CSTaskGroupIndex, CSTaskImp, ChrDebugSpawnRequest, ChrIns, ChrInsExt,
        FieldInsHandle, WorldChrMan,
    },
    fd4::FD4TaskData,
    position::HavokPosition,
    rotation::Quaternion,
    util::system::wait_for_system_init,
};
use fromsoftware_shared::{F32Vector4, FromStatic, SharedTaskImpExt, program::Program};
use serde::Deserialize;
use windows::Win32::{
    Foundation::{HINSTANCE, HMODULE},
    System::LibraryLoader::GetModuleFileNameW,
};

const DLL_PROCESS_ATTACH: u32 = 1;
const CONFIG_FILE_NAME: &str = "summon.toml";

static INSTALL_TASKS: Once = Once::new();
static DLL_MODULE: AtomicIsize = AtomicIsize::new(0);
static SETTINGS: OnceLock<AttackSummonSettings> = OnceLock::new();
static STATE: LazyLock<Mutex<AttackSummonState>> =
    LazyLock::new(|| Mutex::new(AttackSummonState::default()));

#[unsafe(no_mangle)]
/// # Safety
pub unsafe extern "C" fn DllMain(hmodule: HINSTANCE, reason: u32, _: *mut c_void) -> i32 {
    DLL_MODULE.store(hmodule.0 as isize, Ordering::Relaxed);
    if reason != DLL_PROCESS_ATTACH {
        return 1;
    }

    thread::spawn(|| {
        thread::sleep(settings().start_delay());
        if wait_for_system_init(&Program::current(), Duration::MAX).is_ok() {
            install_tasks_once();
        }
    });

    1
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
    });
}

fn attack_summon_task(_: &FD4TaskData) {
    let settings = settings();
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance_mut() }) else {
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

    let Some((player_pos, player_yaw, triggers)) = consume_player_triggers(world_chr_man, settings)
    else {
        state.release_all(world_chr_man);
        return;
    };
    if triggers.is_empty() {
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
    fallback_lifetime_ms: u64,
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
    fallback_lifetime_ms: Option<u64>,
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

    fn fallback_lifetime(&self, settings: &AttackSummonSettings) -> Duration {
        Duration::from_millis(
            self.fallback_lifetime_ms
                .unwrap_or(settings.fallback_lifetime_ms)
                .max(1),
        )
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
    expires_at: Instant,
    handle: Option<FieldInsHandle>,
    active: bool,
    pending_bind: bool,
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

        if settings.reuse_spawned_units {
            if let Some(index) = self
                .summons
                .iter()
                .position(|summon| summon.config_index == config_index)
            {
                if self.summons[index].active || self.summons[index].pending_bind {
                    return;
                }

                if let Some(handle) = self.summons[index].handle {
                    if let Some(chr) = chr_by_handle_mut(world_chr_man, handle) {
                        activate_existing_summon(chr, position, player_yaw, config, settings);
                        self.summons[index].requested_at = now;
                        self.summons[index].expires_at = now + config.fallback_lifetime(settings);
                        self.summons[index].active = true;
                        self.summons[index].pending_bind = false;
                        return;
                    }
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
        world_chr_man.spawn_debug_character(&ChrDebugSpawnRequest {
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
        });

        self.summons.push(AttackSummonInstance {
            config_index,
            requested_at: now,
            expires_at: now + config.fallback_lifetime(settings),
            handle: None,
            active: true,
            pending_bind: true,
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
                if !summon.active && !summon.pending_bind {
                    false
                } else if summon.handle.is_none() {
                    now.duration_since(summon.requested_at) >= settings.unbound_timeout()
                } else {
                    summon_should_disappear(world_chr_man, summon, now, settings)
                }
            };

            if should_remove {
                if settings.reuse_spawned_units && self.summons[index].handle.is_some() {
                    let summon = &mut self.summons[index];
                    park_summon(world_chr_man, summon, settings);
                    index += 1;
                } else {
                    let summon = self.summons.swap_remove(index);
                    release_summon(world_chr_man, &summon);
                }
            } else {
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
        self.summons[summon_index].handle = Some(chr.field_ins_handle);
        self.summons[summon_index].active = true;
        self.summons[summon_index].pending_bind = false;
        world_chr_man.debug_chr_creator.last_created_chr = None;
    }

    fn release_all(&mut self, world_chr_man: &mut WorldChrMan) {
        for summon in &self.summons {
            release_summon(world_chr_man, summon);
        }
        self.summons.clear();
    }
}

fn summon_should_disappear(
    world_chr_man: &mut WorldChrMan,
    summon: &AttackSummonInstance,
    now: Instant,
    settings: &AttackSummonSettings,
) -> bool {
    if now >= summon.expires_at {
        return true;
    }
    let config = settings.summons[summon.config_index];

    let Some(handle) = summon.handle else {
        return false;
    };
    let Some(chr) = chr_by_handle_mut(world_chr_man, handle) else {
        return true;
    };

    if chr_dead_or_disabled(chr) || chr_has_speffect(chr, config.vanish_request_speffect(settings))
    {
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
    chr.last_hit_by = FieldInsHandle::none();
    chr.load_state.set_extinction_death(false);
    chr.chr_flags1c4.set_is_render_group_enabled(true);
    chr.chr_flags1c5.set_death_flag(false);
    chr.chr_flags1c5.set_enable_render(true);
    chr.chr_flags1c8.set_is_active(true);
    chr.chr_activation_flags.set_activation_enabled(true);
    let debug_flags = chr.debug_flags_mut();
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
    let debug_flags = chr.debug_flags_mut();
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
    let debug_flags = chr.debug_flags_mut();
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

fn chr_dead_or_disabled(chr: &ChrIns) -> bool {
    let data = chr.modules.as_ref().data.as_ref();
    data.hp <= 0 || chr.chr_flags1c5.death_flag() || chr.debug_flags().character_disabled()
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

fn default_fallback_lifetime_ms() -> u64 {
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
