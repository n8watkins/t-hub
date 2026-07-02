"""T13a: binary PTY framing (PROTOCOL_VERSION 2) - server-half proof.

Proves three things against the live control socket, on ONE disposable tmux
session (th_t13-binframe), which it creates and kills:

  1. V2 client: opt in with attach_pty arg `"binary": true`, read a binary
     SCROLLBACK frame, write a binary WRITE frame, read binary OUT frames, and
     resize with a binary RESIZE frame - full round-trip, no base64, no JSON.
  2. V1 client (regression): the historical base64-NDJSON framing still works
     unchanged (attach without `binary`), proving the live webview is unaffected.
  3. Byte reduction: run an identical firehose over V2 and measure the wire cost
     of the base64+NDJSON tax the V1 framing would have paid for the SAME output
     frames (exact, per-frame), plus a real measured V1 firehose cross-check.

Only ever touches th_t13-binframe.

Server: by default this launches its OWN headless control listener (the
`control_probe_server` example) on a TEMP handshake file, so it never touches the
user's live app or ~/.t-hub/control.json. Point it at an already-running server
instead by exporting T_HUB_T13_HANDSHAKE=/path/to/control.json.
"""
import atexit, json, math, os, shutil, socket, struct, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import send_line, b64d, b64e, LineReader

SESSION = "th_t13-binframe"
HERE = os.path.dirname(os.path.abspath(__file__))
SRC_TAURI = os.path.normpath(os.path.join(HERE, "..", "..", "apps", "desktop", "src-tauri"))
PROBE_TOKEN = "t13-probe-token"


def connect_to(handshake_path, timeout=15.0):
    with open(handshake_path) as f:
        hs = json.load(f)
    host, port = hs["addr"].rsplit(":", 1)
    s = socket.create_connection((host, int(port)), timeout=timeout)
    return s, hs


def launch_probe_server():
    """Build + launch the headless control listener; return (handshake_path, proc).

    Returns (path, None) if T_HUB_T13_HANDSHAKE points at an already-running server.
    """
    ext = os.environ.get("T_HUB_T13_HANDSHAKE")
    if ext:
        print(f"[server] using external handshake at {ext}")
        return ext, None

    print("[server] building control_probe_server example (cargo)...")
    build = subprocess.run(
        ["cargo", "build", "--example", "control_probe_server"],
        cwd=SRC_TAURI, capture_output=True, text=True)
    if build.returncode != 0:
        print(build.stdout[-2000:]); print(build.stderr[-2000:])
        raise SystemExit("FAIL: could not build control_probe_server example")
    exe = os.path.join(SRC_TAURI, "target", "debug", "examples", "control_probe_server")
    tmpdir = tempfile.mkdtemp(prefix="th-t13-")
    handshake_path = os.path.join(tmpdir, "control.json")
    env = dict(os.environ, T_HUB_CONTROL_FILE=handshake_path)
    env.pop("T_HUB_CONTROL_ADDR", None)
    env.pop("T_HUB_CONTROL_TOKEN", None)
    proc = subprocess.Popen([exe, PROBE_TOKEN], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def _cleanup():
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
        shutil.rmtree(tmpdir, ignore_errors=True)
    atexit.register(_cleanup)

    deadline = time.time() + 20
    while time.time() < deadline:
        if os.path.exists(handshake_path) and os.path.getsize(handshake_path) > 0:
            time.sleep(0.2)
            print(f"[server] headless listener up; handshake at {handshake_path}")
            return handshake_path, proc
        if proc.poll() is not None:
            raise SystemExit("FAIL: control_probe_server exited before writing handshake")
        time.sleep(0.1)
    raise SystemExit("FAIL: control_probe_server never wrote a handshake file")


HANDSHAKE_PATH, _SERVER = launch_probe_server()


def connect():
    return connect_to(HANDSHAKE_PATH)

# Binary frame type tags (mirror pty::binframe in the Rust server).
OUT, EXIT, SCROLLBACK, ERROR, WRITE, RESIZE = 0x01, 0x02, 0x03, 0x04, 0x10, 0x11


def tmux(*args):
    return subprocess.run(["tmux", "-L", "t-hub", *args], capture_output=True, text=True)


def geometry():
    # list-panes (not `display -p`) reports pane size reliably whether or not a
    # client is attached; `display -p` returns empty for a detached session.
    r = tmux("list-panes", "-t", SESSION, "-F", "#{pane_width}x#{pane_height}")
    return r.stdout.strip().splitlines()[0] if r.stdout.strip() else "?"


class BinReader:
    """Reads length-prefixed binary frames ([u8 type][u32 BE len][payload])."""

    def __init__(self, sock):
        self.sock = sock
        self.buf = b""

    def _fill(self, n, timeout):
        self.sock.settimeout(timeout)
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("socket closed mid-frame")
            self.buf += chunk

    def read_frame(self, timeout=10.0):
        self._fill(5, timeout)
        ty = self.buf[0]
        length = int.from_bytes(self.buf[1:5], "big")
        self._fill(5 + length, timeout)
        payload = self.buf[5:5 + length]
        self.buf = self.buf[5 + length:]
        return ty, payload


def send_bin(sock, ty, payload=b""):
    sock.sendall(bytes([ty]) + len(payload).to_bytes(4, "big") + payload)


# --------------------------------------------------------------------------
# setup: disposable session, seed marker BEFORE attach
# --------------------------------------------------------------------------
existing = tmux("ls").stdout
assert SESSION not in existing, f"session {SESSION} already exists; aborting"
r = tmux("new-session", "-d", "-s", SESSION, "-c", "/tmp", "-x", "100", "-y", "30")
assert r.returncode == 0, f"new-session failed: {r.stderr}"


def seed_marker(marker="T13-SEED-MARKER", tries=20):
    """Type `echo <marker>` and poll capture-pane until it renders. Robust to a
    cold WSL shell that isn't ready to read keystrokes the instant the pane opens."""
    for _ in range(tries):
        tmux("send-keys", "-t", SESSION, f"echo {marker}", "Enter")
        for _ in range(5):
            time.sleep(0.25)
            cap = tmux("capture-pane", "-p", "-t", SESSION).stdout
            # the marker appears twice once it runs (the command line + its output);
            # require 2 so we don't match the un-executed command line alone
            if cap.count(marker) >= 2:
                return True
    return False


seeded = seed_marker()
print("[setup] session created; geometry:", geometry(), "| seed rendered:", seeded)

fail = False


def check(label, ok):
    global fail
    print(f"  [{'PASS' if ok else 'FAIL'}] {label}")
    if not ok:
        fail = True


# ==========================================================================
# (1) V2 binary client
# ==========================================================================
print("\n[V2] binary framing client")
sock, hs = connect()
send_line(sock, {"token": hs["token"], "command": "attach_pty",
                 "args": {"sessionId": SESSION, "cols": 100, "rows": 30, "binary": True}, "v": 2})
br = BinReader(sock)

ty, payload = br.read_frame(15)
check("opening frame is a binary SCROLLBACK frame", ty == SCROLLBACK)
sb = payload.decode("utf-8", "replace")
check("scrollback seed carries T13-SEED-MARKER", "T13-SEED-MARKER" in sb)
print(f"       scrollback {len(payload)}B (binary, no base64)")


def collect_out_bin(br, want, timeout=8.0):
    acc, frames, deadline = b"", 0, time.time() + timeout
    while time.time() < deadline:
        try:
            ty, payload = br.read_frame(max(0.2, deadline - time.time()))
        except (socket.timeout, EOFError):
            break
        if ty == OUT:
            frames += 1
            acc += payload
            if want in acc:
                break
        elif ty == EXIT:
            return acc, frames, payload
    return acc, frames, None


# binary WRITE round-trip
send_bin(sock, WRITE, b"echo T13-V2-LOOPBACK\r")
acc, nframes, _ = collect_out_bin(br, b"T13-V2-LOOPBACK")
check("binary WRITE round-trips as binary OUT frames",
      acc.count(b"T13-V2-LOOPBACK") >= 1 and nframes > 0)
print(f"       out frames={nframes} decoded={len(acc)}B")

# binary RESIZE
g_before = geometry()
send_bin(sock, RESIZE, struct.pack(">HH", 90, 25))
time.sleep(1.2)
g_after = geometry()
check(f"binary RESIZE changed geometry {g_before} -> {g_after}", g_before != g_after)

# --------------------------------------------------------------------------
# firehose over V2: capture OUT payload sizes to price both framings exactly
# --------------------------------------------------------------------------
print("\n[V2] firehose (seq 1 50000) - measuring the wire")
send_bin(sock, WRITE, b"seq 1 50000; echo T13-FIREHOSE-DONE\r")
frame_lens, payload_total, marker_seen = [], 0, False
deadline = time.time() + 30.0
tail = b""
while time.time() < deadline:
    try:
        ty, payload = br.read_frame(1.2)
    except (socket.timeout, EOFError):
        if marker_seen:
            break
        continue
    if ty != OUT:
        continue
    frame_lens.append(len(payload))
    payload_total += len(payload)
    tail = (tail + payload)[-64:]
    if b"T13-FIREHOSE-DONE" in tail:
        marker_seen = True
        # keep draining briefly to catch the final redraw frames, then stop
        drain_deadline = time.time() + 1.0
        while time.time() < drain_deadline:
            try:
                ty, payload = br.read_frame(0.4)
            except (socket.timeout, EOFError):
                break
            if ty == OUT:
                frame_lens.append(len(payload))
                payload_total += len(payload)
        break
sock.close()
check("firehose marker observed over V2", marker_seen)

nframes = len(frame_lens)
# V2 wire cost for these frames: 5-byte header + raw payload each.
v2_wire = sum(5 + n for n in frame_lens)
# V1 wire cost the SAME frames would have paid: {"out":"<b64>"}\n
#   envelope = len('{"out":"') + len('"}') + len('\n') = 8 + 2 + 1 = 11 bytes
#   base64 of L bytes = 4*ceil(L/3) bytes
v1_wire = sum(11 + 4 * math.ceil(n / 3) for n in frame_lens)
reduction = 1 - (v2_wire / v1_wire) if v1_wire else 0.0
print(f"       frames={nframes} payload={payload_total}B")
print(f"       V1 wire (base64+NDJSON, same frames) = {v1_wire}B")
print(f"       V2 wire (binary)                     = {v2_wire}B")
print(f"       reduction = {reduction*100:.1f}%  (V2 is {v2_wire/v1_wire*100:.1f}% of V1)")
check("V2 firehose is materially smaller on the wire (>20% reduction)", reduction > 0.20)


# ==========================================================================
# (2) V1 client regression: base64-NDJSON still works unchanged
# ==========================================================================
print("\n[V1] base64-NDJSON client (regression - what the webview speaks)")
sock2, hs2 = connect()
rd = LineReader(sock2)
send_line(sock2, {"token": hs2["token"], "command": "attach_pty",
                  "args": {"sessionId": SESSION, "cols": 100, "rows": 30}, "v": 1})
seed = json.loads(rd.read_line(timeout=15))
check("opening frame is JSON {\"scrollback\"}", "scrollback" in seed)
sb2 = b64d(seed["scrollback"]).decode("utf-8", "replace")
# After the V2 firehose the tail of the pane is seq output near 50000, not the
# early markers - so just prove the v1 base64 scrollback decodes to real content.
check("V1 scrollback decodes to non-empty pane content", len(sb2) > 0)


def collect_out_v1(rd, want, timeout=8.0):
    acc, frames, wire, deadline = b"", 0, 0, time.time() + timeout
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
            wire += len(line) + 1
            acc += b64d(f["out"])
            if want in acc:
                break
        elif "exit" in f:
            break
    return acc, frames, wire


send_line(sock2, {"write": b64e(b"echo T13-V1-LOOPBACK\r")})
acc1, nf1, _ = collect_out_v1(rd, b"T13-V1-LOOPBACK")
check("V1 JSON write round-trips as {\"out\"} frames",
      acc1.count(b"T13-V1-LOOPBACK") >= 1 and nf1 > 0)

g_before = geometry()
send_line(sock2, {"resize": {"cols": 110, "rows": 40}})
time.sleep(1.2)
g_after = geometry()
check(f"V1 JSON resize changed geometry {g_before} -> {g_after}", g_before != g_after)

# a real measured V1 firehose, as an independent cross-check on the reduction
print("\n[V1] firehose (seq 1 50000) - real measured wire cross-check")
send_line(sock2, {"write": b64e(b"seq 1 50000; echo T13-V1-FIREHOSE-DONE\r")})
v1_meas_wire, v1_meas_payload, v1_frames, seen = 0, 0, 0, False
deadline = time.time() + 30.0
tail = b""
while time.time() < deadline:
    try:
        line = rd.read_line(timeout=1.2)
    except Exception:
        if seen:
            break
        continue
    if line is None:
        break
    f = json.loads(line)
    if "out" not in f:
        continue
    v1_frames += 1
    v1_meas_wire += len(line) + 1
    dec = b64d(f["out"])
    v1_meas_payload += len(dec)
    tail = (tail + dec)[-64:]
    if b"T13-V1-FIREHOSE-DONE" in tail:
        seen = True
        break
sock2.close()
print(f"       V1 measured: frames={v1_frames} payload={v1_meas_payload}B wire={v1_meas_wire}B "
      f"(tax={v1_meas_wire/max(1,v1_meas_payload)*100-100:.1f}% over raw)")

# --------------------------------------------------------------------------
# teardown
# --------------------------------------------------------------------------
tmux("kill-session", "-t", SESSION)
print("\n[teardown] killed", SESSION)

print("\nRESULT:", "FAIL" if fail else "T13-BINFRAME-OK")
sys.exit(1 if fail else 0)
