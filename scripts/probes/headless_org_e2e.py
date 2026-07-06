"""Headless-org E2E acceptance driver (requirements 1-3) over the control socket.

Runs against a REAL dev instance of the webview app (isolated namespace:
T_HUB_CONTROL_FILE=$HO_HS, T_HUB_TMUX_SOCKET=t-hub-e2e), exercising every
general requirement end to end:

  1. NO FOCUS STEAL + HONORED PLACEMENT - spawn_terminal with tabName lands in
     that (hidden) tab; the UI's OWN up-synced activeTabId proves the view
     never moved.
  2. HEADLESS ORGANIZATION - move_tile into a hidden tab applies and SURVIVES
     the UI's report cycle (the old lost-update repro); rename_tab of a hidden
     tab applies too.
  3. TAB LIFECYCLE - close_terminal drops tiles out of their (hidden) tab;
     close_tab reaps the emptied tab headlessly; the last tab is refused.

Every assertion reads back through the socket (list_tabs / list_terminals /
read_terminal), and the placement assertions are re-checked AFTER the live UI's
reporter has had time to echo - so a stale-report clobber would fail the probe.

Usage: HO_HS=/tmp/th-e2e/control.json python3 scripts/probes/headless_org_e2e.py
"""
import json
import os
import socket
import sys
import time

HS_PATH = os.environ.get("HO_HS", "/tmp/th-e2e/control.json")


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

    def call(self, command, args=None):
        req = {"token": self.token, "command": command, "args": args or {}, "v": 2}
        self.sock.sendall((json.dumps(req) + "\n").encode())
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("connection closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def ok(self, command, args=None):
        res = self.call(command, args)
        if not res.get("ok"):
            raise RuntimeError(f"{command} failed: {res.get('error')}")
        return res["result"]

    def err(self, command, args=None):
        res = self.call(command, args)
        if res.get("ok"):
            raise RuntimeError(f"{command} unexpectedly succeeded: {res}")
        return res["error"]


PASS = 0


def check(label, cond, detail=""):
    global PASS
    mark = "PASS" if cond else "FAIL"
    print(f"  [{mark}] {label}" + (f"  ({detail})" if detail else ""))
    if not cond:
        sys.exit(f"E2E FAILED at: {label} {detail}")
    PASS += 1


def tabs_by_name(c):
    lt = c.ok("list_tabs")
    return {t["name"]: t for t in lt["tabs"]}, lt


def wait_for(pred, timeout=10.0, every=0.25):
    deadline = time.time() + timeout
    while time.time() < deadline:
        v = pred()
        if v:
            return v
        time.sleep(every)
    return None


def main():
    c = Conn()
    print(f"== headless-org E2E against {HS_PATH} ({handshake()['addr']}) ==")

    # -- Baseline: the live UI has reported its layout + active tab. -----------
    lt = wait_for(lambda: (lambda r: r if r.get("activeTabId") else None)(c.ok("list_tabs")))
    check("UI reported layout (activeTabId known)", lt is not None, json.dumps(lt))
    active_before = lt["activeTabId"]
    active_name = next(t["name"] for t in lt["tabs"] if t["id"] == active_before)
    print(f"  user's active tab: '{active_name}' ({active_before})")

    # ===== Requirement 1: spawn into a NAMED HIDDEN tab, no focus steal ======
    print("\n[1] spawn_terminal tabName=e2e-hidden while another tab is focused")
    r = c.ok("spawn_terminal", {"cwd": "/tmp", "name": "ho-probe", "tabName": "e2e-hidden"})
    tid1 = r["id"]
    check("spawn returned a real id synchronously", bool(tid1), tid1)
    check("spawn reports placed:true", r.get("placed") is True)
    hidden_id = r["tabId"]
    check("a tab id was resolved for the name", bool(hidden_id), hidden_id)
    check("placement did NOT reuse the active tab", hidden_id != active_before)

    by_name, lt = tabs_by_name(c)
    check("registry shows tab 'e2e-hidden'", "e2e-hidden" in by_name)
    check("tile is IN the hidden tab", tid1 in by_name["e2e-hidden"]["tileIds"])
    check(
        "user's view did not change (registry activeTabId)",
        lt["activeTabId"] == active_before,
        f"active={lt['activeTabId']}",
    )

    # The UI adopts the snapshot and up-syncs; after its report cycle the
    # placement must STILL hold and the UI-reported active tab must be unchanged.
    def ui_echoed():
        by, l = tabs_by_name(c)
        return (
            (by, l)
            if "e2e-hidden" in by and tid1 in by["e2e-hidden"]["tileIds"]
            else None
        )

    echoed = wait_for(ui_echoed, timeout=8)
    check("placement survives the UI report cycle", echoed is not None)
    _, lt = echoed
    check(
        "UI's own report confirms the view never moved",
        lt["activeTabId"] == active_before,
        f"active={lt['activeTabId']}",
    )
    term_live = c.ok("read_terminal", {"sessionId": tid1})
    check("spawned session is live and readable", "text" in term_live)

    # ===== Requirement 2: headless move_tile into the hidden tab =============
    print("\n[2] move_tile into the hidden tab must apply + survive reports")
    r = c.ok("spawn_terminal", {"cwd": "/tmp", "name": "ho-move-me"})
    tid2 = r["id"]
    check("second spawn landed in the ACTIVE tab by default", r["tabId"] == active_before, r["tabId"])

    c.ok("move_tile", {"terminalId": tid2, "tabId": hidden_id})
    by_name, lt = tabs_by_name(c)
    check("registry reflects the move immediately", tid2 in by_name["e2e-hidden"]["tileIds"])

    # THE morning repro: wait out several UI report cycles; the move must not
    # be clobbered back (stale reports are rejected now).
    time.sleep(3)
    by_name, lt = tabs_by_name(c)
    check("move SURVIVES the UI's report cycles (no lost update)", tid2 in by_name["e2e-hidden"]["tileIds"])
    check("view still unchanged after move", lt["activeTabId"] == active_before)

    err = c.err("move_tile", {"terminalId": tid2, "tabId": "no-such-tab"})
    check("move_tile to an unknown tab is a HARD error", "unknown tabId" in err, err)

    print("\n[2b] rename_tab on the hidden tab applies headlessly")
    c.ok("rename_tab", {"tabId": hidden_id, "name": "e2e-renamed"})
    by_name, _ = tabs_by_name(c)
    check("registry shows the rename", "e2e-renamed" in by_name)
    renamed = wait_for(
        lambda: (lambda b: b if "e2e-renamed" in b[0] else None)(tabs_by_name(c)), timeout=8
    )
    check("rename survives the UI report cycle", renamed is not None)

    # ===== Requirement 3: headless tab lifecycle ==============================
    print("\n[3] close_terminal empties the tab; close_tab reaps it headlessly")
    err = c.err("close_tab", {"tabId": hidden_id})
    check("close_tab refuses a NON-EMPTY tab without force", "close its terminals first" in err, err)

    for tid in (tid1, tid2):
        c.ok("close_terminal", {"sessionId": tid})
    by_name, _ = tabs_by_name(c)
    check("closed tiles left the (hidden) tab", by_name["e2e-renamed"]["tileIds"] == [])

    c.ok("close_tab", {"tabId": hidden_id})
    by_name, lt = tabs_by_name(c)
    check("emptied tab closed headlessly", "e2e-renamed" not in by_name)
    check("view STILL unchanged through the whole lifecycle", lt["activeTabId"] == active_before)

    gone = wait_for(
        lambda: None if "e2e-renamed" in tabs_by_name(c)[0] else True, timeout=8
    )
    check("tab close survives the UI report cycle", gone is True)

    if len(lt["tabs"]) == 1:
        err = c.err("close_tab", {"tabId": lt["tabs"][0]["id"]})
        check("the LAST tab is refused", "last tab" in err, err)

    sessions = c.ok("list_terminals")
    leaked = [t for t in sessions["terminals"] if t["id"] in (tid1, tid2)]
    check("no leaked sessions", leaked == [])

    print(f"\n== ALL {PASS} CHECKS PASSED ==")


if __name__ == "__main__":
    main()
