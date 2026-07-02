"""T12 acceptance driver: MCP organization continuity against the NATIVE client.

Runs against a PATCHED headless control server (`cargo run --example
control_probe_server`, T13a's harness server) with the native cockpit attached
as its UI, so the `control://apply` broadcast path is exercised end to end
WITHOUT touching the user's live app. The T12 handshake file path comes from
$T12_HS (the probe server was launched with T_HUB_CONTROL_FILE=$T12_HS; the
native client reads the same file via T_HUB_CONTROL_JSON).

For every audited MCP organization tool it drives the raw socket command and
verifies the native client's reaction through three independent signals:
  1. `list_tabs` - which the NATIVE client's `report_workspace_tabs` mirror
     keeps truthful (rename proves the round trip: the server never renames
     its registry itself);
  2. the native layout JSON on disk ($T12_LAYOUT) - tabs/tiles/active;
  3. a monitor event subscription logging every `control://apply` frame.

Creates ONLY disposable resources (a temp git repo + `th_*` sessions it spawns
itself) and kills them on exit.
"""
import json, os, socket, subprocess, sys, time

HS_PATH = os.environ.get("T12_HS", "/tmp/th-t12/control.json")
LAYOUT = os.environ.get("T12_LAYOUT", "/tmp/th-t12/native-layout.json")
WORK = os.path.dirname(HS_PATH)


def handshake():
    with open(HS_PATH) as f:
        return json.load(f)


class Conn:
    def __init__(self, timeout=15.0):
        hs = handshake()
        host, port = hs["addr"].rsplit(":", 1)
        self.token = hs["token"]
        self.sock = socket.create_connection((host, int(port)), timeout=timeout)
        self.buf = b""

    def send(self, obj):
        self.sock.sendall((json.dumps(obj) + "\n").encode())

    def read_line(self, timeout=15.0):
        self.sock.settimeout(timeout)
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                return None
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return line.decode("utf-8", "replace")

    def request(self, command, args=None):
        self.send({"token": self.token, "command": command, "args": args or {}, "v": 2})
        line = self.read_line()
        return json.loads(line) if line is not None else None


def wait_for(desc, pred, timeout=10.0, interval=0.25):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = pred()
        if v:
            print(f"  ok: {desc}")
            return v
        time.sleep(interval)
    raise AssertionError(f"TIMEOUT waiting for: {desc}")


def layout():
    try:
        with open(LAYOUT) as f:
            return json.load(f)
    except FileNotFoundError:
        return None


def tabs_by_name(c):
    r = c.request("list_tabs")
    assert r.get("ok"), r
    return {t["name"]: t for t in r["result"]["tabs"]}


def tabs_by_id(c):
    r = c.request("list_tabs")
    assert r.get("ok"), r
    return {t["id"]: t for t in r["result"]["tabs"]}


def sh(cmd, **kw):
    return subprocess.run(cmd, shell=True, check=True, capture_output=True, text=True, **kw)


def main():
    c = Conn()

    # A second subscription monitors the apply broadcasts (the server fanout
    # writes each frame to EVERY subscriber - the native client still gets its
    # own copy).
    mon = Conn()
    mon.send({"token": mon.token, "command": "__subscribe_events", "args": {}, "v": 2})
    ack = json.loads(mon.read_line())
    assert ack.get("ok") and ack["result"].get("subscribed"), ack
    applies = []

    def drain_monitor(quiet=True):
        mon.sock.settimeout(0.2)
        try:
            while True:
                line = mon.read_line(timeout=0.2)
                if line is None:
                    break
                frame = json.loads(line)
                if frame.get("event") == "control://apply":
                    applies.append(frame["payload"])
                    if not quiet:
                        print("  apply frame:", frame["payload"])
        except (socket.timeout, TimeoutError):
            pass

    print("== 0. native boot: initial tab report reaches list_tabs ==")
    wait_for(
        "native reported a tab layout with placed tiles",
        lambda: any(t["tileIds"] for t in tabs_by_name(c).values()),
        timeout=30,
    )
    boot_tabs = tabs_by_name(c)
    print("  tabs at boot:", {n: len(t["tileIds"]) for n, t in boot_tabs.items()})
    ws1_id = list(boot_tabs.values())[0]["id"]

    print("== 1. new_tab ==")
    r = c.request("new_tab", {"name": "T12 Ops"})
    assert r.get("ok") and r["result"]["applied"], r
    t12_id = r["result"]["tabId"]
    # Long timeout: right after a fresh boot the worker's first reconcile pass
    # attaches every placed tile sequentially, and applies only drain between
    # passes - the FIRST apply can trail the attach storm by ~15s. Steady-state
    # latency is one hint tick (~250ms-2s).
    wait_for(
        "native adopted the core-minted tab id (layout JSON + report)",
        lambda: (lambda l: l and any(t.get("id") == t12_id for t in l["tabs"]))(layout())
        and t12_id in tabs_by_id(c),
        timeout=60,
    )

    print("== 2. rename_tab (proves the native report round-trip) ==")
    r = c.request("rename_tab", {"tabId": t12_id, "name": "T12 Renamed"})
    assert r.get("ok") and r["result"]["applied"], r
    # The server never renames its own registry - only the native client's
    # report_workspace_tabs can make list_tabs show the new name.
    wait_for(
        "list_tabs shows the rename (native report round-trip)",
        lambda: tabs_by_id(c).get(t12_id, {}).get("name") == "T12 Renamed",
    )

    print("== 3. spawn_terminal (server-spawn native path) ==")
    r = c.request("spawn_terminal", {"cwd": "/tmp", "name": "t12-spawn"})
    assert r.get("ok") and r["result"].get("id"), r
    spawn_id = r["result"]["id"]
    print("  server minted:", spawn_id, r["result"]["tmuxSession"])
    wait_for(
        "tile placed in the ACTIVE (T12) tab",
        lambda: spawn_id in tabs_by_id(c).get(t12_id, {}).get("tileIds", []),
    )

    print("== 4. move_tile to Workspace 1 ==")
    r = c.request("move_tile", {"terminalId": spawn_id, "tabId": ws1_id})
    assert r.get("ok") and r["result"]["applied"], r
    wait_for(
        "tile relocated to Workspace 1 (registry + native agree)",
        lambda: spawn_id in tabs_by_id(c).get(ws1_id, {}).get("tileIds", [])
        and spawn_id not in tabs_by_id(c).get(t12_id, {}).get("tileIds", []),
    )

    print("== 5. focus_tab ==")
    r = c.request("focus_tab", {"tabId": t12_id})
    assert r.get("ok") and r["result"]["applied"], r
    wait_for(
        "native active workspace switched (layout JSON)",
        lambda: (lambda l: l and l["tabs"][l["active"]].get("id") == t12_id)(layout()),
    )

    print("== 6. focus_session (tile id activates its owning tab) ==")
    r = c.request("focus_session", {"sessionId": spawn_id})
    assert r.get("ok") and r["result"]["applied"], r
    wait_for(
        "native switched to the tile's owning workspace",
        lambda: (lambda l: l and l["tabs"][l["active"]].get("id") == ws1_id)(layout()),
    )

    print("== 7. create_worktree (named tab placement) ==")
    repo = os.path.join(WORK, "repo")
    wt = os.path.join(WORK, "repo-wt")
    sh(f"rm -rf {repo} {wt} && mkdir -p {repo}")
    sh(
        "git init -q -b main && git -c user.email=t@t -c user.name=t12 commit -q --allow-empty -m init",
        cwd=repo,
    )
    with open(os.path.join(repo, "hello.txt"), "w") as f:
        f.write("T12-OPEN-FILE-MARKER\n")
    sh("git add hello.txt && git -c user.email=t@t -c user.name=t12 commit -q -m f", cwd=repo)
    r = c.request(
        "create_worktree",
        {"repoRoot": repo, "worktreePath": wt, "branch": "t12-wt", "tabName": "t12-wt"},
    )
    assert r.get("ok") and r["result"]["applied"], r
    wt_tab = r["result"]["tabId"]
    wt_tile = wait_for(
        "worktree tab exists with its terminal placed in it",
        lambda: (tabs_by_id(c).get(wt_tab, {}).get("tileIds") or [None])[0],
    )
    # The spawned terminal really runs in the worktree dir.
    r = c.request("list_terminals")
    cwds = {t["id"]: t["cwd"] for t in r["result"]["terminals"]}
    assert cwds.get(wt_tile, "").rstrip("/") == wt, (wt_tile, cwds.get(wt_tile))
    print("  ok: worktree terminal cwd =", cwds[wt_tile])

    print("== 8. open_file (webview parity: contents returned, no UI mutation) ==")
    before = layout()
    r = c.request("open_file", {"path": os.path.join(repo, "hello.txt")})
    assert r.get("ok") and "T12-OPEN-FILE-MARKER" in json.dumps(r["result"]), r
    time.sleep(1.0)
    after = layout()
    assert before == after, "open_file must not change the native layout"
    print("  ok: contents served, native layout untouched")

    print("== 9. remove_worktree (sink-less refusal, documented deviation) ==")
    r = c.request("remove_worktree", {"repoRoot": repo, "worktreePath": wt})
    assert not r.get("ok") and "no UI is connected" in r.get("error", ""), r
    print("  ok: refused as documented (git side needs the webview until T14)")

    drain_monitor(quiet=False)
    seen = [p["command"] for p in applies]
    print("== monitor saw apply broadcasts:", seen)
    for expected in [
        "new_tab",
        "rename_tab",
        "spawn_terminal",
        "move_tile",
        "focus_tab",
        "focus_session",
        "add_worktree_workspace",
    ]:
        assert expected in seen, f"missing broadcast: {expected}"

    print("== cleanup ==")
    for sid in [spawn_id, wt_tile]:
        r = c.request("close_terminal", {"sessionId": sid})
        print("  close_terminal", sid, "->", r.get("ok"))
    wait_for(
        "closed tiles left every tab (native reconcile + report)",
        lambda: all(
            sid not in (t.get("tileIds") or [])
            for t in tabs_by_id(c).values()
            for sid in [spawn_id, wt_tile]
        ),
        timeout=15,
    )
    sh(f"git -C {repo} worktree remove --force {wt} || rm -rf {wt}")
    sh(f"rm -rf {repo} {wt}")

    print("T12-APPLY-OK")


if __name__ == "__main__":
    main()
