"""E2E verification of the in-app Scribe voice-gate's FILE FALLBACK path.

Drives the REAL Scribe dictation-state fallback file (status.json, the contract
section-6 mirror the running T-Hub app reads when Scribe's v1 endpoint is
unavailable) and observes the LIVE app's `scribe_status` answer over the
control socket. This is the same command the in-app voice watcher
(voiceAnnounce.ts) polls every ~250ms, so it exercises the real scribe.rs
fallback gate end to end: the `busy`/`dictating` gate (the deprecated
`listening` alias must be ignored), pid-liveness, the 15s `updatedAt` TTL
(contract section 7.2), and the fail-open doctrine.

PRECONDITION - Scribe must NOT be running: the app prefers Scribe's v1 status
endpoint (discovered via ~/.scribe/control.json), which SHADOWS this file
while it is reachable. The probe preflights that endpoint and exits with a
SKIP (code 2) if it answers, so a live Scribe can never produce misleading
FAILs here.

Safety: snapshots the original status.json bytes up front and ALWAYS restores
them in a finally block, so the general's real Scribe state is left untouched.
Run only while the general is NOT dictating (baseline listening=false).

Usage:  python3 scripts/probes/scribe_gate_e2e.py
Exit:   0 if every scenario matches the expected gate decision, 1 on FAIL,
        2 (SKIP) when the v1 endpoint is live and shadows the fallback file.
"""
import datetime
import json
import os
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import LineReader, connect, request

STATUS = os.environ.get(
    "SCRIBE_STATUS_FILE",
    "/mnt/c/Users/natha/AppData/Local/com.natkins.scribe/status.json",
)
CONTROL = os.environ.get(
    "SCRIBE_CONTROL_FILE",
    "/mnt/c/Users/natha/.scribe/control.json",
)
DEAD_PID = 4294967294  # a pid Windows will never hand out -> OpenProcess fails


def now_iso(offset_s=0):
    return (
        (
            datetime.datetime.now(datetime.timezone.utc)
            + datetime.timedelta(seconds=offset_s)
        )
        .isoformat()
        .replace("+00:00", "Z")
    )


def v1_endpoint_is_live():
    """True when Scribe's v1 status endpoint answers (it then shadows the
    fallback file inside the app, so file scenarios would be meaningless)."""
    try:
        with open(CONTROL) as f:
            ctl = json.load(f)
        base = ctl["baseUrl"].rstrip("/")
        path = ctl.get("endpoints", {}).get("status", "/v1/status")
        req = urllib.request.Request(
            base + path,
            headers={"Authorization": f"Bearer {ctl['readToken']}"},
        )
        with urllib.request.urlopen(req, timeout=1.5) as resp:
            return resp.status == 200
    except Exception:
        return False


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
    if v1_endpoint_is_live():
        print(f"[preflight] Scribe's v1 endpoint (via {CONTROL}) is LIVE.")
        print("[preflight] It shadows the status.json fallback inside the app,")
        print("[preflight] so the file scenarios below cannot be exercised.")
        print("[preflight] Quit Scribe and re-run to verify the fallback path.")
        print("SCRIBE-GATE-E2E-SKIP")
        return 2

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
    # (With Scribe quit - the preflight requirement - its old pid may be dead,
    # so the app pid is the usual choice here.)
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
            "dictating: busy+dictating + live pid + fresh -> GATE (true)",
            {"status": "Recording", "dictating": True, "busy": True,
             "listening": True, "since": now_iso(), "updatedAt": now_iso(),
             "pid": live_pid},
            True,
        ),
        (
            "busy-only (Transcribing): dictating=false    -> GATE (true)",
            {"status": "Transcribing", "dictating": False, "busy": True,
             "listening": False, "since": now_iso(), "updatedAt": now_iso(),
             "pid": live_pid},
            True,
        ),
        (
            "idle: busy=false                             -> SPEAK (false)",
            {"status": "Ready", "dictating": False, "busy": False,
             "listening": False, "since": now_iso(), "updatedAt": now_iso(),
             "pid": live_pid},
            False,
        ),
        (
            "alias-only legacy file (no dictating/busy)   -> SPEAK (false)",
            {"status": "Recording", "listening": True, "since": now_iso(),
             "updatedAt": now_iso(), "pid": live_pid},
            False,
        ),
        (
            "fail-open: busy=true but DEAD pid            -> SPEAK (false)",
            {"status": "Recording", "dictating": True, "busy": True,
             "since": now_iso(), "updatedAt": now_iso(), "pid": DEAD_PID},
            False,
        ),
        (
            "fail-open: stale updatedAt (20s) + live pid  -> SPEAK (false)",
            {"status": "Recording", "dictating": True, "busy": True,
             "since": now_iso(-20), "updatedAt": now_iso(-20),
             "pid": live_pid},
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
            "recover: busy+dictating + live pid again     -> GATE (true)",
            {"status": "Recording", "dictating": True, "busy": True,
             "since": now_iso(), "updatedAt": now_iso(), "pid": live_pid},
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
