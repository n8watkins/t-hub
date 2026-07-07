"""Shared helpers for T1 server-split wire checks (loopback E2E)."""
import json, os, socket, base64, time

HS_PATH = os.path.expanduser("~/.t-hub/control.json")

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

def connect(timeout=15.0):
    hs = handshake()
    # Discovery of the address still comes from control.json; the endpoint address
    # is not a secret. An explicit T_HUB_CONTROL_ADDR (paired with the env token)
    # overrides it - the same all-or-nothing pairing the Rust MCP client uses.
    env_addr = os.environ.get("T_HUB_CONTROL_ADDR")
    addr = env_addr if (env_addr and os.environ.get("T_HUB_CONTROL_TOKEN")) else hs["addr"]
    host, port = addr.rsplit(":", 1)
    s = socket.create_connection((host, int(port)), timeout=timeout)
    # Expose the Phase 3-aware control token on the handshake dict so callers that
    # do `tok = hs["token"]` keep working after the flip by reading hs["token"]
    # (now the effective control token when the env injects one).
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

def b64d(s):
    return base64.b64decode(s)

def b64e(b):
    return base64.b64encode(b).decode()
