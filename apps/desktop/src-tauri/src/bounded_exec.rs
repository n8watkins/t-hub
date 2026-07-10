//! Bounded subprocess execution — the single choke point that guarantees NO
//! child process spawned on a control-handler thread can park it indefinitely.
//!
//! This is the residual control-flap fix, factored out of `tmux.rs` so BOTH the
//! tmux orchestration layer AND the git awareness layer (`git.rs`) run their
//! subprocesses through ONE reviewed implementation instead of two copies. Every
//! caller supplies its OWN timeout (tmux and git have very different healthy
//! latencies — see each module's `*_cmd_timeout`), but the drain/kill/reap
//! machinery is identical and lives here.
//!
//! Why it matters: the `-L t-hub` tmux server is single-threaded and the git
//! store can sit on a slow (OneDrive-backed) filesystem, so a slow op makes every
//! OTHER command QUEUE behind it. A control handler that ran a bare `.output()`
//! then PARKS for the full stall; because the control server caps live connections
//! ([`crate::control::MAX_CONNS`]), enough parked handlers make `serve` reject every
//! NEW connection — the residual flap (`list_terminals` round-trips time out for
//! minutes while bare TCP connects still succeed via the kernel backlog). Bounding
//! the subprocess turns an indefinite park into a fast, recoverable error that frees
//! the handler thread and its connection slot.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Poll cadence for [`output_with_timeout`]'s wait loop. Small enough that
/// completion latency is invisible next to a process spawn, large enough not to
/// busy-spin.
const OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Run `cmd` to completion, but KILL it (and return an [`std::io::ErrorKind::TimedOut`]
/// error) if it has not finished within `timeout`. This is the single choke point that
/// guarantees NO subprocess can park a control handler thread indefinitely.
///
/// stdout/stderr are drained on dedicated threads so a child that fills a ~64 KB pipe
/// buffer can't deadlock the wait (the classic `.output()`-by-hand trap), and a
/// timed-out child is `kill`ed AND reaped so no zombie leaks. On Windows the child is
/// commonly `wsl.exe`; killing it frees THIS process's handler thread and connection
/// slot even if the process inside WSL lingers (an orphan the WSL server reaps), which
/// is the property that matters — the control channel must not stay wedged.
pub fn output_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Output> {
    use std::io::Read;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    // Drain both pipes concurrently: a blocked reader on one pipe must not stop the
    // child from making progress on the other (avoids a full-buffer deadlock).
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = out_handle.join().unwrap_or_default();
                let stderr = err_handle.join().unwrap_or_default();
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None => {
                if Instant::now() >= deadline {
                    // Stalled past the bound: kill + reap (no zombie), then surface a
                    // TimedOut error. Killing closes the pipes, so the reader threads
                    // hit EOF and join cleanly rather than leaking.
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("command exceeded {}s timeout", timeout.as_secs()),
                    ));
                }
                std::thread::sleep(OUTPUT_POLL_INTERVAL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- output_with_timeout: the residual control-flap fix -----------------
    //
    // These prove the property the whole fix rests on: a subprocess that stalls
    // can NO LONGER park its caller indefinitely (which is what let a transient
    // `-L t-hub` server stall — or a wedged git call on the slow store — pile up
    // parked control handlers until `serve` rejected every new connection). They
    // drive a generic hung/fast child (portable `sleep`/`echo`/`cat`) so they need
    // no live tmux or git and run deterministically. Unix-gated: the Windows CI
    // target has neither on PATH.

    /// A child that outlives the timeout is KILLED and surfaces `TimedOut` FAST —
    /// the caller does not wait for the child's natural (30s) exit. This is the
    /// wedge that used to hang a control handler forever; now it returns bounded.
    #[cfg(unix)]
    #[test]
    fn output_with_timeout_kills_a_hung_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let start = Instant::now();
        let res = output_with_timeout(cmd, Duration::from_millis(200));
        let elapsed = start.elapsed();
        let err = res.expect_err("a 30s sleep must time out under a 200ms bound");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "got: {err}");
        // Must return promptly (well under the child's 30s), proving we didn't wait
        // it out. Generous upper bound to stay non-flaky under a loaded CI box.
        assert!(
            elapsed < Duration::from_secs(5),
            "timed-out call should return promptly, took {elapsed:?}"
        );
    }

    /// A fast child completes normally and its stdout/exit are captured intact —
    /// the bound never trips on a healthy call (no behavior change for the 99%).
    #[cfg(unix)]
    #[test]
    fn output_with_timeout_passes_through_a_fast_child() {
        let mut cmd = Command::new("echo");
        cmd.arg("t-hub-ok");
        let out = output_with_timeout(cmd, Duration::from_secs(5)).expect("echo should run");
        assert!(out.status.success(), "echo should exit 0");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "t-hub-ok");
    }

    /// Two hung children run CONCURRENTLY, each finishing at its own ~timeout — the
    /// bound is per-call, not a shared serialization point, so one stalled call
    /// cannot block a concurrent reader (the "concurrent reader is NOT blocked"
    /// property, mirroring #45's persist_hook wedge test). If the two were
    /// serialized, total wall-clock would be ~2x a single bound.
    #[cfg(unix)]
    #[test]
    fn output_with_timeout_does_not_serialize_concurrent_calls() {
        let start = Instant::now();
        let handles: Vec<_> = (0..2)
            .map(|_| {
                std::thread::spawn(|| {
                    let mut cmd = Command::new("sleep");
                    cmd.arg("30");
                    output_with_timeout(cmd, Duration::from_millis(400))
                })
            })
            .collect();
        for h in handles {
            let r = h.join().expect("thread should not panic");
            assert_eq!(
                r.expect_err("hung child times out").kind(),
                std::io::ErrorKind::TimedOut
            );
        }
        let elapsed = start.elapsed();
        // Both bounds are 400ms; run in parallel they finish in ~400ms, NOT ~800ms.
        // Upper bound stays comfortably under 2x to catch accidental serialization
        // while tolerating scheduler jitter.
        assert!(
            elapsed < Duration::from_millis(700),
            "concurrent timed-out calls should overlap (~400ms), took {elapsed:?}"
        );
    }

    /// F3: a child that writes > 64 KB to BOTH stdout AND stderr completes without
    /// deadlock. This is the exact trap a hand-rolled `.output()` wait falls into:
    /// once a pipe's ~64 KB kernel buffer fills, the child BLOCKS on its next write
    /// and never exits, so a single-threaded waiter that reads stdout only after
    /// the child exits deadlocks forever. The two dedicated drain threads read both
    /// pipes CONCURRENTLY while the child runs, so it can always make progress. We
    /// push well past one buffer on each stream and assert every byte is captured
    /// and the call returns quickly under a generous bound (never trips the timeout).
    #[cfg(unix)]
    #[test]
    fn output_with_timeout_drains_large_dual_pipes_without_deadlock() {
        // 256 KiB on EACH stream — 4x the classic ~64 KB pipe buffer, so a full
        // buffer is guaranteed. Both streams are written CONCURRENTLY (the stderr
        // writer runs in the background while the stdout writer runs in the
        // foreground, then we `wait`), so BOTH kernel pipe buffers fill at the same
        // time: if EITHER drain thread were missing, the child would block on its
        // full pipe and the call would deadlock (and only escape via the timeout).
        // `yes X | head -c N` emits exactly N bytes; kept to `sh -c` with portable
        // tools so no live tmux/git is needed.
        const N: usize = 256 * 1024;
        let script = format!(
            "yes E | head -c {N} 1>&2 & yes T | head -c {N} ; wait",
            N = N
        );
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);

        let start = Instant::now();
        // Generous bound: the work is pure pipe throughput (well under a second on
        // any box). If the drain DID deadlock, this would instead hit the bound and
        // return TimedOut — so a clean Ok here is itself the anti-deadlock proof.
        let out = output_with_timeout(cmd, Duration::from_secs(20))
            .expect("large dual-pipe child must complete without deadlock");
        let elapsed = start.elapsed();

        assert!(out.status.success(), "child should exit 0");
        assert_eq!(out.stdout.len(), N, "all stdout bytes drained");
        assert_eq!(out.stderr.len(), N, "all stderr bytes drained");
        assert!(
            out.stdout.iter().all(|&b| b == b'T' || b == b'\n'),
            "stdout content preserved"
        );
        assert!(
            out.stderr.iter().all(|&b| b == b'E' || b == b'\n'),
            "stderr content preserved"
        );
        // Completed on throughput, nowhere near the 20s bound — proves it drained
        // live rather than crawling toward the deadline.
        assert!(
            elapsed < Duration::from_secs(10),
            "dual-pipe drain should finish on throughput, took {elapsed:?}"
        );
    }
}
