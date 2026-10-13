mod creation;
mod host;
#[cfg(test)]
mod native_guards;
mod presentation;
mod retirement;
use serde::Deserialize;
use std::{
    ffi::{OsString, c_void},
    fs,
    io::Write,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, OnceLock,
        atomic::{AtomicIsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HINSTANCE, HMODULE},
    System::LibraryLoader::GetModuleFileNameW,
};
use windows::core::BOOL;
const CONFIG_FILE_NAME: &str = "summon.toml";
const LOG_FILE_NAME: &str = "summon.log";
static DLL_MODULE: AtomicIsize = AtomicIsize::new(0);
static SETTINGS: OnceLock<AttackSummonSettings> = OnceLock::new();
static LOG: LazyLock<Mutex<Option<fs::File>>> = LazyLock::new(|| Mutex::new(None));
pub(crate) fn log_line(message: impl std::fmt::Display) {
    if !settings().log_enabled {
        return;
    }
    let Ok(mut guard) = LOG.lock() else { return };
    if guard.is_none() {
        let Some(path) = module_dir().map(|d| d.join(LOG_FILE_NAME)) else {
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
/// # Safety
/// Windows loader supplies the process module and standard loader arguments. Actual game
/// registration runs outside loader lock; the resident SDK pins all installed callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(module: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == 1 {
        DLL_MODULE.store(module.0 as isize, Ordering::Relaxed);
        thread::spawn(|| {
            thread::sleep(settings().start_delay());
            if let Err(e) = host::install() {
                log_line(format_args!("summon install rejected: {e}"));
            }
        });
    }
    BOOL(1)
}
#[derive(Clone, Copy)]
struct HavokPosition(f32, f32, f32, f32);
fn yaw_from_orientation([qx, qy, qz, qw]: [f32; 4]) -> f32 {
    let x = 2.0 * (qx * qz + qw * qy);
    let z = 1.0 - 2.0 * (qx * qx + qy * qy);
    x.atan2(z)
}
/// What happens to a summon that has to disappear.
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RemoveMode {
    /// Only field writes: the unit stops rendering and updating but stays resident.
    Hide,
    /// Real removal: the event id is released, the engine destructor runs and the
    /// character set entry is cleared, so nothing of the unit is left in memory.
    #[default]
    Destroy,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OffsetSpace {
    #[default]
    Camera,
    World,
}

#[derive(Clone, Deserialize)]
pub(crate) struct AttackSummonSettings {
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
    #[serde(default)]
    offset_space: OffsetSpace,
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
            offset_space: OffsetSpace::default(),
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
    parse_settings(&text)
}

fn parse_settings(text: &str) -> AttackSummonSettings {
    let Ok(mut settings) = toml::from_str::<AttackSummonSettings>(text) else {
        return AttackSummonSettings::default();
    };
    settings.summons.retain(|config| {
        config.trigger_speffect > 0 && config.chr_id >= 0 && config.npc_param_id > 0
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
pub(crate) struct AttackSummonConfig {
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
    #[serde(default)]
    offset_space: Option<OffsetSpace>,
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
    fn offset_space(&self, settings: &AttackSummonSettings) -> OffsetSpace {
        self.offset_space.unwrap_or(settings.offset_space)
    }

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

fn character_model_name(chr_id: i32) -> String {
    format!("c{:0>4}", chr_id)
}

fn uses_player_creation(chr_id: i32, is_player: bool) -> bool {
    // c0000 NPCs share the player-character creation path; this does not
    // designate them as the local player or replace the main_player pointer.
    chr_id == 0 || is_player
}

fn summon_position(
    player_pos: HavokPosition,
    forward: (f32, f32, f32),
    right: (f32, f32, f32),
    config: AttackSummonConfig,
    settings: &AttackSummonSettings,
) -> HavokPosition {
    let (forward, right) = match config.offset_space(settings) {
        OffsetSpace::Camera => (forward, right),
        OffsetSpace::World => ((0.0, 0.0, 1.0), (1.0, 0.0, 0.0)),
    };
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

fn normalize_horizontal(vector: (f32, f32, f32), fallback: (f32, f32, f32)) -> (f32, f32, f32) {
    let len = (vector.0 * vector.0 + vector.2 * vector.2).sqrt();
    if len <= 0.001 || !len.is_finite() {
        return fallback;
    }
    (vector.0 / len, 0.0, vector.2 / len)
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
            offset_space: None,
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
            offset_space: None,
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
            offset_space: None,
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
            offset_space: None,
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
            offset_space: None,
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
            offset_space: None,
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
    fn world_offsets_ignore_camera_axes_and_camera_offsets_rotate() {
        let mut settings = AttackSummonSettings {
            offset_forward: 3.0,
            offset_right: -2.0,
            offset_up: 0.5,
            ..Default::default()
        };
        let mut config = settings.summons[0];
        config.offset_right = None;
        let origin = HavokPosition(10.0, 20.0, 30.0, 0.0);
        let rotated = ((1.0, 0.0, 0.0), (0.0, 0.0, -1.0));
        let standard = ((0.0, 0.0, 1.0), (1.0, 0.0, 0.0));
        let coords = |p: HavokPosition| (p.0, p.1, p.2);
        assert_eq!(config.offset_space(&settings), OffsetSpace::Camera);
        assert_eq!(
            coords(summon_position(
                origin, rotated.0, rotated.1, config, &settings
            )),
            (13.0, 20.5, 32.0)
        );
        settings.offset_space = OffsetSpace::World;
        for axes in [rotated, standard] {
            assert_eq!(
                coords(summon_position(origin, axes.0, axes.1, config, &settings)),
                (8.0, 20.5, 33.0)
            );
        }
        config.offset_space = Some(OffsetSpace::Camera);
        assert_eq!(
            coords(summon_position(
                origin, rotated.0, rotated.1, config, &settings
            )),
            (13.0, 20.5, 32.0)
        );
        config.offset_space = Some(OffsetSpace::World);
        config.offset_forward = Some(-4.0);
        config.offset_right = Some(5.0);
        config.offset_up = Some(-1.0);
        settings.offset_space = OffsetSpace::Camera;
        assert_eq!(
            coords(summon_position(
                origin, rotated.0, rotated.1, config, &settings
            )),
            (15.0, 19.0, 26.0)
        );
    }

    #[test]
    fn offset_space_parses_global_entry_override_and_legacy_default() {
        let text = r#"
offset_space = "world"
[[summons]]
trigger_speffect = 1
npc_param_id = 2
animation_id = -1
[[summons]]
trigger_speffect = 2
npc_param_id = 3
animation_id = -1
offset_space = "camera"
"#;
        let settings: AttackSummonSettings = toml::from_str(text).unwrap();
        assert_eq!(
            settings.summons[0].offset_space(&settings),
            OffsetSpace::World
        );
        assert_eq!(
            settings.summons[1].offset_space(&settings),
            OffsetSpace::Camera
        );
        let legacy: AttackSummonSettings =
            toml::from_str(&text.replace("offset_space = \"world\"", "")).unwrap();
        assert_eq!(legacy.summons[0].offset_space(&legacy), OffsetSpace::Camera);
        assert!(
            toml::from_str::<AttackSummonSettings>(&text.replace("\"world\"", "\"wrold\""))
                .is_err()
        );
    }

    #[test]
    fn animation_fade_survives_summon_binding_and_updates() {
        for phase in [presentation::Phase::Binding, presentation::Phase::Updating] {
            let input = host::presentation_input_for_test(phase);
            assert!(
                !presentation::plan(&input)
                    .unwrap()
                    .iter()
                    .any(|s| matches!(s, presentation::Step::Opacity(_)))
            );
        }
    }

    #[test]
    fn hidden_reactivation_restores_only_fields_changed_by_parking() {
        let mut input = host::presentation_input_for_test(presentation::Phase::Reactivating);
        input.visible_opacity = [0.0, -1.0, 0.6, 0.0];
        assert!(
            presentation::plan(&input)
                .unwrap()
                .contains(&presentation::Step::Opacity(input.visible_opacity))
        );
    }

    #[test]
    fn humanoid_config_survives_runtime_filter_and_uses_player_creation() {
        let settings = parse_settings(
            r#"
[[summons]]
trigger_speffect = 3269
chr_id = 0
chara_init_param_id = 23090
npc_param_id = 523090010
npc_think_param_id = 523090010
animation_id = -1
[[summons]]
trigger_speffect = 14200120
chr_id = 4205
npc_param_id = 42050100
animation_id = 20010
[[summons]]
trigger_speffect = 99
chr_id = -1
npc_param_id = 1
animation_id = -1
"#,
        );
        assert_eq!(settings.summons.len(), 2);
        let human = settings.summons[0];
        assert_eq!(character_model_name(human.chr_id), "c0000");
        assert!(uses_player_creation(human.chr_id, human.is_player));
        assert_eq!(human.chara_init_param_id, 23090);
        assert_eq!(human.npc_param_id, 523090010);
        assert_eq!(human.npc_think_param_id, 523090010);
        assert_eq!(human.animation_id, -1);
        let enemy = settings.summons[1];
        assert_eq!(character_model_name(enemy.chr_id), "c4205");
        assert!(!uses_player_creation(enemy.chr_id, enemy.is_player));
        assert!(uses_player_creation(enemy.chr_id, true));
    }

    #[test]
    fn spawn_animation_minus_one_does_not_touch_pending_request() {
        for (phase, reused) in [
            (presentation::Phase::Binding, false),
            (presentation::Phase::Reactivating, true),
        ] {
            let mut input = host::presentation_input_for_test(phase);
            input.animation = -1;
            assert!(
                !presentation::plan(&input)
                    .unwrap()
                    .iter()
                    .any(|s| matches!(s, presentation::Step::Animation { .. }))
            );
            input.animation = 20012;
            assert!(
                presentation::plan(&input)
                    .unwrap()
                    .contains(&presentation::Step::Animation { id: 20012, reused })
            );
        }
    }

    #[test]
    fn summon_team_override_and_no_animation_parse_together() {
        let settings: AttackSummonSettings = toml::from_str(
            r#"
generated_team_type = 6
[[summons]]
trigger_speffect = 1
npc_param_id = 2
animation_id = -1
generated_team_type = 26
[[summons]]
trigger_speffect = 2
npc_param_id = 3
animation_id = -1
"#,
        )
        .unwrap();
        assert_eq!(settings.summons[0].generated_team_type(&settings), 26);
        assert_eq!(settings.summons[1].generated_team_type(&settings), 6);
        assert_eq!(settings.summons[0].animation_id, -1);
        assert_eq!(settings.summons[1].animation_id, -1);
        assert_eq!(AttackSummonSettings::default().generated_team_type, 47);
    }

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
        let deadline = Some(now + Duration::from_millis(100));
        assert!(!host::retirement_due(deadline, now));
        assert!(host::retirement_due(
            deadline,
            now + Duration::from_millis(100)
        ));
    }

    #[test]
    fn supports_only_the_pinned_fsrs_game_versions() {
        assert_eq!(shikifsrs_elden_ring::WW_171.file_size, 87042128);
        assert!(shikifsrs_elden_ring::Ww171File::verify(&[]).is_err());
        assert_eq!(
            shikifsrs_elden_ring::WW_171.name,
            "Elden Ring WW 1.17.1 / EXE 2.7.1.0"
        );
    }

    #[test]
    fn pinned_debug_creator_layout_matches_private_field_bridge() {
        native_guards::assert_creation_guard();
    }

    /// Pins the offsets the lifecycle module was written against.
    ///
    /// Each value below comes from disassembling WW 2.7.1.0, so the assertions
    /// fail loudly if a future binding revision moves a field the removal path
    /// depends on. Native byte fixtures are retained under `tests/fixtures/`.
    #[test]
    fn pinned_chr_lifecycle_offsets_match_disassembly() {
        native_guards::assert_retirement_guards();
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
