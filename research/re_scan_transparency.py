"""Find writers of ChrIns.base_transparency_modifier (offset 0x250, f32).

The pinned fsrs revision documents ChrDebugFlags::force_unloaded as:
  "Will set base_transparency_modifier to -1 when set
   and change chrSetEntry status to Unloading"

So the instruction group that consumes the force_unloaded debug flag both
writes -1.0f to [chr+0x250] and writes the Unloading status to [entry+8].
Locating the former gives us the exact engine function that performs the
Unloading transition.

Scan: raw byte search for the disp32 0x250 in memory operands (mod=10),
then decode candidates with capstone and keep only writes.

Usage: python re_scan_transparency.py <exe>
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

# opcode prefixes that write to memory, followed by modrm with mod=10 rm=reg
WRITE_PREFIXES = {
    b"\xc7": "mov dword [mem], imm32",
    b"\xf3\x0f\x11": "movss [mem], xmm",
    b"\x0f\x11": "movups [mem], xmm",
    b"\x0f\x29": "movaps [mem], xmm",
    b"\xf3\x0f\x10": "movss xmm, [mem]  (read)",
    b"\xf3\x0f\x5e": "divss",
}


def main():
    exe = sys.argv[1]
    pe = pefile.PE(exe, fast_load=True)
    data = open(exe, "rb").read()
    base = pe.OPTIONAL_HEADER.ImageBase
    text = None
    for s in pe.sections:
        if s.Name.rstrip(b"\x00") == b".text":
            text = s
    blob = data[text.PointerToRawData:text.PointerToRawData + text.SizeOfRawData]
    text_va = base + text.VirtualAddress

    needle = (0x250).to_bytes(4, "little")
    md = Cs(CS_ARCH_X86, CS_MODE_64)
    md.detail = True

    found = []
    pos = 0
    while True:
        i = blob.find(needle, pos)
        if i < 0:
            break
        pos = i + 1
        # the modrm byte is directly before the disp32
        if i == 0:
            continue
        modrm = blob[i - 1]
        if (modrm & 0xC0) != 0x80:
            continue
        # look back up to 4 bytes for a known write prefix
        for back in range(2, 6):
            j = i - 1 - back
            if j < 0:
                continue
            seg = blob[j:i + 4]
            for pfx, desc in WRITE_PREFIXES.items():
                if seg.startswith(pfx):
                    va = text_va + j
                    insns = list(md.disasm(blob[j:j + 16], va, count=1))
                    if insns:
                        found.append((va, insns[0], desc))

    seen = set()
    print(f"candidates: {len(found)}")
    print()
    for va, insn, desc in sorted(found):
        if va in seen:
            continue
        seen.add(va)
        print(f"0x{va:X}  rva=0x{va - base:X}  {insn.mnemonic} {insn.op_str}    [{desc}]")


if __name__ == "__main__":
    main()
