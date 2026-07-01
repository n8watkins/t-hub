"""Focused re-test: (a) fresh seed contains marker, (d) resize changes pane geometry."""
import json, subprocess, sys, time
import os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import connect, LineReader, send_line, b64d, b64e

SESSION = "th_t1-split-check"

def geometry():
    r = subprocess.run(["tmux", "-L", "t-hub", "list-panes", "-t", SESSION,
                        "-F", "#{pane_width}x#{pane_height}"],
                       capture_output=True, text=True)
    return r.stdout.strip()

sock, hs = connect()
rd = LineReader(sock)
send_line(sock, {"token": hs["token"], "command": "attach_pty",
                 "args": {"sessionId": SESSION, "cols": 100, "rows": 30}, "v": 1})
seed = json.loads(rd.read_line(timeout=15))
sb = b64d(seed["scrollback"]).decode("utf-8", "replace")
print("[a-redo] fresh seed %dB; contains T1-SEED-MARKER: %s; contains T1-LOOPBACK-CHECK: %s"
      % (len(sb), "T1-SEED-MARKER" in sb, "T1-LOOPBACK-CHECK" in sb))
time.sleep(1.0)
print("[d] attached 100x30 -> pane:", geometry())
send_line(sock, {"resize": {"cols": 90, "rows": 25}})
time.sleep(1.2)
g1 = geometry(); print("[d] resize(90x25)  -> pane:", g1)
send_line(sock, {"resize": {"cols": 120, "rows": 40}})
time.sleep(1.2)
g2 = geometry(); print("[d] resize(120x40) -> pane:", g2)
print("[d] geometry follows resize frames:", g1 == "90x24" and g2 == "120x39")
sock.close()
print("RESIZE-DONE")
