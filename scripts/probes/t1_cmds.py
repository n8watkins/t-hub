"""Phase A: command surface + protocol_version behavior over one pipelined connection."""
import json, sys
import os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import connect, LineReader, request

def trunc(v, n=220):
    s = json.dumps(v)
    return s if len(s) <= n else s[:n] + f"...(+{len(s)-n}B)"

sock, hs = connect()
rd = LineReader(sock)
tok = hs["token"]
out = {}

# 1. list_terminals (v=1)
r = request(sock, rd, tok, "list_terminals")
out["list_terminals"] = r
ids = [t["id"] for t in r["result"]["terminals"]] if r.get("ok") else []
print("list_terminals ok=%s count=%s ids=%s" % (r.get("ok"), r["result"]["count"] if r.get("ok") else "-", ids))

# 2. get_status for an existing session
sid = ids[0] if ids else None
r = request(sock, rd, tok, "get_status", {"sessionId": sid})
print("get_status(%s): %s" % (sid, trunc(r)))

# 3. supervision_session_ids
r = request(sock, rd, tok, "supervision_session_ids")
print("supervision_session_ids:", trunc(r))

# 4. wsl_health
r = request(sock, rd, tok, "wsl_health")
print("wsl_health:", trunc(r, 300))

# 5. git_info on this repo
r = request(sock, rd, tok, "git_info", {"path": "/home/natkins/projects/tools/t-hub/t-hub-app"})
print("git_info:", trunc(r, 300))

# 6. list_tabs
r = request(sock, rd, tok, "list_tabs")
print("list_tabs:", trunc(r))

# 7. protocol version behavior: no v (legacy) -> accepted
r = request(sock, rd, tok, "list_tabs", v=None)
print("no-v request ok=%s (legacy accepted)" % r.get("ok"))

# 8. wrong v -> rejected with clear message
r = request(sock, rd, tok, "list_tabs", v=2)
print("v=2 request:", trunc(r))

# 9. bad token -> unauthorized
r = request(sock, rd, tok, "list_terminals", token_override="not-the-token-000000000000000000000")
print("bad-token:", trunc(r))

# 10. spawn_terminal -> expected GATED error (documents the fallback need)
r = request(sock, rd, tok, "spawn_terminal", {"cwd": "/tmp"})
print("spawn_terminal:", trunc(r, 300))

sock.close()
print("PHASE-A-DONE")
