"""Resolve DLAllocator::deallocate and see which heap it really frees through.

Chain:
  1. RVA 0x4846dc0 (rva_ww::runtime_heap_allocator) holds the instance pointer.
  2. instance +0x00 is its vftable.
  3. vftable + 8*13 is DLAllocatorVmt::deallocate (allocator.rs slot order).

Usage: python re_allocator.py <exe>
"""

import sys
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

RUNTIME_HEAP_ALLOCATOR_PTR_RVA = 0x4846DC0
DEALLOCATE_SLOT = 13

SLOTS = [
    "destructor",
    "allocator_id",
    "unk10",
    "heap_flags",
    "heap_capacity",
    "heap_size",
    "backing_heap_capacity",
    "heap_allocation_count",
    "allocation_size",
    "allocate",
    "allocate_aligned",
    "reallocate",
    "reallocate_aligned",
    "deallocate",
    "allocate_second",
    "allocate_aligned_second",
    "reallocate_second",
    "reallocate_aligned_second",
    "deallocate_second",
    "unka0",
    "allocation_belongs_to_first_allocator",
    "allocation_belongs_to_second_allocator",
    "lock",
    "unlock",
    "get_memory_block_for_allocation",
]


class Image:
    def __init__(self, exe):
        self.pe = pefile.PE(exe, fast_load=True)
        self.base = self.pe.OPTIONAL_HEADER.ImageBase
        self.data = open(exe, "rb").read()

    def off(self, va):
        rva = va - self.base
        for s in self.pe.sections:
            if s.VirtualAddress <= rva < s.VirtualAddress + max(
                s.Misc_VirtualSize, s.SizeOfRawData
            ):
                if rva - s.VirtualAddress < s.SizeOfRawData:
                    return s.PointerToRawData + (rva - s.VirtualAddress)
        return None

    def u64(self, va):
        o = self.off(va)
        return None if o is None else int.from_bytes(self.data[o:o + 8], "little")


def main():
    img = Image(sys.argv[1])
    md = Cs(CS_ARCH_X86, CS_MODE_64)

    holder = img.base + RUNTIME_HEAP_ALLOCATOR_PTR_RVA
    instance = img.u64(holder)
    print(f"runtime_heap_allocator holder VA 0x{holder:X} -> instance 0x{instance:X}")
    if not instance or instance < img.base:
        print("!! holder does not contain a plausible instance pointer")
        return
    print(f"instance RVA 0x{instance - img.base:X}")

    vtable = img.u64(instance)
    print(f"vftable VA 0x{vtable:X} (rva 0x{vtable - img.base:X})")
    if not vtable or vtable < img.base:
        print("!! instance does not start with a vtable pointer")
        return

    print()
    for i, name in enumerate(SLOTS):
        fn = img.u64(vtable + 8 * i)
        if fn is None or fn < img.base:
            print(f"  [{i:2d}] 0x{fn}  {name}  (not a pointer)")
            continue
        print(f"  [{i:2d}] 0x{fn:X}  rva=0x{fn - img.base:X}  {name}")

    target = img.u64(vtable + 8 * DEALLOCATE_SLOT)
    print(f"\n===== deallocate @ 0x{target:X} (rva 0x{target - img.base:X}) =====")
    code = img.data[img.off(target):img.off(target) + 0x180]
    for insn in md.disasm(code, target):
        note = ""
        if insn.mnemonic in ("call", "jmp") and insn.operands and insn.operands[0].type == 2:
            note = f"   -> 0x{insn.operands[0].imm:X}"
        print(f"0x{insn.address:X}: {insn.mnemonic:8s} {insn.op_str}{note}")


if __name__ == "__main__":
    main()
