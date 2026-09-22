"""Dump the ChrIns vtable and disassemble selected slots.

ChrInsVmt slot order (pinned fsrs revision):
  0 get_runtime_metadata
  1 destructor(dtor_arg)          <- the engine's per-character free primitive
  2 get_field_ins_type
  3 use_npc_atk_param
  4 get_atk_param_for_behavior
  5 use_player_behavior_param
  6 unk30
  7 unk38
  8 initialize_character
  9 initialize_model_resources
  10 initialize_character_rendering

Usage: python re_chrins_vmt.py <exe> [slot ...]
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

CHR_INS_VMT_RVA = 0x2A310C8

SLOTS = {
    0: "get_runtime_metadata",
    1: "destructor",
    2: "get_field_ins_type",
    3: "use_npc_atk_param",
    4: "get_atk_param_for_behavior",
    5: "use_player_behavior_param",
    6: "unk30",
    7: "unk38",
    8: "initialize_character",
    9: "initialize_model_resources",
    10: "initialize_character_rendering",
}


def main():
    exe = sys.argv[1]
    pe = pefile.PE(exe, fast_load=True)
    data = open(exe, "rb").read()
    base = pe.OPTIONAL_HEADER.ImageBase

    def va_to_off(va):
        rva = va - base
        for s in pe.sections:
            if s.VirtualAddress <= rva < s.VirtualAddress + max(s.Misc_VirtualSize, s.SizeOfRawData):
                if rva - s.VirtualAddress < s.SizeOfRawData:
                    return s.PointerToRawData + (rva - s.VirtualAddress)
        return None

    vtable = base + CHR_INS_VMT_RVA
    print(f"ChrIns vftable VA 0x{vtable:X}")
    entries = []
    for i in range(12):
        off = va_to_off(vtable + 8 * i)
        val = int.from_bytes(data[off:off + 8], "little")
        entries.append(val)
        name = SLOTS.get(i, "?")
        print(f"  [{i:2d}] 0x{val:X}  rva=0x{val - base:X}  {name}")

    if len(sys.argv) > 2:
        md = Cs(CS_ARCH_X86, CS_MODE_64)
        md.detail = True
        for arg in sys.argv[2:]:
            i = int(arg)
            va = entries[i]
            off = va_to_off(va)
            print(f"\n===== slot[{i}] {SLOTS.get(i, '?')} @ 0x{va:X} (rva 0x{va - base:X}) =====")
            code = data[off:off + 0x400]
            for insn in md.disasm(code, va):
                note = ""
                if insn.mnemonic == "call" and insn.operands and insn.operands[0].type == 2:
                    t = insn.operands[0].imm
                    note = f"   -> 0x{t:X}"
                print(f"0x{insn.address:X}: {insn.mnemonic:8s} {insn.op_str}{note}")


if __name__ == "__main__":
    main()
