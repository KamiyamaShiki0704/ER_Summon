# Summon

[中文](README.md) | English

A standalone summon DLL for Elden Ring. SpEffects applied to the local player trigger NPC summons with configurable models, parameters, animations, teams, placement, lifetimes, and event entity IDs.

## Installation

Download the ZIP from Releases and place both files in the same DLL loader directory:

```text
Summon/
|-- summon.dll
`-- summon.toml
```

Back up existing custom configuration before updating. Configuration is read at startup; restart the game after editing it.

## Compatibility

The current entity registration and memory release features have been tested on Worldwide `2.7.1.0` (App Ver. 1.17.1).
The initialization version check also recognizes Worldwide `2.7.0.0` and Japanese `2.7.0.1`, but the new native registration and release paths have not been tested on those versions. Operations are refused when native function verification fails; passing the startup version check does not guarantee equivalent functionality.

## Configuration

See [summon.toml](summon.toml) for the complete default configuration. Global settings must appear before the first `[[summons]]` entry.

```toml
fallback_lifetime_ms = -1
remove_mode = "destroy"
reuse_spawned_units = false
vanish_request_speffect = 14200129
log_enabled = true
```

- `fallback_lifetime_ms = -1`: unlimited lifetime. In a valid loaded map, removal begins when the unit dies or receives the configured vanish SpEffect.
- Nonnegative lifetimes are in milliseconds; `0` uses a minimum of 1 ms. The built-in default is 10000 ms; the bundled configuration selects `-1`.
- `remove_mode = "destroy"`: unregisters character tasks, detaches the character from its set, and waits for reference checks before destruction and memory release. Units are not reused in this mode.
- `remove_mode = "hide"`: hides the character while keeping it resident. Set `reuse_spawned_units = true` to reuse it.
- `destroy_delay_ms`: minimum delay before frame-end detachment. Actual deallocation also waits for reference checks to pass.
- `vanish_request_speffect`: the disappearance effect applied to the summoned unit itself, separate from the summon trigger applied to the player.

Unlimited lifetime does not preserve summons across map changes. During map transitions or world/player resets, detached objects awaiting release may remain allocated until the process exits. Releasing a character instance does not guarantee unloading model caches or its projectiles.

### Summon entries

Each `[[summons]]` entry defines a summon type.

| Field | Purpose |
| --- | --- |
| `trigger_speffect` | Trigger applied to the local player and consumed as a one-shot request |
| `chr_id` | Character model number, e.g. 4205 for c4205 |
| `chara_init_param_id` | CharaInitParam ID |
| `npc_param_id` / `npc_think_param_id` | NPC and AI parameter IDs |
| `event_entity_id` | Entity ID used by event scripts; 0 skips registration |
| `animation_id` | Animation requested on activation |
| `talk_id` / `is_player` | Talk ID and character creation type |
| `offset_forward` | Camera-relative horizontal offset; positive is forward |
| `offset_right` | Camera-relative horizontal offset; positive is right |
| `offset_up` | World vertical offset; positive is upward |

Entries can override the global `marker_speffect`, `vanish_request_speffect`, `generated_team_type`, `fallback_lifetime_ms`, all three position offsets, and `disable_lock_on`. Omitted overrides inherit the global value.

Activated units face the player's current horizontal facing direction. Position offsets use the camera orientation and are calculated separately.
`disable_lock_on = true` disables lock-on for the entire summoned character.
Legacy position names `spawn_forward_distance`, `side_offset`, and `spawn_height_offset` remain accepted. Do not specify both the old and new names for an axis in the same scope.

### Event entity IDs

Set an unused positive ID on an entry that needs event control:

```toml
# Inside the relevant [[summons]] entry
event_entity_id = 13000890
```

This number is only an example; confirm it is unused by your own maps and events. Registration covers the character field, entity mapping, and native group registration.

Example EMEVD command:

```js
ForceAnimationPlayback(13000890, 20012, false, false, false);
```

Simultaneously active units that need separate control should use different IDs. Registration is refused if another character in the same character set already owns the ID. Once the original unit finishes cleanup, the ID can be used again.

### Other global settings

- `max_active_summons`: maximum active summons.
- `start_delay_ms`: initialization delay after DLL loading; defaults to 5000 ms.
- `unbound_timeout_ms`: timeout for a creation request to bind to a character.
- `same_trigger_cooldown_ms` / `any_trigger_cooldown_ms`: per-entry and shared trigger cooldowns.
- `marker_speffect`: effect marking generated units.
- `generated_team_type`: team assigned to generated units.

### Logging

With `log_enabled = true`, Summon writes `summon.log` next to the DLL.

- `entity_id_outcome=conflict`: the entity ID is already occupied.
- `group_after=1`: the group mapping for this ID/character has been established.
- `local_set_id_resolves_to_this_unit=true`: the local character-set mapping resolves to the unit. This does not mean every event instruction has been verified.
- `outcome=destroyed retirement=frame-end-drained`: final character deallocation has returned.

## Building

Requires Rust, the `x86_64-pc-windows-msvc` target, and Visual Studio C++ build tools.

```powershell
cargo test --locked
cargo build --release --locked
```

Output: `target/x86_64-pc-windows-msvc/release/summon.dll`.
Release builds statically link the MSVC CRT and retain debug information. With the Windows SDK installed, `build.rs` can embed a version resource. If `rc.exe` is unavailable, the resource is skipped with a warning.

Cargo fetches the dependencies at this pinned revision; the first build requires network access:

```text
https://github.com/KamiyamaShiki0704/fromsoftware-rs
02fa5681e27fd2ecd9da79d34aef9fae96805539
```

## License and acknowledgements

Licensed under MIT or Apache-2.0. Runtime bindings are based on [fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs). Dependencies retain their own license terms.
