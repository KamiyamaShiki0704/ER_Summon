"""Check a completed human disappearance trial, using the last startup session.

Run after waiting for removal, before another game startup overwrites the session
boundary. PASS concerns logged completion only; CT list and crash checks remain
independent runtime acceptance evidence.
"""
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8-sig")
session = text.rsplit("summon initialised:", 1)[-1]
staged = re.findall(r"summon staged for removal: handle=(.*?) npc_param_id=", session)
finished = re.findall(
    r"summon removed: handle=(FieldIns\([^\r\n)]*\))[^\r\n]*outcome=destroyed retirement=frame-end-drained",
    session,
)
if not staged:
    print("INCONCLUSIVE: no staged removal in latest session")
    sys.exit(2)
pending = list(staged)
for handle in finished:
    if handle in pending:
        pending.remove(handle)
print(f"staged={len(staged)} released={len(finished)} incomplete={len(pending)}")
for handle in pending:
    print(f"FAIL: no final release for {handle}")
sys.exit(1 if pending else 0)
