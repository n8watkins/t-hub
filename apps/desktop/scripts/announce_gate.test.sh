#!/usr/bin/env bash
# Conformance test for announce.sh's Scribe dictation gate (the P0 voice-gate
# fix). It drives the SHIPPED gate through `announce.sh --gate`, which prints the
# raw single-shot decision "<state> <source>" with no deferral/log/TTS, using the
# override seam (T_HUB_CONTROL_FILE / T_HUB_SCRIBE_CONTROL_FILE /
# T_HUB_SCRIBE_STATUS_FILE) so nothing touches the real T-Hub or Scribe.
#
# Two layers:
#   F1 - the full decision matrix incl. the incident cell (T-Hub unknown + Scribe
#        busy over a mock v1 socket -> HOLD), stale/future -> fail-open, and the
#        T-Hub-primary path (a mock control socket answering listening/idle).
#   F2 - a golden cross-check: the SAME gate-fixtures.json that the Rust suite
#        asserts against (scribe.rs::gate_matches_golden_fixtures_cross_impl) is
#        replayed through the shell gate here, asserting identical HOLD verdicts,
#        so Rust/shell contract drift turns this test red.
#
# All mock servers self-time-out, so a missed connection can never hang the run;
# every gate call is `timeout`-wrapped for the same reason. Exit 0 iff all pass.
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ANNOUNCE="$SCRIPT_DIR/announce.sh"
FIXTURES="$SCRIPT_DIR/gate-fixtures.json"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; pkill -P $$ 2>/dev/null' EXIT
MOCK_TTL=4  # seconds a mock server waits for its single connection before dying

PASS=0
FAIL=0

check() { # check <expected> <label> <actual>
  if [ "$3" = "$1" ]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf '  FAIL %s\n       expected: [%s]\n       got:      [%s]\n' "$2" "$1" "$3"
  fi
}

run_gate() { # run_gate [env assignments...] -> prints "<state> <source>"
  timeout 10 env "$@" "$ANNOUNCE" --gate 2>/dev/null
}

iso_now_offset() { # $1 = age in ms (negative => future); prints ISO-8601 UTC Z
  python3 - "$1" <<'PY'
import sys
from datetime import datetime, timezone, timedelta
print((datetime.now(timezone.utc) - timedelta(milliseconds=int(sys.argv[1]))).isoformat().replace("+00:00", "Z"))
PY
}

wait_port() { # wait_port <portfile> -> prints the port once written
  local pf="$1" i
  for i in $(seq 1 60); do [ -s "$pf" ] && { cat "$pf"; return; }; sleep 0.05; done
}

# ---------------------------------------------------------------------------
# F2 - golden cross-check: replay gate-fixtures.json through the shell gate.
# T-Hub is forced unknown, so scribe_direct decides via the single pinned file;
# we assert HOLD parity ("listening" iff the golden `hold` is true).
# ---------------------------------------------------------------------------
echo "F2 golden cross-check (gate-fixtures.json -> shell gate):"
while IFS=$'\t' read -r name hold age snap; do
  [ -n "$name" ] || continue
  f="$WORK/fix_$name.json"
  ua="$(iso_now_offset "$age")"
  UA="$ua" SNAP="$snap" python3 - "$f" <<'PY'
import json, os, sys
snap = json.loads(os.environ["SNAP"]); snap["updatedAt"] = os.environ["UA"]
json.dump(snap, open(sys.argv[1], "w"))
PY
  case "$hold" in True|true) exp="listening";; *) exp="notlistening";; esac
  state="$(run_gate T_HUB_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_STATUS_FILE="$f" | cut -d' ' -f1)"
  case "$state" in listening) got="listening";; *) got="notlistening";; esac
  check "$exp" "golden:$name (hold parity)" "$got"
done < <(python3 - "$FIXTURES" <<'PY'
import json, sys
for c in json.load(open(sys.argv[1]))["cases"]:
    print("\t".join([c["name"], str(c["hold"]), str(c["offsetMs"]), json.dumps(c["snapshot"])]))
PY
)

# ---------------------------------------------------------------------------
# F1 - full decision matrix (exact state + source), T-Hub forced unknown.
# ---------------------------------------------------------------------------
echo "F1 decision matrix (exact state + source):"
FRESH="$(iso_now_offset 2000)"
STALE="$(iso_now_offset 20000)"
FUTURE="$(iso_now_offset -60000)"
printf '{"app":"scribe","status":"Recording","dictating":true,"busy":true,"updatedAt":"%s"}\n' "$FRESH"  > "$WORK/busy.json"
printf '{"app":"scribe","status":"Idle","dictating":false,"busy":false,"updatedAt":"%s"}\n' "$FRESH"    > "$WORK/idle.json"
printf '{"app":"scribe","status":"Recording","busy":true,"updatedAt":"%s"}\n' "$STALE"                  > "$WORK/stale.json"
printf '{"app":"scribe","busy":true,"updatedAt":"%s"}\n' "$FUTURE"                                       > "$WORK/future.json"

check "idle scribe-direct"      "thub-unknown + scribe idle fresh (file)"     "$(run_gate T_HUB_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_STATUS_FILE="$WORK/idle.json")"
check "listening scribe-direct" "thub-unknown + scribe busy fresh (file)"     "$(run_gate T_HUB_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_STATUS_FILE="$WORK/busy.json")"
check "unknown fail-open"       "thub-unknown + scribe busy STALE -> failopen" "$(run_gate T_HUB_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_STATUS_FILE="$WORK/stale.json")"
check "unknown fail-open"       "thub-unknown + scribe busy FUTURE -> failopen" "$(run_gate T_HUB_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_STATUS_FILE="$WORK/future.json")"
check "unknown fail-open"       "thub-unknown + both down -> failopen"        "$(run_gate T_HUB_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_STATUS_FILE=/nonexistent)"

# F1 incident cell: T-Hub unknown + Scribe busy over a MOCK v1 loopback socket
# (the exact regression condition: T-Hub wedged, Scribe reachable+busy -> HOLD).
python3 - "$FRESH" "$WORK/v1port" "$MOCK_TTL" >/dev/null 2>&1 <<'PY' &
import sys, json, socketserver, http.server
fresh, portfile, ttl = sys.argv[1], sys.argv[2], int(sys.argv[3])
body = json.dumps({"schemaVersion":1,"app":"scribe","status":"Recording","dictating":True,"busy":True,"updatedAt":fresh,"pid":1}).encode()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.headers.get("Authorization") != "Bearer tok123":
            self.send_response(401); self.end_headers(); return
        self.send_response(200); self.send_header("Content-Type","application/json")
        self.send_header("Content-Length",str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self,*a): pass
srv = socketserver.TCPServer(("127.0.0.1",0), H); srv.timeout = ttl
open(portfile,"w").write(str(srv.server_address[1])); srv.handle_request()
PY
V1_PORT="$(wait_port "$WORK/v1port")"
printf '{"schemaVersion":1,"app":"scribe","baseUrl":"http://127.0.0.1:%s","endpoints":{"status":"/v1/status"},"readToken":"tok123"}\n' "$V1_PORT" > "$WORK/v1ctl.json"
check "listening scribe-direct" "INCIDENT: thub-unknown + scribe v1 busy (mock http) -> HOLD" \
  "$(run_gate T_HUB_CONTROL_FILE=/nonexistent T_HUB_SCRIBE_CONTROL_FILE="$WORK/v1ctl.json")"

# F1 T-Hub-primary path: a MOCK control socket answering the scribe_status
# command. When T-Hub answers we trust it (source thub-gate) without Scribe.
start_thub_mock() { # $1 listening(true/false) $2 portfile
  python3 - "$1" "$2" "$MOCK_TTL" >/dev/null 2>&1 <<'PY' &
import sys, json, socketserver
listening = sys.argv[1] == "true"; portfile = sys.argv[2]; ttl = int(sys.argv[3])
class H(socketserver.StreamRequestHandler):
    def handle(self):
        self.rfile.readline()
        self.wfile.write((json.dumps({"ok": True, "result": {"listening": listening}}) + "\n").encode())
class S(socketserver.TCPServer):
    allow_reuse_address = True
srv = S(("127.0.0.1", 0), H); srv.timeout = ttl
open(portfile, "w").write(str(srv.server_address[1])); srv.handle_request()
PY
}
start_thub_mock true "$WORK/tlport"
TPORT="$(wait_port "$WORK/tlport")"
printf '{"addr":"127.0.0.1:%s","read_token":"x"}\n' "$TPORT" > "$WORK/thub_listen.json"
check "listening thub-gate" "thub-primary answers listening -> HOLD (no scribe consult)" \
  "$(run_gate T_HUB_CONTROL_FILE="$WORK/thub_listen.json")"

start_thub_mock false "$WORK/tiport"
TPORT="$(wait_port "$WORK/tiport")"
printf '{"addr":"127.0.0.1:%s","read_token":"x"}\n' "$TPORT" > "$WORK/thub_idle.json"
check "idle thub-gate" "thub-primary answers idle -> speak" \
  "$(run_gate T_HUB_CONTROL_FILE="$WORK/thub_idle.json")"

# ---------------------------------------------------------------------------
echo "---------------------------------------------"
printf 'announce.sh gate: %d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
