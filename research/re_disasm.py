"""Disassemble a range of eldenring.exe and resolve call targets.

Usage: python re_disasm.py <exe> <start-va-hex> <length> [--follow <hex> ...]
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

KNOWN = {
    0x140493D50: "ChrSet::get_capacity",
    0x140493CE0: "ChrSet::safe_get_chr_ins_by_index",
    0x140493D40: "ChrSet::get_chr_ins_by_index",
    0x140493D00: "ChrSet::get_chr_ins_by_handle",
    0x140493C50: "ChrSet::safe_get_chr_set_entry_by_index",
    0x140493CB0: "ChrSet::get_chr_set_entry_by_index",
    0x140493C70: "ChrSet::get_chr_set_entry_by_handle",
    0x140493CC0: "ChrSet::get_index_by_handle",
    0x140495890: "ChrSet::free_chr_list",
    0x140494790: "ChrSet::unk48",
    0x140495AA0: "ChrSet::unk50",
    0x1404960D0: "ChrSet::unk58",
    0x140496260: "ChrSet::unk60",
    0x140495E40: "ChrSet::unk68",
}


def load(exe):
    pe = pefile.PE(exe, fast_load=True)
    data = open(exe, "rb").read()
    return pe, data


def make_va_to_off(pe):
    def va_to_off(va):
        rva = va - pe.OPTIONAL_HEADER.ImageBase
        for s in pe.sections:
            if s.VirtualAddress <= rva < s.VirtualAddress + max(s.Misc_VirtualSize, s.SizeOfRawData):
                if rva - s.VirtualAddress < s.SizeOfRawData:
                    return s.PointerToRawData + (rva - s.VirtualAddress)
        return None
    return va_to_off


def main():
    exe = sys.argv[1]
    start = int(sys.argv[2], 16)
    length = int(sys.argv[3], 16)
    follow = [int(x, 16) for x in sys.argv[4:]]

    pe, data = load(exe)
    va_to_off = make_va_to_off(pe)
    md = Cs(CS_ARCH_X86, CS_MODE_64)
    md.detail = True

    def dump(start_va, size, label=""):
        off = va_to_off(start_va)
        if off is None:
            print(f"!! cannot map 0x{start_va:X}")
            return
        code = data[off:off + size]
        print(f"=== {label}0x{start_va:X} ({size:#x} bytes) ===")
        for insn in md.disasm(code, start_va):
            note = ""
            if insn.mnemonic == "call" and insn.operands:
                op = insn.operands[0]
                if op.type == 2:  # X86_OP_IMM
                    tgt = op.imm
                    note = f"   -> 0x{tgt:X}"
                    if tgt in KNOWN:
                        note += f"  [{KNOWN[tgt]}]"
                    if abs(tgt - start_va) > 0x4000:
                        note += "  (external)"
            print(f"0x{insn.address:X}: {insn.mnemonic:8s} {insn.op_str}{note}")
        print()

    dump(start, length, "")
    for t in follow:
        known = KNOWN.get(t, "")
        dump(t, 0x180, f"{known} " if known else "")


if __name__ == "__main__":
    main()
