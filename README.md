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
- Worldwide `2.7.1.0` (App Ver. 1.17.1)
- Japanese `2.7.0.1`

Modified or version-mismatched executables are not supported. On an unrecognized
version, Summon stops its background initialization before installing tasks or
accessing summon memory.

## Runtime Behavior

Trigger Speffects are consumed from the local player as one-shot requests. The generated unit appears relative to the player and camera, plays the configured animation, and ends when it receives the vanish Speffect, dies, disappears, or reaches its fallback lifetime.

New and reused units face the local player's current horizontal facing direction when activated. Position offsets are separate from facing: forward/right use the camera's horizontal local axes, while up uses the world vertical axis.

### Entity ids

`event_entity_id` is the id event scripts use to find and control a generated unit. `0` leaves a unit reachable only through its handle. For any other value Summon writes the id onto the character and registers it in the owning character set, so resolving the id leads back to that unit. An id another character in the same character set already owns is refused rather than stolen.

Whether a given number is free depends on your own event id space, so Summon reports the outcome instead of guessing: `summon.log` records what every bind settled on, and warns when a configured id does not resolve back to its unit.

### Removal

`remove_mode` decides how a unit leaves the world.

- `"destroy"` (default): the unit releases its event ids, runs the engine destructor and clears its character set entry, so it is really gone from memory instead of resident-but-hidden. Unit reuse is impossible by definition here, so it overrides `reuse_spawned_units`.
- `"hide"`: only field writes. The unit stays resident and can be reused, which is the previous behavior.

Removal is staged: a unit is first made inert and has its ids released, and is freed at least one tick later, so no other system is mid-iteration over it when it goes. `destroy_delay_ms` extends that gap. Because Summon clears the character set entry as its last step, a later engine-side teardown of the character set sees an empty slot and skips it rather than freeing the same address twice.

With `reuse_spawned_units = true` and `remove_mode = "hide"`, the DLL keeps one hidden generated entity for each `[[summons]]` entry and reuses it on later triggers. This prevents repeated activations from continuously consuming the debug/enemy character pool.

### Log

`log_enabled = true` appends lifecycle evidence to `summon.log` next to the DLL: the startup summary, one `summon bound` line per activation, and one line per staged and completed removal. Each line carries the counts needed to check the claims without a debugger — the entity id outcome, whether the id resolves back to the unit, and the character set's id count before and after a removal.

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
- `remove_mode`
- `destroy_delay_ms`
- `register_event_entity_id`
- `log_enabled`

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

See [summon.toml](summon.toml) for a complete working configuration, and [docs/compatibility/engine-chr-lifecycle.md](docs/compatibility/engine-chr-lifecycle.md) for the disassembly evidence behind the entity id and removal behavior.

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
https://github.com/KamiyamaShiki0704/fromsoftware-rs
02fa5681e27fd2ecd9da79d34aef9fae96805539
```

That fork revision preserves the validated Elden Ring 1.17 character layout
and adds the five verified WW 1.17.1 RVA changes. Japanese 1.17.1 is not yet
verified; Japanese 1.17.0 remains supported.
`Cargo.lock` records the same full commit, so `--locked` builds cannot silently
move to a newer upstream revision. The first build needs network access to fetch
the pinned Git dependency; later builds can use Cargo's local cache.

Output:

```text
target/x86_64-pc-windows-msvc/release/summon.dll
```

Release builds statically link the MSVC CRT, so users do not need to install the Microsoft Visual C++ Redistributable separately for this DLL.

### Artifact shape

Release builds deliberately keep debug information and symbols (`debug = 2`,
`strip = "none"`) and carry a VERSIONINFO resource generated by `build.rs`.
This is not cosmetic. A stripped, size-minimized cdylib that gets loaded into
another process and rewrites its memory is the shape endpoint security
heuristics are built around, and the stripped build was flagged
intermittently by the managed agent on this machine. Keeping the symbols also
makes crash triage possible, which the stripped build could not offer, and the
version resource gives a scanner something to attribute the file to instead of
falling back to pure heuristic scoring.

`build.rs` embeds the resource only when it can find the Windows SDK `rc.exe`
(it scans `Windows Kits\10\bin`, or takes an explicit `RC` environment
variable). When it cannot be found the resource is skipped with a warning and
the build still succeeds, so the SDK is not a hard build requirement.

See [docs/compatibility/av-false-positive.md](docs/compatibility/av-false-positive.md)
for the differential evidence against the sibling mod DLLs that are never
flagged, the hypotheses that were measured and excluded, and what has to
happen outside this repository before the artifact is actually trusted.

[docs/handover.md](docs/handover.md) summarises the 2026-09-22 changes as a
whole: the event entity id work, the real removal path, and the artifact-shape
changes above, together with what is still unverified.

## License

Licensed under either Apache License 2.0 or MIT, at your option. The `eldenring`
and `fromsoftware-shared` Git dependencies retain their upstream license terms.

## Acknowledgements

Runtime bindings are derived from the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) project.
