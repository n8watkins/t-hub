#!/usr/bin/env bash
# Captain voice announcements via the local TTS servers.
# Usage: announce.sh "text to speak" [voice]
# Settings: ~/.t-hub/captain/voice.json {enabled, engine (kokoro|piper), voice, volume 0..1, sapiRate}
#   kokoro server = 127.0.0.1:7478, piper server = 127.0.0.1:7477
# Falls back to Windows SAPI if the selected engine's server is down.
#
# SCRIBE VOICE-GATE (canonical): before speaking, this consults the T-Hub app's
# AUTHORITATIVE scribe_status over the control socket - the SAME source of truth
# the in-app voice watcher (voiceAnnounce.ts) uses. The app (scribe.rs) asks
# Scribe's v1 status endpoint (loopback HTTP, discovered via ~/.scribe/
# control.json) whether the general is inside a dictation cycle (the `busy`
# flag), falling back to Scribe's status.json file (pid-liveness + 15s TTL)
# only when the endpoint is unavailable, and already FAILS OPEN (reports
# not-listening) whenever it cannot positively confirm active dictation.
#
# WEDGE DEFENSE (P0 fix): the T-Hub control socket is a proxy, and it can be
# unreachable OR wedged (flapping windows where a bare TCP connect succeeds but
# the request round-trips hang until timeout). In that case the socket gate
# returns "unknown" and we would fail open and SPEAK - talking over the general
# even though Scribe itself is healthy and reporting busy:true, because we never
# asked Scribe. So the T-Hub socket stays the PRIMARY gate (authoritative,
# combines prod+dev, matches the in-app path), but when it answers "unknown" we
# fall back to asking SCRIBE DIRECTLY - a faithful reimplementation of the same
# v1-first + status.json check scribe.rs performs (app=="scribe", 15s updatedAt
# TTL, future-reject, busy flag, prod+dev OR'd). We only fail open and speak
# when BOTH T-Hub and Scribe cannot positively confirm state, so a wedged T-Hub
# can no longer make us talk over an active dictation.
#
# Behavior while the general dictates: DEFER (wait, bounded), then speak shortly
# after they stop - matching the in-app hold+flush, so a cue is never dropped.
# FAIL OPEN (speak now) if the control socket / app is unreachable or the answer
# is anything but a confident "listening" - voice is never lost when the app or
# Scribe is closed.
#
# Divergence from the in-app path (documented, deliberate): the in-app watcher
# coalesces held cues to a single latest announcement. This shell path is a
# one-shot process per cue, so it cannot coalesce ACROSS invocations without
# shared state; multiple deferred cues will each speak (roughly together) once
# dictation stops. For the low-frequency captain path this is acceptable and
# upholds "never lose voice"; it is preferred over dropping.
#
# Env overrides:
#   SCRIBE_GATE_DISABLE=1   never gate (always speak now)
#   SCRIBE_DEFER_MAX_S=120  hard cap on deferral; at the cap we speak anyway so a
#                           cue is never lost (fail open) even if dictation runs long
#   SCRIBE_POLL_S=0.3       re-poll interval while deferring
#   SCRIBE_TAIL_S=0.5       quiet tail after dictation stops before speaking
#   T_HUB_CONTROL_FILE / T_HUB_CONTROL_ADDR / T_HUB_CONTROL_TOKEN  (see control.json)
#   T_HUB_SCRIBE_CONTROL_FILE / T_HUB_SCRIBE_STATUS_FILE  pin the direct-Scribe
#                           fallback to exact files (tests / single-flavor E2E),
#                           mirroring scribe.rs's override seam; when unset the
#                           fallback resolves prod + dev from the host defaults
set -u
# Test/E2E seam: `announce.sh --gate` runs ONLY the dictation gate and prints its
# raw single-shot decision ("<state> <source>"), with no voice.json read, no
# deferral, no log, no TTS. The conformance + golden cross-check tests drive this
# via the override env (T_HUB_CONTROL_FILE / T_HUB_SCRIBE_CONTROL_FILE /
# T_HUB_SCRIBE_STATUS_FILE), so they exercise the SHIPPED gate, not a copy.
GATE_ONLY=0
[ "${1:-}" = "--gate" ] && GATE_ONLY=1
TEXT="${1:?usage: announce.sh \"text\" [voice]  (or: announce.sh --gate)}"
CFG="$HOME/.t-hub/captain/voice.json"

if [ "$GATE_ONLY" -eq 0 ]; then
  read -r ENABLED ENGINE CFG_VOICE VOLUME SAPI_RATE <<EOF
$(python3 - "$CFG" <<'PY'
import json, sys
try:
    c = json.load(open(sys.argv[1]))
except Exception:
    c = {}
eng = c.get("engine", "kokoro")
default_voice = "af_heart" if eng == "kokoro" else "en_US-ryan-high.onnx"
print(str(c.get("enabled", True)).lower(),
      eng,
      c.get("voice", default_voice),
      c.get("volume", 0.6),
      c.get("sapiRate", 0))
PY
)
EOF
  [ "$ENABLED" = "true" ] || exit 0
fi

# --- Scribe dictation gate -------------------------------------------------
# Ask the app's authoritative gate: prints "listening" | "idle" | "unknown".
# "unknown" means the socket/app is unreachable or the answer was malformed ->
# we treat it as SPEAK (fail open). scribe_status is a Read-tier command, so we
# use the least-privilege read_token (falling back to the full token / an
# env-injected control token when present).
scribe_state() {
  python3 - <<'PY'
import json, os, re, socket, sys, time, urllib.request
from datetime import datetime, timezone

SOCK_TIMEOUT = 2.0        # T-Hub control-socket round-trip budget (per poll)
SCRIBE_HTTP_TIMEOUT = 1.5 # direct GET /v1/status budget (loopback, tight)
SNAPSHOT_TTL_MS = 15000   # same 15s updatedAt TTL scribe.rs enforces

def now_ms():
    return int(datetime.now(timezone.utc).timestamp() * 1000)

def parse_iso_ms(s):
    """Scribe emits RFC3339 UTC with a Z and up to 9 sub-second digits;
    truncate the fraction to 6 so fromisoformat can parse it."""
    if not isinstance(s, str):
        return None
    s = s.strip()
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    m = re.match(r"^(.*\.\d{6})\d*([+-]\d{2}:\d{2})$", s)
    if m:
        s = m.group(1) + m.group(2)
    try:
        dt = datetime.fromisoformat(s)
    except Exception:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return int(dt.timestamp() * 1000)

def fresh(updated_at, now):
    t = parse_iso_ms(updated_at)
    # present, parseable, not future, within the TTL (mirrors snapshot_is_fresh)
    return t is not None and t <= now and now - t <= SNAPSHOT_TTL_MS

def snapshot_state(snap):
    """One parsed snapshot (v1 or file) -> 'listening'/'idle'/None(untrusted).
    None means fall through: not Scribe, stale/future updatedAt, or no boolean."""
    if not isinstance(snap, dict):
        return None
    if snap.get("app") is not None and snap.get("app") != "scribe":
        return None
    if not fresh(snap.get("updatedAt"), now_ms()):
        return None
    busy = snap.get("busy")
    if busy is None:
        busy = snap.get("dictating")  # dictating stands in only when busy absent
    if busy is None:
        return None
    return "listening" if busy else "idle"

# --- Primary: the T-Hub app's authoritative control-socket gate --------------
def thub_gate():
    hs_path = os.environ.get("T_HUB_CONTROL_FILE") or os.path.expanduser("~/.t-hub/control.json")
    try:
        hs = json.load(open(hs_path))
    except Exception:
        return "unknown"
    env_tok = os.environ.get("T_HUB_CONTROL_TOKEN")
    env_addr = os.environ.get("T_HUB_CONTROL_ADDR")
    addr = env_addr if (env_addr and env_tok) else hs.get("addr")
    tok = env_tok or hs.get("read_token") or hs.get("token")
    if not addr or not tok:
        return "unknown"
    try:
        # Absolute wall-clock deadline: a slow trickle (many partial recvs, each
        # under the per-read timeout) must not let a wedged socket outlast the
        # whole SOCK_TIMEOUT budget. We shrink the per-read timeout toward the
        # deadline and bail the instant it passes -> "unknown" -> fail toward the
        # Scribe-direct fallback rather than parking the poll.
        deadline = time.monotonic() + SOCK_TIMEOUT
        host, port = addr.rsplit(":", 1)
        s = socket.create_connection((host, int(port)), timeout=SOCK_TIMEOUT)
        req = {"token": tok, "command": "scribe_status", "args": {}, "v": 1}
        s.sendall((json.dumps(req) + "\n").encode())
        buf = b""
        while b"\n" not in buf:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            s.settimeout(remaining)
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        s.close()
        resp = json.loads(buf.split(b"\n", 1)[0].decode("utf-8", "replace"))
    except Exception:
        return "unknown"  # unreachable OR wedged (connect ok, round-trip hangs)
    if not resp.get("ok"):
        return "unknown"
    result = resp.get("result") or {}
    return "listening" if result.get("listening") else "idle"

# --- Fallback: ask Scribe directly when T-Hub can't answer -------------------
def first_existing(*paths):
    for p in paths:
        if p and os.path.exists(p):
            return p
    return None

def scribe_control_dir():
    # Native Linux Scribe first, then the WSL->Windows home this host uses.
    return first_existing(os.path.expanduser("~/.scribe"), "/mnt/c/Users/natha/.scribe")

def scribe_cache_file(bundle):
    # status.json in the Tauri per-app cache dir (Linux ~/.cache, else Windows LOCALAPPDATA).
    return first_existing(
        os.path.join(os.path.expanduser("~/.cache"), bundle, "status.json"),
        os.path.join("/mnt/c/Users/natha/AppData/Local", bundle, "status.json"),
    )

def v1_flavor(control_path):
    """GET this flavor's /v1/status -> 'listening'/'idle'/None(unreachable/untrusted)."""
    if not control_path:
        return None
    try:
        c = json.load(open(control_path))
    except Exception:
        return None
    base = c.get("baseUrl")
    tok = c.get("readToken")
    if not base or not tok:
        return None
    path = (c.get("endpoints") or {}).get("status") or "/v1/status"
    try:
        req = urllib.request.Request(
            base.rstrip("/") + path, headers={"Authorization": "Bearer " + tok}
        )
        # Force a DIRECT connection: an http(s)_proxy / ALL_PROXY env var must not
        # reroute a 127.0.0.1 GET (a proxy would mangle or refuse the loopback
        # call and silently break the gate). An empty ProxyHandler disables all.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(req, timeout=SCRIBE_HTTP_TIMEOUT) as r:
            snap = json.loads(r.read().decode("utf-8", "replace"))
    except Exception:
        return None  # refused/closed/timeout/401 -> endpoint unavailable
    return snapshot_state(snap)

def file_flavor(status_path):
    """This flavor's status.json -> 'listening'/'idle'/None. pid-liveness is
    skipped (a Windows pid is not checkable from the WSL shell); the 15s TTL +
    v1 reachability cover a dead producer in the safe (defer) direction."""
    if not status_path:
        return None
    try:
        snap = json.load(open(status_path))
    except Exception:
        return None
    return snapshot_state(snap)

def scribe_direct():
    ctl_override = os.environ.get("T_HUB_SCRIBE_CONTROL_FILE")
    file_override = os.environ.get("T_HUB_SCRIBE_STATUS_FILE")
    if ctl_override or file_override:
        # Test / single-flavor seam: exactly the pinned source(s).
        r = v1_flavor(ctl_override) if ctl_override else None
        if r is None:
            r = file_flavor(file_override)
        return r or "unknown"
    ctl_dir = scribe_control_dir()
    flavors = [
        (os.path.join(ctl_dir, "control.json") if ctl_dir else None, scribe_cache_file("com.natkins.scribe")),
        (os.path.join(ctl_dir, "control.dev.json") if ctl_dir else None, scribe_cache_file("com.natkins.scribe.dev")),
    ]
    saw_idle = False
    for ctl, status_file in flavors:
        r = v1_flavor(ctl)
        if r is None:
            r = file_flavor(status_file)
        if r == "listening":
            return "listening"  # OR: either flavor busy holds
        if r == "idle":
            saw_idle = True
    return "idle" if saw_idle else "unknown"

# Print "<state> <source>": the final gate state and which source decided it,
# so the caller can log WHY it spoke or held (thub-gate / scribe-direct / fail-open).
st = thub_gate()
src = "thub-gate"
if st == "unknown":
    st = scribe_direct()  # T-Hub can't answer -> ask Scribe before speaking
    src = "scribe-direct" if st != "unknown" else "fail-open"
print(st, src)
PY
}

# Append one line per invocation to the captain announce log: timestamp, caller,
# gate decision + deciding source + state, and the (bounded) utterance. This
# closes the "exact utterance unrecoverable" gap the P0 report called out - a
# talked-over-the-general event is now always attributable after the fact.
# Cheap: a plain append, with a trivial size guard that keeps the tail.
announce_log() {
  local decision="$1" state="$2" source="$3"
  local logf="$HOME/.t-hub/captain/announce.log"
  mkdir -p "$(dirname "$logf")" 2>/dev/null || true
  # Size guard (~1 MiB): keep the last 500 lines, no cron/rotation needed.
  if [ -f "$logf" ] && [ "$(wc -c <"$logf" 2>/dev/null || echo 0)" -gt 1048576 ]; then
    # Concurrent announce.sh invocations can trim at the same moment; a per-run
    # mktemp in the SAME dir (then an atomic rename) keeps them from clobbering
    # a shared fixed-name tmp and truncating each other's rewrite.
    local tmp
    tmp="$(mktemp "$(dirname "$logf")/.announce.log.XXXXXX" 2>/dev/null)" || tmp=""
    if [ -n "$tmp" ] && tail -n 500 "$logf" > "$tmp" 2>/dev/null; then
      mv -f "$tmp" "$logf" 2>/dev/null || rm -f "$tmp" 2>/dev/null
    elif [ -n "$tmp" ]; then
      rm -f "$tmp" 2>/dev/null
    fi
  fi
  local ts caller text
  ts="$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ 2>/dev/null || date -u +%Y-%m-%dT%H:%M:%SZ)"
  # Caller: explicit override wins; else best-effort parent process name.
  caller="${T_HUB_ANNOUNCE_CALLER:-$(ps -o comm= -p "$PPID" 2>/dev/null | tr -d '[:space:]')}"
  caller="${caller:-unknown}"
  # One line, bounded: strip newlines/tabs, cap length, then escape backslashes
  # and double-quotes so the quoted text="..." field can never be broken open.
  text="$(printf '%s' "$TEXT" | tr '\n\t' '  ' | cut -c1-200)"
  text="${text//\\/\\\\}"
  text="${text//\"/\\\"}"
  printf '%s\tcaller=%s\tdecision=%s\tsource=%s\tstate=%s\ttext="%s"\n' \
    "$ts" "$caller" "$decision" "$source" "$state" "$text" >> "$logf" 2>/dev/null || true
}

# --gate seam: print the raw single-shot gate decision and exit (no defer/log/TTS).
if [ "$GATE_ONLY" -eq 1 ]; then
  scribe_state
  exit 0
fi

DECISION="speak"; GATE_ST="unknown"; GATE_SRC="disabled"
if [ "${SCRIBE_GATE_DISABLE:-0}" != "1" ]; then
  DEFER_MAX_S="${SCRIBE_DEFER_MAX_S:-120}"
  POLL_S="${SCRIBE_POLL_S:-0.3}"
  TAIL_S="${SCRIBE_TAIL_S:-0.5}"
  read -r GATE_ST GATE_SRC <<< "$(scribe_state)"
  if [ "$GATE_ST" = "listening" ]; then
    # Defer: wait (bounded) for dictation to stop, re-polling the authoritative
    # gate. Any non-"listening" answer (idle OR unknown/fail-open) breaks the
    # loop and we speak. At the cap we speak anyway so a cue is never lost.
    DECISION="deferred"
    SECONDS=0
    while [ "$GATE_ST" = "listening" ] && [ "$SECONDS" -lt "$DEFER_MAX_S" ]; do
      sleep "$POLL_S"
      read -r GATE_ST GATE_SRC <<< "$(scribe_state)"
    done
    # Quiet tail in case they resume immediately (mirrors the in-app flush delay).
    sleep "$TAIL_S"
  fi
else
  DECISION="gate-disabled"
fi
# Record the decision (final state + deciding source) before we speak.
announce_log "$DECISION" "$GATE_ST" "$GATE_SRC"
# --- end Scribe dictation gate ---------------------------------------------

VOICE="${2:-$CFG_VOICE}"
if [ "$ENGINE" = "piper" ]; then PORT=7477; else PORT=7478; fi

WAV="$(mktemp /mnt/c/Users/natha/Downloads/.thub-announce-XXXXXX.wav)"
trap 'rm -f "$WAV"' EXIT
WAV_WIN="C:/Users/natha/Downloads/$(basename "$WAV")"

BODY="$(TEXT="$TEXT" VOICE="$VOICE" python3 -c 'import json, os; print(json.dumps({"text": os.environ["TEXT"], "voice": os.environ["VOICE"]}))')"

CODE=$(curl -s -m 30 -X POST http://127.0.0.1:$PORT/tts \
  -H 'Content-Type: application/json' \
  -d "$BODY" \
  -o "$WAV" -w "%{http_code}")

if [ "$CODE" = "200" ]; then
  powershell.exe -NoProfile -Command "
    Add-Type -AssemblyName PresentationCore;
    \$p = New-Object System.Windows.Media.MediaPlayer;
    \$p.Open([uri]'file:///$WAV_WIN');
    \$p.Volume = $VOLUME;
    while (-not \$p.NaturalDuration.HasTimeSpan) { Start-Sleep -Milliseconds 50 };
    \$p.Play();
    Start-Sleep -Milliseconds ([int]\$p.NaturalDuration.TimeSpan.TotalMilliseconds + 200);
    \$p.Close()" >/dev/null 2>&1
else
  TEXT_PS="$(printf '%s' "$TEXT" | sed "s/'/''/g")"
  powershell.exe -NoProfile -Command "
    Add-Type -AssemblyName System.Speech;
    \$s = New-Object System.Speech.Synthesis.SpeechSynthesizer;
    \$s.Rate = $SAPI_RATE; \$s.Volume = [int]($VOLUME * 100);
    \$s.Speak('$TEXT_PS')" >/dev/null 2>&1
fi
