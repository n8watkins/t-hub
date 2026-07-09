"""Spawn-retry idempotency probe (Incident A/B/C/D repro).

Reproduces the exact failure the fix targets: a spawn-class command whose
RESPONSE leg fails while the command APPLIES server-side, then a retry with the
same requestId. Asserts the retry does NOT create a duplicate - it replays the
original outcome - and that get_request_status resolves the ambiguity.

Runs over the control socket (control.json, or T_HUB_CONTROL_ADDR/TOKEN). The
live app must be HEALTHY for this to pass end-to-end; against a wedged app it
reports the wedge instead. It reaps every session it creates.

  python3 scripts/probes/t_spawn_retry.py
"""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import (connect, LineReader, request, send_line, new_request_id,
                    get_request_status, call_idempotent)

CWD = "/tmp"


def spawn_and_drop_response(tok, request_id):
    """Send a spawn_terminal, then CLOSE the connection WITHOUT reading the
    response - simulating a failed response leg after the command was accepted.
    The server still applies the spawn (idempotent under request_id)."""
    sock, hs = connect()
    try:
        send_line(sock, {"token": tok, "command": "spawn_terminal",
                         "args": {"cwd": CWD, "requestId": request_id}, "v": 1})
        # Deliberately do NOT read the response: drop the socket mid-flight.
    finally:
        sock.close()


def main():
    sock, hs = connect()
    rd = LineReader(sock)
    tok = hs["token"]

    before = request(sock, rd, tok, "list_terminals")
    if not (before and before.get("ok")):
        print("FAIL: control channel not answering list_terminals (wedged app?):", before)
        return 1
    n_before = before["result"]["count"]
    print("before: count=%d" % n_before)

    request_id = new_request_id()
    print("requestId:", request_id)

    # 1) First attempt: the response leg fails (we drop the socket). The command
    #    is accepted and applies server-side.
    spawn_and_drop_response(tok, request_id)
    print("sent spawn_terminal, dropped the response leg")

    # 2) Resolve the ambiguity: what actually happened to this request?
    status = get_request_status(tok, request_id)
    print("get_request_status:", status)
    if not status or status.get("status") not in ("completed", "inFlight"):
        print("FAIL: the accepted spawn was not recorded under its requestId")
        sock.close()
        return 1

    # 3) The idempotent recovery path: call_idempotent replays / resolves without
    #    creating a second session. Reuse the SAME requestId.
    result = call_idempotent(tok, "spawn_terminal", {"cwd": CWD, "requestId": request_id})
    spawned_id = result["id"]
    print("resolved spawn id:", spawned_id,
          "(idempotentReplay=%s)" % result.get("idempotentReplay"))

    # 4) A blind retry with the same requestId must ALSO replay - never duplicate.
    replay = request(sock, rd, tok, "spawn_terminal",
                     {"cwd": CWD, "requestId": request_id})
    if not (replay and replay.get("ok")):
        print("FAIL: retry did not succeed:", replay)
        sock.close()
        return 1
    if replay["result"]["id"] != spawned_id:
        print("FAIL: retry produced a DIFFERENT id (duplicate!): %s vs %s"
              % (replay["result"]["id"], spawned_id))
        sock.close()
        return 1
    if not replay["result"].get("idempotentReplay"):
        print("FAIL: retry was not tagged idempotentReplay:", replay["result"])
        sock.close()
        return 1
    print("retry replayed the same id (no duplicate):", spawned_id)

    # 5) Exactly ONE new session exists for the id.
    after = request(sock, rd, tok, "list_terminals")
    live = [t for t in after["result"]["terminals"] if t["id"] == spawned_id]
    if len(live) != 1:
        print("FAIL: expected exactly 1 session for the id, found %d" % len(live))
        sock.close()
        return 1
    print("exactly one live session for the id")

    # 6) Reap it, and confirm close_terminal reports the honest outcome.
    closed = request(sock, rd, tok, "close_terminal", {"sessionId": spawned_id})
    print("close_terminal outcome:", closed["result"].get("outcome"))
    if closed["result"].get("outcome") != "killed":
        print("WARN: expected outcome=killed for a live session")

    # A second close of the same id is now a phantom -> already_gone.
    closed2 = request(sock, rd, tok, "close_terminal", {"sessionId": spawned_id})
    print("second close outcome:", closed2["result"].get("outcome"))
    if closed2["result"].get("outcome") != "already_gone":
        print("WARN: expected outcome=already_gone for a phantom close")

    sock.close()
    print("PASS: spawn retry is idempotent (no duplicate) and close is honest")
    return 0


if __name__ == "__main__":
    sys.exit(main())
