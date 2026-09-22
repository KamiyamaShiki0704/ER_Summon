"""PE helpers: .pdata function-boundary lookup + targeted disassembly.

Usage:
  python re_func.py <exe> find <va-hex> [<va-hex> ...]      # which function contains these VAs
  python re_func.py <exe> dis <start-va-hex> <len-hex>      # disassemble
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64


class Pe:
    def __init__(self, path):
        self.pe = pefile.PE(path, fast_load=True)
        self.data = open(path, "rb").read()
        self.base = self.pe.OPTIONAL_HEADER.ImageBase
        self.sections = {}
        for s in self.pe.sections:
            name = s.Name.rstrip(b"\x00").decode()
            self.sections[name] = s

    def rva_to_off(self, rva):
        for s in self.pe.sections:
            if s.VirtualAddress <= rva < s.VirtualAddress + max(s.Misc_VirtualSize, s.SizeOfRawData):
                if rva - s.VirtualAddress < s.SizeOfRawData:
                    return s.PointerToRawData + (rva - s.VirtualAddress)
        return None

    def va_to_off(self, va):
        return self.rva_to_off(va - self.base)

    def read(self, va, n):
        off = self.va_to_off(va)
        if off is None:
            return None
        return self.data[off:off + n]

    def pdata_ranges(self):
        s = self.sections[".pdata"]
        off = s.PointerToRawData
        out = []
        for i in range(s.SizeOfRawData // 12):
            start = int.from_bytes(self.data[off + i * 12:off + i * 12 + 4], "little")
            end = int.from_bytes(self.data[off + i * 12 + 4:off + i * 12 + 8], "little")
            if start == 0 and end == 0:
                continue
            out.append((self.base + start, self.base + end))
        out.sort()
        return out


def main():
    exe = sys.argv[1]
    p = Pe(exe)
    mode = sys.argv[2]

    if mode == "find":
        ranges = p.pdata_ranges()
        starts = [r[0] for r in ranges]
        import bisect
        for arg in sys.argv[3:]:
            va = int(arg, 16)
            i = bisect.bisect_right(starts, va) - 1
            if i < 0:
                print(f"0x{va:X}: no .pdata entry")
                continue
            s, e = ranges[i]
            inside = s <= va < e
            print(f"0x{va:X}: function 0x{s:X}..0x{e:X} (rva 0x{s - p.base:X}..0x{e - p.base:X}, "
                  f"size {e - s:#x}) inside={inside}")
    elif mode == "dis":
        start = int(sys.argv[3], 16)
        length = int(sys.argv[4], 16)
        md = Cs(CS_ARCH_X86, CS_MODE_64)
        md.detail = True
        code = p.read(start, length)
        for insn in md.disasm(code, start):
            note = ""
            if insn.mnemonic == "call" and insn.operands:
                op = insn.operands[0]
                if op.type == 2:
                    tgt = op.imm
                    note = f"   -> 0x{tgt:X} (rva 0x{tgt - p.base:X})"
            print(f"0x{insn.address:X}: {insn.mnemonic:8s} {insn.op_str}{note}")


if __name__ == "__main__":
    main()
