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

`start_delay_ms = 5000` delays Summon's background initialization by five seconds after the DLL is loaded. This does not block `DllMain` or the game's main thread. Set it to `0` to initialize immediately.

Supported official executable versions:

- Worldwide `2.7.0.0`
- Japanese `2.7.0.1`

Modified or version-mismatched executables are not supported. On an unrecognized
version, Summon stops its background initialization before installing tasks or
accessing summon memory.

## Runtime Behavior

Trigger Speffects are consumed from the local player as one-shot requests. The generated unit appears relative to the player and camera, plays the configured animation, and ends when it receives the vanish Speffect, dies, disappears, or reaches its fallback lifetime.

New and reused units face the local player's current horizontal facing direction when activated. Position offsets are separate from facing: forward/right use the camera's horizontal local axes, while up uses the world vertical axis.

With `reuse_spawned_units = true`, the DLL keeps one hidden generated entity for each `[[summons]]` entry and reuses it on later triggers. This prevents repeated activations from continuously consuming the debug/enemy character pool.

## Configuration

Top-level settings provide defaults for every summon:

- `marker_speffect`
- `vanish_request_speffect`
- `generated_team_type`
- `max_active_summons`
- `start_delay_ms`
- `unbound_timeout_ms`
- `fallback_lifetime_ms`
- `offset_forward`
- `offset_right`
- `offset_up`
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
- `is_player`
- `marker_speffect`
- `vanish_request_speffect`
- `generated_team_type`
- `fallback_lifetime_ms`
- `offset_forward`
- `offset_right`
- `offset_up`
- `disable_lock_on`

The last eight fields are optional per-entry overrides. If omitted, the corresponding top-level value is used.

Position fields use readable local-axis names:

- `offset_forward`: positive moves toward the camera view direction; negative moves backward.
- `offset_right`: positive moves camera-right; negative moves camera-left.
- `offset_up`: positive moves upward; negative moves downward.

For compatibility, existing configuration files may still use `spawn_forward_distance`, `side_offset`, and `spawn_height_offset`. Do not define an old and new name for the same axis in one scope.

`disable_lock_on = true` disables lock-on for the whole generated character. It does not target one specific lock-on point ID.

See [summon.toml](summon.toml) for a complete working configuration.

## Build

Requirements:

- Rust toolchain with the `x86_64-pc-windows-msvc` target
- Visual Studio C++ build tools and Windows SDK

Build from the repository root:

```powershell
cargo build --release --locked
```

The repository does not vendor or use a Git submodule for `fromsoftware-rs`.
Cargo fetches both `eldenring` and `fromsoftware-shared` directly from the
upstream repository and pins them to this exact revision:

```text
https://github.com/vswarte/fromsoftware-rs.git
eae96dfec94fd9cf6f9d24813c8d08f72019f243
```

That revision contains the Elden Ring 1.17 RVA and runtime-structure updates.
`Cargo.lock` records the same full commit, so `--locked` builds cannot silently
move to a newer upstream revision. The first build needs network access to fetch
the pinned Git dependency; later builds can use Cargo's local cache.

Output:

```text
target/x86_64-pc-windows-msvc/release/summon.dll
```

Release builds statically link the MSVC CRT, so users do not need to install the Microsoft Visual C++ Redistributable separately for this DLL.

## License

Licensed under either Apache License 2.0 or MIT, at your option. The `eldenring`
and `fromsoftware-shared` Git dependencies retain their upstream license terms.

## Acknowledgements

Runtime bindings are derived from the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) project.
