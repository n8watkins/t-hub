# T1 device-B verification recipe (M2b remote wire)

> Produced by the T1 verification run (2026-07-01).
> The loopback half of T1 PASSED end-to-end on app v0.3.27 (commands, protocol-version gate, auth, event fanout, PTY seed/out/write/resize, mid-stream drop + re-sync; see `NATIVE-RENDER-PIVOT.md` §7 T1).
> This file is the remaining device-B half, ready to run.

Server = the Windows host running T-Hub (WSL distro Ubuntu-24.04, default shell zsh).
Device B = any second machine (Win/Mac/Linux) running the SAME T-Hub build (protocol v1 must match; a skewed build fails fast with "protocol version mismatch: server v1, client vN").

## 0. Which token a remote client uses

The PERSISTENT server key - NOT a per-launch secret: `C:\Users\natha\.t-hub\server-key` on the server (from WSL: `/mnt/c/Users/natha/.t-hub/server-key`).
It is identical to the `token` field of `control.json` (verified on this box) because the server uses `persistent_key()` as its control token (`lib.rs` ~189-192, `control.rs::persistent_key` ~344).
If `T_HUB_CONTROL_TOKEN` is set at server launch it overrides both.
The client env var is `T_HUB_REMOTE_TOKEN` (NOT `T_HUB_CONTROL_TOKEN`, which is the server-side override).

## 1. Install Tailscale (both machines)

Windows host:

```powershell
winget install --id Tailscale.Tailscale
tailscale up          # sign in; then:
tailscale ip -4       # note the 100.x.y.z address
```

Device B: install Tailscale, join the SAME tailnet, `tailscale ip -4` should show a 100.64/10 address.
Note: `control.rs::tailscale_ip4()` shells out to plain `tailscale` - it must be on the PATH of the process that launches T-Hub (a fresh PowerShell after install is fine).

## 2. Relaunch T-Hub on the server with the remote bind (opt-in, default OFF)

Note on safety (corrected 2026-07-01 against `workspace.ts:1270`): sessions SURVIVE an app quit - the Tier-3 reap fires ONLY on the workspace-tab ×, never on app exit; tmux sessions stay detached and native session restore re-attaches tiles on boot.
A relaunch just blanks the UI briefly; agents keep running headless.
PowerShell (env vars set in a shell are inherited by GUI apps it launches):

```powershell
$env:T_HUB_BIND_TAILSCALE = '1'
$env:T_HUB_CONTROL_PORT   = '8790'   # default 8787 collides with workerd in WSL on this box (mirrored networking)
$app = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' |
        Where-Object DisplayName -eq 'T-Hub').InstallLocation
& (Join-Path $app 'T-Hub.exe')
```

Expected: stderr logs "control listener ALSO bound on 100.x.y.z:8790 for REMOTE access (token-gated; loopback + Tailscale peers only)" (launch from a console to see it).
`control.json` still shows the loopback addr - the remote bind is an ADDITIONAL listener serving the same dispatch.
Explicit alternative: `$env:T_HUB_CONTROL_BIND = '100.x.y.z:8790'` (explicit wins over TAILSCALE=1).
Unset the vars afterwards (`Remove-Item Env:T_HUB_*`) - stale `T_HUB_*` env is a known foot-gun on this box.

## 3. Smoke-test the remote listener from device B (before the full client)

```bash
python3 - <<'EOF'
import socket, json
s = socket.create_connection(("100.x.y.z", 8790), timeout=5)
s.sendall((json.dumps({"token":"<server-key>","command":"list_terminals","args":{},"v":1})+"\n").encode())
print(s.makefile().readline())
EOF
```

Expected: `{"ok":true,"result":{"terminals":[...],"count":N}}` matching the server's tmux sessions.
Bad token -> `{"ok":false,"error":"unauthorized: bad control token"}`.
`"v":2` -> the version-mismatch error.

## 4. Thin-client launch on device B

```powershell
$env:T_HUB_REMOTE_ADDR  = '100.x.y.z:8790'
$env:T_HUB_REMOTE_TOKEN = '<contents of server-key>'
& <path-to-T-Hub.exe>      # same-version build
```

(macOS/Linux: `T_HUB_REMOTE_ADDR=... T_HUB_REMOTE_TOKEN=... ./t-hub`.)
Expected: stderr "t-hub: REMOTE client mode - control endpoint = 100.x.y.z:8790"; the sidebar/tiles show the SERVER's sessions; opening a tile does attach_pty over the tailnet ({scrollback} seed, live {out}, keystrokes as {write}, resize follows); events (session://status, supervision://tree, status://snapshot...) stream in.
Overlay reads (recent/usage/git/files) resolve on the SERVER.

## 5. LAN negative test (prove is_allowed_peer rejects non-tailnet peers)

Relaunch the server once with an all-interfaces bind:

```powershell
$env:T_HUB_CONTROL_BIND = '0.0.0.0:8790'
```

From a NON-tailnet LAN machine (Tailscale disconnected - otherwise traffic may arrive via 100.x and pass), run the step-3 probe against the server's LAN IP (192.168.x.x:8790) WITH the VALID token.
Expected: TCP connect SUCCEEDS (the bind accepts) but the server closes the connection immediately with NO response line - `is_allowed_peer` (`control.rs` ~485-511) rejects the peer BEFORE auth, so even a valid token gets nothing.
From a tailnet peer (source 100.64/10) the same probe succeeds.
Caveats: loopback is ALWAYS allowed (with mirrored WSL networking any local process reaches a 0.0.0.0 bind - the token still gates); do not run the "LAN peer" probe on the server itself.

## 6. Reconnect test (device B)

With a tile attached and producing output: `tailscale down` (or toggle wifi) on device B.
Expected: the tile tears down cleanly (reader EOF -> Exited state, no hang); the event forwarder retries with 250ms->10s backoff (`control_client.rs` ~160-199).
`tailscale up`, reopen the session's tile.
Expected: the {scrollback} seed reflects everything the session printed while disconnected (server-side tmux is the source of truth - verified on loopback in the T1 run).

## 7. Restore

Relaunch T-Hub without any `T_HUB_*` env -> loopback-only again (verify `control.json` + no remote log line).

## 8. Findings from the first live run (2026-07-01, v0.3.28)

What was actually observed when this recipe was executed on the dev box:

- **`T_HUB_BIND_TAILSCALE=1` silently no-ops if `tailscale` is not on the app's PATH.**
  The WSL-interop environment snapshot predates a same-day Tailscale install, so the launched app could not run `tailscale ip -4`.
  Use the explicit `T_HUB_CONTROL_BIND=<tailnet-ip>:8790` (verified working: listener came up on `100.70.116.93:8790`), or prepend `C:\Program Files\Tailscale` to PATH before launching.
- **Plain TCP from WSL to the host's tailnet IP times out** - the Hyper-V/WSL firewall layer eats the mirrored hairpin before it reaches the listener.
  Not a T-Hub bug; do not use plain sockets from WSL to test the remote bind.
- **`tailscale nc <host-tailnet-ip> 8790` from WSL connects but is dropped pre-auth** - and this is the peer gate WORKING:
  under mirrored networking, same-host traffic (even from the WSL tailscaled node) arrives with a non-tailnet-looking source, and `is_allowed_peer` closes it without a response, exactly as designed (bad tokens get an explicit `unauthorized` reply; disallowed peers get silence).
  **Consequence: this machine cannot serve as its own device B. The positive test requires a physically separate device.**
- **Positive test from a real second device (phone, Termux, Tailscale VPN on):**

  ```bash
  pkg install -y python
  python - <<'EOF'
  import socket, json
  s = socket.create_connection(("100.70.116.93", 8790), timeout=8)
  s.sendall((json.dumps({"token": "<contents of C:\\Users\\natha\\.t-hub\\server-key>",
                         "command": "list_terminals", "args": {}, "v": 1}) + "\n").encode())
  print(s.makefile().readline())
  EOF
  ```

  Expected: `{"ok":true,...,"count":N}`.
  **✅ CONFIRMED 2026-07-01: an Android phone (Termux, bash `/dev/tcp`, Tailscale VPN on) received `{"ok":true,...}` from `100.70.116.93:8790` - the genuine device-B positive.
  T1 is CLOSED.** (The §5 LAN negative with a `0.0.0.0` bind remains optional/untested; the tailnet-scoped bind + observed pre-auth gate rejection covers the shipped posture.)
- Everything else re-verified on the new build: boot, 13/13 session restore across two app bounces, `git_info` returning real branches (the v0.3.28 fix), loopback wire intact.
- **Daily-use note:** the remote bind is env-var-driven and per-launch; a normal launch reverts to loopback-only.
  If remote should be always-on, promote it to a persisted setting (follow-up).
