"""Locate the engine's character-free path (ChrSet::free_chr_list) in eldenring.exe.

Target: WW 2.7.1.0, SHA256 1A3547101327F65D0C76DA2F9190AC0AA66871EA42BAE2AECC61E11A8B597891
RVAs come from the pinned fromsoftware-rs fork revision 02fa5681 (rva_ww.rs, 1.17 routing).

Re-runnable: python re_chrset.py <path-to-eldenring.exe>
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

CHR_SET_VMT_RVA = 0x2A41348

SLOT_NAMES = [
    "get_capacity",
    "safe_get_chr_ins_by_index",
    "get_chr_ins_by_index",
    "get_chr_ins_by_handle",
    "safe_get_chr_set_entry_by_index",
    "get_chr_set_entry_by_index",
    "get_chr_set_entry_by_handle",
    "get_index_by_handle",
    "free_chr_list",
    "unk48",
    "unk50",
    "unk58",
    "unk60",
    "unk68",
]


def main():
    exe = sys.argv[1]
    pe = pefile.PE(exe, fast_load=True)
    base = pe.OPTIONAL_HEADER.ImageBase
    data = open(exe, "rb").read()

    def va_to_off(va):
        rva = va - base
        for s in pe.sections:
            if s.VirtualAddress <= rva < s.VirtualAddress + max(s.Misc_VirtualSize, s.SizeOfRawData):
                if rva - s.VirtualAddress < s.SizeOfRawData:
                    return s.PointerToRawData + (rva - s.VirtualAddress)
        return None

    def read_u64_va(va):
        off = va_to_off(va)
        if off is None:
            return None
        return int.from_bytes(data[off:off + 8], "little")

    md = Cs(CS_ARCH_X86, CS_MODE_64)

    vtable_va = base + CHR_SET_VMT_RVA
    print(f"image base        : 0x{base:X}")
    print(f"ChrSet vftable VA : 0x{vtable_va:X}")
    print()

    slots = []
    for i in range(24):
        slot_va = read_u64_va(vtable_va + 8 * i)
        if slot_va is None or slot_va < base:
            break
        slots.append(slot_va)

    print(f"found {len(slots)} vtable slots")
    print()
    for i, slot_va in enumerate(slots):
        name = SLOT_NAMES[i] if i < len(SLOT_NAMES) else "?"
        print(f"  [{i:2d}] slot=0x{slot_va:X} rva=0x{slot_va - base:X}  ({name})")
    print()

    # Print a short disassembly of every slot so the trait-to-slot alignment can be
    # verified against observable behaviour (get_capacity reads [rcx+0x10]).
    for i, slot_va in enumerate(slots):
        name = SLOT_NAMES[i] if i < len(SLOT_NAMES) else "?"
        off = va_to_off(slot_va)
        if off is None:
            continue
        code = data[off:off + 0x120]
        print(f"=== slot[{i}] {name} @ 0x{slot_va:X} ===")
        n = 0
        for insn in md.disasm(code, slot_va):
            print(f"  0x{insn.address:X}: {insn.mnemonic:8s} {insn.op_str}")
            n += 1
            if n >= 18:
                break
        print()


if __name__ == "__main__":
    main()
