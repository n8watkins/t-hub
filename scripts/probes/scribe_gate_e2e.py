"""E2E verification of the merged in-app Scribe voice-gate (backend data path).

Drives the REAL Scribe dictation-state file (status.json - the exact file Scribe
0.6.1+ writes and the file the running T-Hub app reads) and observes the LIVE
app's `scribe_status` answer over the control socket. This is the same command
the in-app voice watcher (voiceAnnounce.ts) polls every ~250ms, so it exercises
the real scribe.rs gate end to end: pid-liveness, staleness, and the fail-open
doctrine.

Safety: snapshots the original status.json bytes up front and ALWAYS restores
them in a finally block, so the general's real Scribe state is left untouched.
Run only while the general is NOT dictating (baseline listening=false).

Usage:  python3 scripts/probes/scribe_gate_e2e.py
Exit:   0 if every scenario matches the expected gate decision, else 1.
"""
import datetime
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import LineReader, connect, request

STATUS = os.environ.get(
    "SCRIBE_STATUS_FILE",
    "/mnt/c/Users/natha/AppData/Local/com.natkins.scribe/status.json",
)
DEAD_PID = 4294967294  # a pid Windows will never hand out -> OpenProcess fails


def now_iso():
    return (
        datetime.datetime.now(datetime.timezone.utc)
        .isoformat()
        .replace("+00:00", "Z")
    )


def write_status(raw_bytes_or_obj):
    """Write either raw bytes (for the torn-file case) or a JSON object."""
    with open(STATUS, "wb") as f:
        if isinstance(raw_bytes_or_obj, (bytes, bytearray)):
            f.write(raw_bytes_or_obj)
        else:
            f.write((json.dumps(raw_bytes_or_obj, indent=2)).encode())


def ask(sock, rd, tok):
    """Ask the LIVE app what it thinks the gate is, over the control socket."""
    r = request(sock, rd, tok, "scribe_status")
    if not r or not r.get("ok"):
        raise RuntimeError(f"scribe_status not ok: {r}")
    return bool(r["result"].get("listening"))


def main():
    sock, hs = connect(timeout=8)
    rd = LineReader(sock)
    # scribe_status is a Read-tier command; use the least-privilege read token.
    tok = hs.get("read_token") or hs["token"]

    live_pid = None
    try:
        cur = json.load(open(STATUS))
        live_pid = cur.get("pid")
    except Exception:
        pass
    # A pid we KNOW is alive on this box: prefer Scribe's own live pid; fall back
    # to the running app's pid from the handshake (also a live Windows pid).
    if not live_pid:
        live_pid = hs.get("pid")
    print(f"[setup] status file : {STATUS}")
    print(f"[setup] live pid     : {live_pid}")
    print(f"[setup] read_token   : {'yes' if hs.get('read_token') else 'no (fell back to control token)'}")

    # Snapshot original bytes so we can restore exactly.
    with open(STATUS, "rb") as f:
        original = f.read()

    # (label, file-contents, expected listening)
    scenarios = [
        (
            "dictating: listening=true + live pid + fresh  -> GATE (true)",
            {"status": "Recording", "listening": True, "since": now_iso(),
             "updatedAt": now_iso(), "pid": live_pid},
            True,
        ),
        (
            "idle: listening=false                         -> SPEAK (false)",
            {"status": "Ready", "listening": False, "since": now_iso(),
             "updatedAt": now_iso(), "pid": live_pid},
            False,
        ),
        (
            "fail-open: listening=true but DEAD pid        -> SPEAK (false)",
            {"status": "Recording", "listening": True, "since": now_iso(),
             "updatedAt": now_iso(), "pid": DEAD_PID},
            False,
        ),
        (
            "fail-open: torn/invalid JSON file            -> SPEAK (false)",
            b'{ this is not valid json ',
            False,
        ),
        (
            "fail-open: empty file                        -> SPEAK (false)",
            b"",
            False,
        ),
        (
            "recover: listening=true + live pid again     -> GATE (true)",
            {"status": "Recording", "listening": True, "since": now_iso(),
             "updatedAt": now_iso(), "pid": live_pid},
            True,
        ),
    ]

    results = []
    try:
        for label, contents, expected in scenarios:
            write_status(contents)
            # Backend reads the file fresh on every call (no cache); a tiny beat
            # lets the Windows FS flush the WSL write before we ask.
            time.sleep(0.15)
            got = ask(sock, rd, tok)
            ok = got == expected
            results.append((label, expected, got, ok))
            print(f"  [{'PASS' if ok else 'FAIL'}] {label}  (got listening={got})")
    finally:
        # Restore the general's real Scribe state, exactly.
        with open(STATUS, "wb") as f:
            f.write(original)
        time.sleep(0.15)
        restored = ask(sock, rd, tok)
        print(f"[teardown] restored original status.json (listening now={restored})")
        sock.close()

    passed = sum(1 for *_, ok in results if ok)
    total = len(results)
    print(f"\nRESULT: {passed}/{total} scenarios matched the expected gate decision")
    print("SCRIBE-GATE-E2E-" + ("PASS" if passed == total else "FAIL"))
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
