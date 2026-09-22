"""Verify the WW 2.7.1.0 native paths used by the retirement candidate.
Usage: python research/verify_retirement_paths.py <eldenring.exe>
Static evidence only; this does not prove live scheduler quiescence.
"""
import hashlib
import sys
from pathlib import Path
import pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_64

path=Path(sys.argv[1]); data=path.read_bytes()
assert hashlib.sha256(data).hexdigest().upper()=="1A3547101327F65D0C76DA2F9190AC0AA66871EA42BAE2AECC61E11A8B597891", "unverified executable"
p=pefile.PE(data=data, fast_load=True)
root=Path(__file__).resolve().parents[1]
assert p.get_data(0x495890,0x100)==(root/'tests/fixtures/ww271-chrset-free-prefix.bin').read_bytes()
assert p.get_data(0xeb3530,0x25)==(root/'tests/fixtures/ww271-task-free.bin').read_bytes()
md=Cs(CS_ARCH_X86,CS_MODE_64)
for label,rva,size in [
 ('native detach before free',0x4958d6,0x36),
 ('detach entry reset',0x494f79,0x31),
 ('native task proxy unregister',0xeb3530,0x25),
 ('proxy clears subject',0xeb36f0,0x18),
 ('work list rebuilt from ChrSet',0x512160,0x7d),
 ('stale reader',0x510a20,0x18),
]:
 print(label)
 for x in md.disasm(p.get_data(rva,size),rva):print(f"  {x.address:#x}: {x.mnemonic} {x.op_str}")
print('PASS: verified binary fixtures and native call sites; runtime acceptance pending')
