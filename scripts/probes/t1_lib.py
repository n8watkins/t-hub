"""Shared helpers for T1 server-split wire checks (loopback E2E)."""
import json, os, socket, base64, time, itertools

HS_PATH = os.path.expanduser("~/.t-hub/control.json")

# Spawn-class commands the app makes idempotent via a client `requestId`
# (mirrors the Rust `is_idempotent_command`).
IDEMPOTENT_COMMANDS = ("spawn_terminal", "create_worktree")

_REQ_COUNTER = itertools.count()

def new_request_id():
    """A process-unique idempotency key for a spawn-class command."""
    return "probe-%d-%d-%d" % (os.getpid(), time.time_ns(), next(_REQ_COUNTER))

def handshake():
    with open(HS_PATH) as f:
        return json.load(f)

def control_token(hs):
    """The token to use for CONTROL-tier calls, socket-gate Phase 3 aware.

    Prefer the spawn-tree-injected `T_HUB_CONTROL_TOKEN` (the full-power control
    token the app hands a session it spawns) when it is set. Fall back to
    `control.json`'s `token` for backward compatibility - but note that once Phase
    3 hardening is ON (its default), the file's `token` is the READ token, so a
    caller that is NOT running inside an app-spawned session gets read-only.
    """
    env_tok = os.environ.get("T_HUB_CONTROL_TOKEN")
    if env_tok:
        return env_tok
    return hs["token"]

def _connect_addr(addr, timeout):
    host, port = addr.rsplit(":", 1)
    return socket.create_connection((host, int(port)), timeout=timeout)

def connect(timeout=15.0):
    hs = handshake()
    # Discovery of the address still comes from control.json; the endpoint address
    # is not a secret. An explicit T_HUB_CONTROL_ADDR (paired with the env token) is
    # tried FIRST, but a session's env pin points at a DEAD port after any app
    # restart/rebind (the app rotates its ephemeral port every launch). So on a
    # connection failure we re-resolve the LIVE addr from control.json and retry,
    # KEEPING the env token - a restart rotates the port, not the token (adopt-first);
    # adopting control.json's token (READ-only once hardening is on) would silently
    # drop a control caller to read-only. Mirrors the Rust MCP client's stale-pin
    # recovery (`Discovery::refreshed_endpoint`).
    env_addr = os.environ.get("T_HUB_CONTROL_ADDR")
    env_pinned = bool(env_addr and os.environ.get("T_HUB_CONTROL_TOKEN"))
    addr = env_addr if env_pinned else hs["addr"]
    try:
        s = _connect_addr(addr, timeout)
    except OSError:
        # The pinned addr is dead. Re-read control.json for the addr the live app
        # just published; retry there only if it actually moved, else re-raise.
        fresh = handshake().get("addr")
        if not fresh or fresh == addr:
            raise
        s = _connect_addr(fresh, timeout)
    # Expose the Phase 3-aware control token on the handshake dict so callers that
    # do `tok = hs["token"]` keep working after the flip by reading hs["token"]
    # (now the effective control token when the env injects one). control_token
    # prefers the env token, so the fallback above keeps the caller's real
    # capability even after re-resolving the addr.
    hs = dict(hs)
    hs["token"] = control_token(hs)
    return s, hs

def send_line(sock, obj):
    sock.sendall((json.dumps(obj) + "\n").encode())

class LineReader:
    def __init__(self, sock):
        self.sock = sock
        self.buf = b""
    def read_line(self, timeout=15.0):
        self.sock.settimeout(timeout)
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                return None  # EOF
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return line.decode("utf-8", "replace")

def request(sock, reader, token, command, args=None, v=1, token_override=None):
    req = {"token": token_override if token_override is not None else token,
           "command": command, "args": args or {}}
    if v is not None:
        req["v"] = v
    send_line(sock, req)
    line = reader.read_line()
    return json.loads(line) if line is not None else None

def get_request_status(token, request_id, v=1):
    """Query the app for the outcome of a spawn-class `request_id` on a FRESH
    connection (the original one may be dead). Returns the parsed result dict
    ({status: completed|inFlight|unknown, ...}) or None on transport failure."""
    try:
        s, hs = connect()
    except OSError:
        return None
    try:
        reader = LineReader(s)
        resp = request(s, reader, token, "get_request_status",
                       {"requestId": request_id}, v=v)
        if resp and resp.get("ok"):
            return resp.get("result")
        return None
    finally:
        s.close()

def call_idempotent(token, command, args=None, v=1, resolve_deadline=30.0):
    """Spawn-class call that is SAFE to retry across an ambiguous response leg.

    Injects a `requestId` (reused across retries), and on a transport failure
    (the app accepted the command but the response never came - Incident A/B/D)
    resolves the true outcome via `get_request_status` instead of blindly
    retrying (which duplicates) or giving up (which loses the ghost):
      - completed(ok)  -> return the original result
      - completed(err) -> raise with the app's error
      - unknown        -> re-run ONCE with the same requestId (still idempotent)
      - inFlight       -> poll until it resolves or the deadline

    Returns the command's result dict. Raises RuntimeError on a resolved failure
    or an unresolvable ambiguity.
    """
    args = dict(args or {})
    if command in IDEMPOTENT_COMMANDS and "requestId" not in args:
        args["requestId"] = new_request_id()
    request_id = args.get("requestId")

    def _attempt():
        s, hs = connect()
        try:
            reader = LineReader(s)
            return request(s, reader, token, command, args, v=v)
        finally:
            s.close()

    try:
        resp = _attempt()
    except OSError:
        resp = None

    # A clean response: return it (or raise the app error) verbatim.
    if resp is not None:
        if resp.get("ok"):
            return resp.get("result")
        raise RuntimeError("%s failed: %s" % (command, resp.get("error")))

    # Ambiguous: the response leg failed. Resolve via get_request_status.
    if not request_id:
        raise RuntimeError("%s: no response and no requestId to resolve it" % command)

    deadline = time.time() + resolve_deadline
    while True:
        status = get_request_status(token, request_id)
        if status is not None:
            st = status.get("status")
            if st == "completed":
                if status.get("ok"):
                    return status.get("result")
                raise RuntimeError("%s failed: %s" % (command, status.get("error")))
            if st == "unknown":
                # Never landed: safe to re-run once with the same requestId.
                resp = _attempt()
                if resp and resp.get("ok"):
                    return resp.get("result")
                if resp:
                    raise RuntimeError("%s failed on retry: %s" % (command, resp.get("error")))
                raise RuntimeError("%s: retry produced no response" % command)
            # inFlight: fall through to the wait below.
        if time.time() >= deadline:
            raise RuntimeError(
                "%s: accepted (requestId %s) but still unresolved after %.0fs"
                % (command, request_id, resolve_deadline))
        time.sleep(0.5)

def b64d(s):
    return base64.b64decode(s)

def b64e(b):
    return base64.b64encode(b).decode()
