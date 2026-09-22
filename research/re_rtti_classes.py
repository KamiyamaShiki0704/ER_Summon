"""Dump all RTTI class-name strings from eldenring.exe, filtered by substring.

RTTI names in FS titles are ASCII `.?AV<Class>@<Namespace>@@` (optionally with
DLRuntimeClassImpl wrappers). Each string's file offset is reported so it can be
used with the xref scanner.

Usage: python re_rtti_classes.py <exe> [substring ...]
"""

import sys
import re
import pefile


def main():
    exe = sys.argv[1]
    filters = [a.lower() for a in sys.argv[2:]]
    pe = pefile.PE(exe, fast_load=True)
    data = open(exe, "rb").read()
    base = pe.OPTIONAL_HEADER.ImageBase

    def off_to_va(off):
        for s in pe.sections:
            if s.PointerToRawData <= off < s.PointerToRawData + s.SizeOfRawData:
                return base + s.VirtualAddress + (off - s.PointerToRawData)
        return None

    names = {}
    for m in re.finditer(rb"\.\?AV[A-Za-z0-9_@?$<>:,\.\-]{2,200}@@", data):
        off = m.start()
        raw = m.group(0).decode("ascii", "replace")
        names.setdefault(raw, off)

    print(f"total RTTI-ish name strings: {len(names)}")
    print()
    out = []
    for raw, off in names.items():
        if not filters or any(f in raw.lower() for f in filters):
            va = off_to_va(off)
            out.append((raw, off, va))
    out.sort(key=lambda x: x[0])
    for raw, off, va in out:
        print(f"off=0x{off:X} va=0x{va:X}  {raw}" if va else f"off=0x{off:X}  {raw}")
    print()
    print(f"matched: {len(out)}")


if __name__ == "__main__":
    main()
