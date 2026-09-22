"""Scan eldenring.exe .text for accesses to ChrIns.debug_flags (offset 0x538, u32).

Both the Summon private-field bridge and the pinned fsrs revision place
ChrDebugFlags at ChrIns+0x538 (bit 10 = force_unloaded, bit 11 = force_loaded).
The engine code that consumes force_unloaded is the code that performs the
chrSetEntry -> Unloading transition, so locating these accesses gives the exact
unload implementation.

Any memory operand with disp32 == 0x538 needs the byte sequence 38 05 00 00.
We classify by the opcode bytes preceding it.

Usage: python re_scan_debugflags.py <exe>
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

DISP = (0x538).to_bytes(4, "little")

# (prefix bytes, mnemonic hint) -- modrm byte follows the prefix
OPCODES = [
    (b"\xf7", "test dword [m],imm"),
    (b"\x81", "grp1 dword [m],imm (or/and/sub)"),
    (b"\xc7", "mov dword [m],imm"),
    (b"\x8b", "mov r32,[m]"),
    (b"\x89", "mov [m],r32"),
    (b"\xf6", "test byte [m+1],imm"),
    (b"\x0f\xba", "bt/bts/btr [m],imm"),
    (b"\x0f\xb6", "movzx r32,byte [m]"),
    (b"\x0f\xb7", "movzx r32,word [m]"),
    (b"\x0f\xbe", "movsx"),
    (b"\x0f\xbf", "movsx"),
    (b"\x83", "grp1 dword [m],imm8"),
    (b"\x39", "cmp [m],r32"),
    (b"\x3b", "cmp r32,[m]"),
    (b"\x85", "test [m],r32"),
    (b"\x0b", "or r32,[m]"),
    (b"\xf3\x0f\x10", "movss xmm,[m]"),
    (b"\xf3\x0f\x11", "movss [m],xmm"),
]

REX = set(range(0x40, 0x50))


def main():
    exe = sys.argv[1]
    pe = pefile.PE(exe, fast_load=True)
    data = open(exe, "rb").read()
    base = pe.OPTIONAL_HEADER.ImageBase
    for s in pe.sections:
        if s.Name.rstrip(b"\x00") == b".text":
            text = s
    blob = data[text.PointerToRawData:text.PointerToRawData + text.SizeOfRawData]
    text_va = base + text.VirtualAddress
    md = Cs(CS_ARCH_X86, CS_MODE_64)
    md.detail = True

    results = []
    pos = 0
    while True:
        i = blob.find(DISP, pos)
        if i < 0:
            break
        pos = i + 1
        if i < 6:
            continue
        # try to decode an instruction ending exactly at i+4, starting at i-k for k=1..6
        for k in range(1, 7):
            j = i - k
            seg = blob[j:i + 4]
            insns = list(md.disasm(seg, text_va + j, count=1))
            if len(insns) != 1:
                continue
            insn = insns[0]
            if insn.address + insn.size != text_va + i + 4:
                continue
            has_mem = any(op.type == 3 and op.mem.disp == 0x538 for op in insn.operands) if insn.operands else False
            if not has_mem:
                continue
            results.append((text_va + j, insn))
            break

    # de-dup
    seen = set()
    uniq = []
    for va, insn in results:
        if va in seen:
            continue
        seen.add(va)
        uniq.append((va, insn))

    print(f"instructions touching ChrIns+0x538: {len(uniq)}")
    print()
    for va, insn in sorted(uniq):
        print(f"0x{va:X}  rva=0x{va - base:X}  {insn.mnemonic} {insn.op_str}")


if __name__ == "__main__":
    main()
