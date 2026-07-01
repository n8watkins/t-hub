"""Phase C+D: PTY plane on a disposable session + reconnect re-sync.

Creates tmux session th_t1-split-check (cwd /tmp) on the isolated t-hub socket,
attaches via the control socket's attach_pty, exercises scrollback/out/write/
resize, then drops the connection mid-stream and re-attaches to verify re-sync.
Only ever touches th_t1-split-check.
"""
import json, subprocess, sys, time
import os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import connect, LineReader, send_line, b64d, b64e

SESSION = "th_t1-split-check"

def tmux(*args):
    return subprocess.run(["tmux", "-L", "t-hub", *args],
                          capture_output=True, text=True)

def geometry():
    r = tmux("display", "-p", "-t", "=" + SESSION, "#{pane_width}x#{pane_height}")
    return r.stdout.strip()

def attach(cols, rows):
    sock, hs = connect()
    rd = LineReader(sock)
    send_line(sock, {"token": hs["token"], "command": "attach_pty",
                     "args": {"sessionId": SESSION, "cols": cols, "rows": rows}, "v": 1})
    seed_line = rd.read_line(timeout=15)
    seed = json.loads(seed_line)
    if "scrollback" not in seed:
        raise SystemExit(f"FAIL: expected scrollback seed frame, got: {seed_line[:300]}")
    return sock, rd, b64d(seed["scrollback"]).decode("utf-8", "replace")

def collect_out(rd, want, timeout=8.0):
    """Read {out} frames until `want` appears in the decoded stream or timeout."""
    acc, frames, deadline = b"", 0, time.time() + timeout
    while time.time() < deadline:
        try:
            line = rd.read_line(timeout=max(0.2, deadline - time.time()))
        except Exception:
            break
        if line is None:
            break
        f = json.loads(line)
        if "out" in f:
            frames += 1
            acc += b64d(f["out"])
            if want.encode() in acc:
                break
        elif "exit" in f:
            return acc, frames, f["exit"]
    return acc, frames, None

# --- setup: disposable session, seed marker BEFORE attach ---
existing = tmux("ls").stdout
assert SESSION not in existing, f"session {SESSION} already exists; aborting"
r = tmux("new-session", "-d", "-s", SESSION, "-c", "/tmp")
assert r.returncode == 0, f"new-session failed: {r.stderr}"
tmux("send-keys", "-t", "=" + SESSION, "echo T1-SEED-MARKER", "Enter")
time.sleep(0.8)
print("[setup] session created; pre-attach geometry:", geometry())

# --- (a) scrollback seed ---
sock, rd, sb = attach(100, 30)
print("[a] seed frame received, %dB decoded; contains T1-SEED-MARKER: %s"
      % (len(sb), "T1-SEED-MARKER" in sb))
time.sleep(1.0)
print("[a] post-attach geometry (asked 100x30):", geometry())

# --- (b)+(c) write frame round-trips; live out frames stream back ---
send_line(sock, {"write": b64e(b"echo T1-LOOPBACK-CHECK\r")})
acc, nframes, _ = collect_out(rd, "T1-LOOPBACK-CHECK")
print("[b/c] out frames=%d decoded=%dB; echo round-trip visible: %s"
      % (nframes, len(acc), acc.count(b"T1-LOOPBACK-CHECK")))

# --- (d) resize frame changes pane geometry ---
g_before = geometry()
send_line(sock, {"resize": {"cols": 90, "rows": 25}})
time.sleep(1.2)
g_after = geometry()
print("[d] geometry before resize=%s after resize(90x25)=%s changed=%s"
      % (g_before, g_after, g_before != g_after))

# --- reconnect re-sync: drop mid-stream, re-attach, seed reflects prior output ---
send_line(sock, {"write": b64e(b"echo PRE-DISCONNECT-MARKER\r")})
acc2, _, _ = collect_out(rd, "PRE-DISCONNECT-MARKER", timeout=5)
sock.close()  # abrupt drop, no protocol goodbye
print("[4] dropped connection mid-stream (saw PRE-DISCONNECT-MARKER pre-drop: %s)"
      % (b"PRE-DISCONNECT-MARKER" in acc2))
time.sleep(1.5)
r = tmux("has-session", "-t", "=" + SESSION)
print("[4] tmux session survived the drop:", r.returncode == 0)

sock2, rd2, sb2 = attach(100, 30)
print("[4] re-attach seed %dB; contains T1-LOOPBACK-CHECK: %s; contains PRE-DISCONNECT-MARKER: %s"
      % (len(sb2), "T1-LOOPBACK-CHECK" in sb2, "PRE-DISCONNECT-MARKER" in sb2))
send_line(sock2, {"write": b64e(b"echo POST-RECONNECT-OK\r")})
acc3, nf3, _ = collect_out(rd2, "POST-RECONNECT-OK")
print("[4] post-reconnect write round-trip: %s (frames=%d)"
      % (acc3.count(b"POST-RECONNECT-OK"), nf3))
sock2.close()
print("PHASE-C-DONE")
