"""Phase B: subscribe to the event stream, capture frames for ~60s to stdout."""
import json, sys, time
import os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t1_lib import connect, LineReader, send_line

sock, hs = connect()
rd = LineReader(sock)
send_line(sock, {"token": hs["token"], "command": "__subscribe_events", "args": {}, "v": 1})
ack = rd.read_line(timeout=10)
print(json.dumps({"t": time.time(), "ack": json.loads(ack)}), flush=True)

deadline = time.time() + 60
while time.time() < deadline:
    try:
        line = rd.read_line(timeout=max(0.5, deadline - time.time()))
    except Exception:
        break
    if line is None:
        print(json.dumps({"t": time.time(), "eof": True}), flush=True)
        break
    try:
        frame = json.loads(line)
    except Exception:
        frame = {"raw": line[:200]}
    print(json.dumps({"t": round(time.time(), 3), "frame": frame}), flush=True)
sock.close()
print(json.dumps({"done": True}), flush=True)
