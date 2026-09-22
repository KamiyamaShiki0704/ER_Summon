"""Scan eldenring.exe .text for ChrSetEntry::chr_load_status (byte at entry+8) accesses.

ChrSetEntry layout (pinned fsrs revision, verified against vtable slot behaviour):
  +0x00 chr_ins: Option<NonNull>   (8 bytes)
  +0x08 chr_load_status: u8        (Unloaded=0 Initializing=1 Active=2 NetworkInit=3 ReadyForActivation=4 Unloading=5)
  +0x09 chr_update_type: u8
  +0x0a entry_flags: u8

We look for:
  writes  : C6 /0 ib   with modrm mem disp8=0x08 and imm=5 or 2
  compares: 80 /7 ib   with modrm mem disp8=0x08 and imm=5 or 2
  and the REX / disp32 / SIB variants.

Usage: python re_scan_loadstatus.py <exe>
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

IMMS = (2, 5)


def get_text(pe, data):
    for s in pe.sections:
        if s.Name.rstrip(b"\x00") == b".text":
            return s
    raise SystemExit("no .text")


def main():
    exe = sys.argv[1]
    pe = pefile.PE(exe, fast_load=True)
    data = open(exe, "rb").read()
    base = pe.OPTIONAL_HEADER.ImageBase
    text = get_text(pe, data)
    blob = data[text.PointerToRawData:text.PointerToRawData + text.SizeOfRawData]
    text_va = base + text.VirtualAddress

    md = Cs(CS_ARCH_X86, CS_MODE_64)
    md.detail = True

    hits = []

    # Candidate encodings: (prefix bytes, opcode, modrm-of-memory-with-disp8, imm)
    # modrm mem forms with rm=reg (no SIB): 0x40..0x47 (SIB), 0x48..0x4F (disp8), 0x50..0x57, 0x58..0x5F
    prefixes = [b"", b"\x41", b"\x44", b"\x45", b"\x48", b"\x4c", b"\x49", b"\x4d",
                b"\x66", b"\x48\x66"]
    for pfx in prefixes:
        for opcode in (b"\xc6", b"\x80"):
            for r in range(8):
                # mod=01 rm=000..111 => 0x40|r<<3|rm  (rm=100 has SIB -> skip)
                for rm in (0, 1, 2, 3, 4, 5, 6, 7):
                    modrm = 0x40 | (r << 3) | rm
                    if rm == 4:
                        continue
                    for imm in IMMS:
                        pat = pfx + opcode + bytes([modrm, 0x08, imm])
                        start = 0
                        while True:
                            i = blob.find(pat, start)
                            if i < 0:
                                break
                            hits.append((text_va + i, pat))
                            start = i + 1

    print(f"raw pattern hits: {len(hits)}")
    seen = set()
    out = []
    for va, pat in hits:
        if va in seen:
            continue
        seen.add(va)
        off = va - text_va
        # decode the single instruction at this location, report modrm + imm + operands
        code = blob[off:off + 16]
        insns = list(md.disasm(code, va, count=1))
        if not insns:
            continue
        insn = insns[0]
        if not insn.operands:
            continue
        # require: memory operand with disp == 8, and imm operand
        has_mem8 = False
        has_imm = None
        for op in insn.operands:
            if op.type == 3:  # X86_OP_MEM
                if op.mem.disp == 8:
                    has_mem8 = True
            elif op.type == 2:
                has_imm = op.imm
        if has_mem8 and has_imm in IMMS:
            out.append((va, insn))

    print(f"validated instructions: {len(out)}")
    print()
    for va, insn in out:
        kind = "WRITE" if insn.mnemonic == "mov" else "CMP  "
        print(f"{kind} 0x{va:X}  rva=0x{va - base:X}  {insn.mnemonic} {insn.op_str}")


if __name__ == "__main__":
    main()
