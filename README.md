# Summon

Standalone Elden Ring DLL that turns player Speffect notifications into temporary NPC attack summons. Each configured trigger can choose the model, NPCParam, NPCThinkParam, animation, placement, faction, lifetime, and cleanup behavior.

The default configuration is the validated `c4205` attack set previously shipped with `nightreign_style_hud`.

## Installation

Place both files in the same loader directory:

```text
Summon/
|-- summon.dll
`-- summon.toml
```

The DLL reads `summon.toml` once during startup. Missing or invalid values fall back to the built-in defaults, and configuration changes require a game restart.

This build targets the supported Elden Ring executable detected by the bundled `eldenring` bindings. Modified or version-mismatched executables are not supported.

## Default Speffects

| Speffect | NPCParam | NPCThinkParam | Animation |
| --- | ---: | ---: | ---: |
| `14200120` | `42050100` | `42050000` | `20010` |
| `14200121` | `42050101` | `42050000` | `20011` |
| `14200122` | `42050102` | `42050000` | `20012` |
| `14200123` | `42050103` | `42050000` | `20013` |
| `14200124` | `42050104` | `42050000` | `20017` |
| `14200125` | `42050105` | `42050000` | `20018` |

- `14200128` is the default marker applied to generated units.
- `14200129` is the default vanish request. Apply it near the animation end.
- Generated units use model `c4205`, CharaInit `0`, and team/type `47` by default.

## Runtime Behavior

Trigger Speffects are consumed from the local player as one-shot requests. The generated unit appears relative to the player and camera, plays the configured animation, and ends when it receives the vanish Speffect, dies, disappears, or reaches its fallback lifetime.

With `reuse_spawned_units = true`, the DLL keeps one hidden generated entity for each `[[summons]]` entry and reuses it on later triggers. This prevents repeated activations from continuously consuming the debug/enemy character pool.

## Configuration

Top-level settings provide defaults for every summon:

- `marker_speffect`
- `vanish_request_speffect`
- `generated_team_type`
- `max_active_summons`
- `unbound_timeout_ms`
- `fallback_lifetime_ms`
- `spawn_forward_distance`
- `spawn_height_offset`
- `same_trigger_cooldown_ms`
- `any_trigger_cooldown_ms`
- `reuse_spawned_units`
- `disable_lock_on`

Each `[[summons]]` entry supports:

- `trigger_speffect`
- `chr_id`
- `chara_init_param_id`
- `npc_param_id`
- `npc_think_param_id`
- `event_entity_id`
- `talk_id`
- `animation_id`
- `side_offset`
- `is_player`
- `marker_speffect`
- `vanish_request_speffect`
- `generated_team_type`
- `fallback_lifetime_ms`
- `spawn_forward_distance`
- `spawn_height_offset`
- `disable_lock_on`

The last seven fields are optional per-entry overrides. If omitted, the corresponding top-level value is used.

`disable_lock_on = true` disables lock-on for the whole generated character. It does not target one specific lock-on point ID.

See [summon.toml](summon.toml) for a complete working configuration.

## Build

Requirements:

- Rust toolchain with the `x86_64-pc-windows-msvc` target
- Visual Studio C++ build tools and Windows SDK

Build from the repository root:

```powershell
cargo build --release
```

Output:

```text
target/x86_64-pc-windows-msvc/release/summon.dll
```

Release builds statically link the MSVC CRT, so users do not need to install the Microsoft Visual C++ Redistributable separately for this DLL.

## License

Licensed under either Apache License 2.0 or MIT, at your option. The bundled `eldenring` and `fromsoftware-shared` bindings retain their upstream license terms.

## Acknowledgements

Runtime bindings are derived from the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) project.
