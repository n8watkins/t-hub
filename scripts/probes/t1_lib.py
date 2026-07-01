"""Shared helpers for T1 server-split wire checks (loopback E2E)."""
import json, os, socket, base64, time

HS_PATH = os.path.expanduser("~/.t-hub/control.json")

def handshake():
    with open(HS_PATH) as f:
        return json.load(f)

def connect(timeout=15.0):
    hs = handshake()
    host, port = hs["addr"].rsplit(":", 1)
    s = socket.create_connection((host, int(port)), timeout=timeout)
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
