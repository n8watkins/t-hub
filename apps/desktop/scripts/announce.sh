#!/usr/bin/env bash
# Captain voice announcements via the local TTS servers.
# Usage: announce.sh "text to speak" [voice]
# Settings: ~/.t-hub/captain/voice.json {enabled, engine (kokoro|piper), voice, volume 0..1, sapiRate}
#   kokoro server = 127.0.0.1:7478, piper server = 127.0.0.1:7477
# Falls back to Windows SAPI if the selected engine's server is down.
#
# SCRIBE VOICE-GATE (canonical): before speaking, this consults the T-Hub app's
# AUTHORITATIVE scribe_status over the control socket - the SAME source of truth
# the in-app voice watcher (voiceAnnounce.ts) uses. The app (scribe.rs) computes
# "is the general dictating?" from Scribe's status file WITH pid-liveness and a
# staleness backstop, and already FAILS OPEN (reports not-listening) whenever it
# cannot positively confirm active dictation. So this script does NOT re-read the
# status file or re-implement a weaker file-only check; it asks the one gate.
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
set -u
TEXT="${1:?usage: announce.sh \"text\" [voice]}"
CFG="$HOME/.t-hub/captain/voice.json"

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

# --- Scribe dictation gate -------------------------------------------------
# Ask the app's authoritative gate: prints "listening" | "idle" | "unknown".
# "unknown" means the socket/app is unreachable or the answer was malformed ->
# we treat it as SPEAK (fail open). scribe_status is a Read-tier command, so we
# use the least-privilege read_token (falling back to the full token / an
# env-injected control token when present).
scribe_state() {
  python3 - <<'PY'
import json, os, socket, sys

def out(s):
    print(s)
    sys.exit(0)

hs_path = os.environ.get("T_HUB_CONTROL_FILE") or os.path.expanduser("~/.t-hub/control.json")
try:
    hs = json.load(open(hs_path))
except Exception:
    out("unknown")

env_tok = os.environ.get("T_HUB_CONTROL_TOKEN")
env_addr = os.environ.get("T_HUB_CONTROL_ADDR")
addr = env_addr if (env_addr and env_tok) else hs.get("addr")
tok = env_tok or hs.get("read_token") or hs.get("token")
if not addr or not tok:
    out("unknown")

try:
    host, port = addr.rsplit(":", 1)
    s = socket.create_connection((host, int(port)), timeout=2.0)
    s.settimeout(2.0)
    req = {"token": tok, "command": "scribe_status", "args": {}, "v": 1}
    s.sendall((json.dumps(req) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    s.close()
    resp = json.loads(buf.split(b"\n", 1)[0].decode("utf-8", "replace"))
except Exception:
    out("unknown")

if not resp.get("ok"):
    out("unknown")
result = resp.get("result") or {}
out("listening" if result.get("listening") else "idle")
PY
}

if [ "${SCRIBE_GATE_DISABLE:-0}" != "1" ]; then
  DEFER_MAX_S="${SCRIBE_DEFER_MAX_S:-120}"
  POLL_S="${SCRIBE_POLL_S:-0.3}"
  TAIL_S="${SCRIBE_TAIL_S:-0.5}"
  ST="$(scribe_state)"
  if [ "$ST" = "listening" ]; then
    # Defer: wait (bounded) for dictation to stop, re-polling the authoritative
    # gate. Any non-"listening" answer (idle OR unknown/fail-open) breaks the
    # loop and we speak. At the cap we speak anyway so a cue is never lost.
    SECONDS=0
    while [ "$ST" = "listening" ] && [ "$SECONDS" -lt "$DEFER_MAX_S" ]; do
      sleep "$POLL_S"
      ST="$(scribe_state)"
    done
    # Quiet tail in case they resume immediately (mirrors the in-app flush delay).
    sleep "$TAIL_S"
  fi
fi
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
