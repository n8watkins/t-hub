"""Basic spawn test: create_worktree over the control socket, confirm a new session appears."""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import connect, LineReader, request

REPO = "/home/natkins/projects/tools/t-hub/t-hub-app"
WT = os.path.join(REPO, ".claude/worktrees/spawn-test")
BRANCH = "spawn-test"
TAB = "spawn-test"

sock, hs = connect()
rd = LineReader(sock)
tok = hs["token"]

before = request(sock, rd, tok, "list_terminals")
n_before = before["result"]["count"] if before.get("ok") else "?"
print("before: count=%s" % n_before)

r = request(sock, rd, tok, "create_worktree", {
    "repoRoot": REPO,
    "worktreePath": WT,
    "branch": BRANCH,
    "tabName": TAB,
})
print("create_worktree:", r)

after = request(sock, rd, tok, "list_terminals")
n_after = after["result"]["count"] if after.get("ok") else "?"
print("after: count=%s" % n_after)

sock.close()
print("DONE")
