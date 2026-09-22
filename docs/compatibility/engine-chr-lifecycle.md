# Character lifecycle in the engine

Date: 2026-09-22. Target: standalone Summon.
Input executable: WW 2.7.1.0 at
`G:/SteamLibrary/steamapps/common/ELDEN RING/Game/eldenring.exe`.
SHA256: `1A3547101327F65D0C76DA2F9190AC0AA66871EA42BAE2AECC61E11A8B597891`.

The hash is identical to the one in [er-1171.md](er-1171.md); only that document's
install path was stale (`F:` drive, no longer present). Every conclusion recorded
there still refers to this same binary.

This note answers two questions the mod cannot answer from bindings alone:

1. Can a generated unit be given a real event entity id that event scripts resolve?
2. Is there an engine path that removes a single unit from memory?

Re-runnable evidence: `research/re_chrset.py`, `research/re_chrins_vmt.py`,
`research/re_disasm.py`, `research/re_allocator.py`. Run them with an interpreter
that has `pefile` and `capstone`.

## Offsets

Verified twice, independently: once by disassembly, once by the pinned
`fromsoftware-rs` revision's own field layout. `pinned_chr_lifecycle_offsets_match_disassembly`
fails if the two ever disagree.

| Structure | Field | Offset |
| --- | --- | --- |
| `ChrIns` | `vftable` | 0x00 |
| `ChrIns` | `field_ins_handle` | 0x08 |
| `ChrIns` | `chr_set_entry` | 0x10 |
| `ChrIns` | `event_entity_id` | 0x1E8 |
| `ChrIns` | size | 0x580 |
| `ChrSet` | `capacity` | 0x10 |
| `ChrSet` | `entries` | 0x18 |
| `ChrSet` | `entity_id_mapping` | 0x28 |
| `ChrSet` | `group_id_mapping` | 0x40 |
| `ChrSetEntry` | `chr_ins` | 0x00 |
| `ChrSetEntry` | `chr_load_status` / `chr_update_type` / `entry_flags` | 0x08 / 0x09 / 0x0A |
| `ChrSetEntry` | size, and the stride `free_chr_list` walks `entries` with | 0x10 |
| `DLMap` | size (allocator / head / size) | 0x18 (0x00 / 0x08 / 0x10) |
| `PlayerIns` | `locked_on_enemy` | 0x6B0 |

Disassembly of `ChrSet::free_chr_list` confirms the `ChrSet` and `ChrSetEntry`
rows directly: it reads the capacity at `+0x10`, walks `entries` at `+0x18` with
`shl rcx, 4` (stride 0x10), clears the mapped id tables at `+0x28` and `+0x40`,
and finally zeroes the capacity.

## Event entity id

`ChrSet::entity_id_mapping` is an ordered map from an event entity id to the
`ChrSetEntry` that owns it. The engine resolves an id by looking the id up there,
so an id only works for event scripts once that mapping contains it.

A generated unit receives the configured id through
`CSDebugChrCreatorInitData`, but nothing establishes that the engine copies it
into both `ChrIns::event_entity_id` and the mapping. The mod therefore checks all
three and fills the gaps:

- it writes `ChrIns::event_entity_id` when the mapping does not already own the id
  for this character,
- it inserts the mapping entry, refusing to steal an id another character in the
  same character set owns,
- it re-resolves the id through the mapping and reports whether that leads back
  to this very unit.

A live run answers the open question without a debugger: `summon bound` in
`summon.log` prints `entity_id_outcome`, `chr_reports_id` and
`id_resolves_to_this_unit`. `engine-registered` means the engine had already done
the work; `dll-registered` means the mod had to.

## Removal

There is **no engine API that frees a single character**. The `ChrSet` vtable has
ten usable slots, and the only one that frees anything is slot 8,
`free_chr_list`, which frees every character in the set and then clears both id
mappings. Slot 9 is a one instruction `ret 0` stub. A mod that wants one unit
gone has to run the teardown itself.

The engine's own per-character free, as performed inside `free_chr_list`:

```text
call get_chr_ins_by_index(chr_set, index)      -> chr
if chr:
    chr.vftable[1]  (this = chr, delete_flag = 0)      ; destructor only
    owning_allocator(chr).vftable[0x68]  (chr)         ; DLAllocator::deallocate
```

The mod instead calls `chr->vftable[1](chr, 1)`, the scalar deleting destructor:

```text
ChrIns scalar deleting destructor, vftable slot 1, RVA 0x3E81F0
    0x1403E81FF  call 0x3E7970                 ; destructor body, unconditional
    0x1403E8204  test bl, 1                    ; the delete flag
    0x1403E8207  je   done
    0x1403E8209  mov  edx, 0x580               ; ChrIns size
    0x1403E8211  call operator delete
```

Both routes free the same memory:

- The exe imports no CRT DLL, so its CRT is linked in statically. The ILT stub at
  `0x1424FD294` resolves through `0x1424FD87C` and `0x142541430` into
  `0x14254CB4C`, which loads a process-wide heap handle and calls `HeapFree` on
  it, setting errno when the call fails — the shape of MSVC `_free_base`.
- The binding documents the character set's allocator
  (`DLAllocator::runtime_heap_allocator`, holder RVA `0x4846DC0`) as using "the
  standard MSVC malloc()/free() implementation for heap management".

Both are MSVC `free` on the CRT heap, so the delete flag gives the engine's own
result while keeping the mod free of hardcoded RVAs: the destructor address comes
from the object's own vtable at runtime.

Because the mod always clears the `ChrSet` entry as its last step, a later
engine-side teardown of the whole set sees an empty slot and skips it instead of
freeing the same address twice.

## What the mod does not touch

- `group_id_mapping` (+0x40). `free_chr_list` clears it wholesale, but nothing in
  the bindings writes to it, so no per-character entry is known to exist. If one
  did, it would point at the cleared entry, which reads back as "no character" —
  never at freed memory.
- Bullets or other field instances a unit may have spawned. They carry their own
  lifetimes and are not part of the character allocation.
- `ChrIns::backread_state` and the block bookkeeping. The mod makes the unit inert
  through the existing hide path before it is freed, and the engine drops its own
  references from there.

## Still unverified

- No live session was run against this build. The staged removal, the id
  registration outcomes and the absence of a crash after a freed summon are what
  `summon.log` and an in-game check are for.
- `free_chr_list` guards its `group_id_mapping` clear behind a global that may be
  null early in startup; that path was read, not exercised.
