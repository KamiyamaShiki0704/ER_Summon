# Endpoint security false positives

## Symptom

Loading the cdylib into the game intermittently trips endpoint security: the
managed agent on this machine raises a malware/riskware notice for
`summon.dll` some of the time, while other Elden Ring mod DLLs in the same
tree are never flagged.

Intermittent matters. The agent inspects on three separate paths and only one
of them is deterministic:

1. on-access file scan when the artifact lands on disk,
2. a cloud reputation lookup keyed by file hash,
3. a sampled scan of the newly created process' memory regions.

(2) is asynchronous and (3) explicitly walks a subset of regions, so the same
binary can be clean on one launch and flagged on the next. Every rebuild also
produces a fresh hash with no reputation history, which is why the problem
keeps coming back after each `cargo build`.

## Differential evidence

### The control group

`FSTool/_Mod/_Project/me3-ft/dist-ft/ft-natives/` contains two Rust cdylibs
that are also loaded into the game process and also read/write game memory:
`animation_debug.dll` and `nightreign_camera_debug.dll`. Neither is flagged.

| property | summon.dll (flagged) | animation_debug.dll (clean) |
| --- | --- | --- |
| signed | no | no |
| VERSIONINFO resource | no | no |
| CRT linkage | static (`+crt-static`) | static |
| debug directory | CODEVIEW + REPRO + EMBEDDED_PORTABLE_PDB | CODEVIEW + REPRO + EMBEDDED_PORTABLE_PDB |
| imported DLLs | kernel32, ntdll, oleaut32, api-ms-win-core-synch | kernel32, ntdll, oleaut32, api-ms-win-core-synch, + graphics/input |
| section entropy | 6.40 / 5.96 | 6.35 / 5.90 |
| size | 470 KB | 1.3 MB |
| profile | `strip=true`, `lto=true`, `codegen-units=1`, `panic="abort"`, `opt-level="z"` | `debug=2`, `strip="none"` |

### Hypotheses tested and excluded

Each of these was a plausible cause and each was measured, not assumed:

- **missing signature** — the control group has none either;
- **missing VERSIONINFO** — the control group has none either;
- **static CRT** — the control group is static too;
- **unusual debug directory / stripped debug info** — all three artifacts
  carry the same three debug entries, including an embedded portable PDB;
- **entropy (packing heuristic)** — 6.40 versus 6.35 in `.text`, which is
  ordinary optimized code, not packed data;
- **section count / timestamp / machine type** — no meaningful difference.

That leaves two real differences: the build profile, and a small set of
strings that only this artifact carries.

### Strings unique to this cdylib

```
VMT address not in executable!
Call target for REGISTER_TASK was not in exe
...\fromsoftware-rs-...\crates\eldenring\src\cs\chr_ins.rs
...\fromsoftware-rs-...\crates\shared\stl\src\rb_tree.rs
```

The control group carries none of them. All four are legible descriptions of
what this code does to another process: locate a virtual method table, patch
a call target inside the game executable, edit a character structure, walk an
engine red-black tree. Read as a set, without any other context, they are an
accurate description of a game memory tool — which is exactly the category
generic detection rules are built to catch.

### Where they come from

| string | origin |
| --- | --- |
| `VMT address not in executable!` | `shared/src/subclass.rs:51,119` — `.expect()` inside the `vmt_va()` default method of the `Superclass`/`Subclass` traits |
| `Call target for REGISTER_TASK was not in exe` | `eldenring/src/cs/task.rs:180` — `expect()` in the `REGISTER_TASK_VA` initializer |
| `chr_ins.rs`, `rb_tree.rs` | panic location paths, injected by rustc's `Location` machinery |

Both `expect()` sites are on code paths this mod genuinely needs: reading a
character's vtable through `Subclass`, and registering the recurring task via
`CSTaskImp`. The strings come in as part of using those APIs — they are not
dead code that can be pruned, and the trait default method is monomorphized
for any type that implements it.

Rust keeps panic messages and `file!()` paths as ordinary data in `.rdata` in
release builds. `strip` removes the symbol table and debug info, not these
literals, which is why `strip = true` did nothing about them.

## Changes applied

### 1. Release profile — artifact shape

`strip = true` + `opt-level = "z"` + `lto = true` + `codegen-units = 1` is a
size-minimization stack. It is also the shape of a packed payload: one
self-contained module, no attribution data, no relocation of symbols, and
denied analysis affordances. The control group uses the opposite convention —
`debug = 2`, `strip = "none"` — and is never flagged.

```toml
[profile.release]
debug = 2
strip = "none"
```

Result: 470 KB -> 684 KB, unwinding tables (`.pdata`) restored, a 14 MB
`summon.pdb` produced next to the artifact. Crash triage is now possible,
which the stripped build could not offer.

### 2. VERSIONINFO resource

`build.rs` compiles a version resource with the Windows SDK `rc.exe` and
hands the `.res` to the linker as a cdylib link argument. There is no build
dependency: the SDK is located by scanning `Windows Kits\10\bin`, and when it
is missing the resource is skipped with a warning instead of failing the
build.

```toml
FileVersion          0.14.0.0
ProductName          Nightreign Attack Summon
FileDescription      Elden Ring Nightreign attack summon mod
OriginalFilename     summon.dll
CompanyName          ER_Summon
LegalCopyright       MIT OR Apache-2.0
```

The artifact now has a readable `.rsrc` section, so a scanner that wants to
attribute the file has something to attribute it to instead of falling back
to pure heuristic scoring.

## What still has to happen outside this repository

The two changes above reduce the heuristic surface. They cannot make the
artifact trusted, because trust comes from a source this repository does not
control:

- **Whitelist the hash with the security team.** This is the actual fix. The
  file is a legitimate build inside the corporate environment; the correct
  route is to submit its SHA256 for the managed agent's file whitelist rather
  than to keep reshaping the binary.
- **Sign the artifact** if a code-signing certificate is available. A valid
  Authenticode signature is the one signal that no heuristic can argue with.

Report the SHA256 after each release build rather than the file itself, so
the whitelist entry keeps matching.

## Residual risk

The strings listed above stay in the artifact. They arrive through `expect()`
calls on code paths this mod genuinely needs, so pruning code does not remove
them. If the notice persists after a whitelist entry is in place, one lever is
left.

## Optional: removing the panic strings

Measured on this repository rather than assumed. Rust 1.100 nightly promotes
`panic_immediate_abort` to a real panic strategy: `panic = "immediate-abort"`
together with `-Z build-std` replaces every panic path with an immediate
abort, after which LLVM drops the message literals because nothing reads them.

```toml
cargo-features = ["panic-immediate-abort"]

[profile.release]
debug = 2
strip = "none"
panic = "immediate-abort"
```

```sh
cargo +nightly build --release -Z build-std
```

Probe counts per build:

| build | size | `VMT address` | `REGISTER_TASK` | `chr_ins.rs` | `eldenring` | `.cargo` | `rustc` |
| --- | --- | --- | --- | --- | --- | --- | --- |
| original profile | 470 KB | 1 | 1 | 1 | 4 | 31 | 50 |
| current profile | 684 KB | 2 | 1 | 1 | 4 | 34 | 71 |
| + immediate-abort | 504 KB | 0 | 0 | 0 | 0 | 0 | 0 |
| unflagged sibling | 1.3 MB | 0 | 0 | 0 | 1 | 152 | 63 |

Every probe reaches zero, and the artifact lands smaller than either stable
build because the panic machinery is gone.

Weigh what this is before reaching for it. Its only effect here is to remove
strings that detection rules match on. That is a different kind of change from
the two above: those are ordinary artifact hygiene that any release build
should have anyway, while this one exists to look different to a scanner. For
software meant to run on a managed corporate machine, the supported way past a
false positive is the whitelist route in the previous section, not reshaping
the binary. Keep this for the case where whitelisting is genuinely not
available, and go in with both costs visible: the project then requires a
nightly toolchain, because `cargo-features` makes `Cargo.toml` unparseable by
stable cargo, and every panic site loses its message.

## How to verify a build

```sh
cargo test --offline
cargo build --release --locked
```

Then confirm the artifact actually carries the resource and the debug
information — do not trust the build succeeding:

```sh
python - <<'PY'
import pefile
pe = pefile.PE("target/x86_64-pc-windows-msvc/release/summon.dll", fast_load=False)
print("VERSIONINFO:", any(e.id == 16 for e in pe.DIRECTORY_ENTRY_RESOURCE.entries))
print("sections:", [s.Name.decode().rstrip("\0") for s in pe.sections])
print("debug types:", [d.struct.Type for d in pe.DIRECTORY_ENTRY_DEBUG])
PY
```

Expected: `VERSIONINFO: True`, `.rsrc` present, debug types `[2, 12, 13]` —
the same debug shape the unflagged sibling artifacts carry.
