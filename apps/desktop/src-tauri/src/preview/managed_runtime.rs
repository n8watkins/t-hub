//! Production process owner for the provider-neutral Preview service.

use std::collections::HashMap;
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tauri_plugin_shell::ShellExt;

use super::endpoint::{
    EndpointInspector, EndpointResolver, ListenerOwnership, ManagedRunIdentity, PreviewEndpoint,
    ProbeCancellation,
};
use super::managed_runner::{self, BoundedOutput};
use super::model::{PreviewScope, PreviewTarget, PreviewTargetKind, PreviewTargetRef};
use super::runtime::{
    ManagedPreviewProcess, PreviewRuntime, RuntimeObservation, RuntimeRediscovery,
};
use super::supervisor::SupervisedPreviewChild;

const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(6);

struct LiveRun {
    target: PreviewTargetRef,
    child: Mutex<SupervisedPreviewChild>,
    output: Arc<BoundedOutput>,
}

#[derive(Clone)]
pub struct ManagedPreviewRuntime {
    app: tauri::AppHandle,
    agent: crate::agent::AgentBridge,
    runs: Arc<Mutex<HashMap<String, Arc<LiveRun>>>>,
}

impl ManagedPreviewRuntime {
    pub fn new(app: tauri::AppHandle, agent: crate::agent::AgentBridge) -> Self {
        Self {
            app,
            agent,
            runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn live_exact(&self, process: &ManagedPreviewProcess) -> Option<Arc<LiveRun>> {
        self.runs
            .lock()
            .get(&process.identity.run_id)
            .filter(|live| {
                let child = live.child.lock();
                child.identity == process.identity && live.target == process.target
            })
            .cloned()
    }
}

impl PreviewRuntime for ManagedPreviewRuntime {
    fn spawn(
        &self,
        _scope: &PreviewScope,
        canonical_root: &Path,
        target: &PreviewTarget,
        target_ref: &PreviewTargetRef,
        run_id: &str,
    ) -> Result<ManagedPreviewProcess, String> {
        if !matches!(target.kind, PreviewTargetKind::PackageScript { .. }) {
            return Err(
                "static Preview targets are not available through the managed runtime".into(),
            );
        }
        let prepared = managed_runner::typed_package_command(canonical_root, target, run_id)?;
        let mut child = prepared.spawn_authenticated(AUTHENTICATION_TIMEOUT)?;
        let identity = child.identity.clone();
        let output = Arc::new(BoundedOutput::default());
        if let Some(stdout) = child.stdout.take() {
            drain_output(stdout, Arc::clone(&output), "stdout")?;
        }
        if let Some(stderr) = child.stderr.take() {
            drain_output(stderr, Arc::clone(&output), "stderr")?;
        }
        let process = ManagedPreviewProcess {
            identity: identity.clone(),
            target: target_ref.clone(),
            output: output.snapshot(),
        };
        let live = Arc::new(LiveRun {
            target: target_ref.clone(),
            child: Mutex::new(child),
            output,
        });
        if self.runs.lock().insert(run_id.to_string(), live).is_some() {
            return Err("managed Preview run id already exists".into());
        }
        Ok(process)
    }

    fn observe(&self, process: &ManagedPreviewProcess) -> Result<RuntimeObservation, String> {
        let Some(live) = self.live_exact(process) else {
            return Ok(RuntimeObservation::OwnershipLost);
        };
        #[cfg(target_os = "linux")]
        if managed_runner::revalidate_process_identity(&process.identity).is_err() {
            return Ok(RuntimeObservation::OwnershipLost);
        }
        let mut child = live.child.lock();
        match child
            .child
            .try_wait()
            .map_err(|error| format!("observe managed Preview process: {error}"))?
        {
            Some(status) => Ok(RuntimeObservation::Exited {
                code: status.code(),
                detail: format!("managed Preview process exited with {status}"),
            }),
            None => Ok(RuntimeObservation::Running {
                output: live.output.snapshot(),
            }),
        }
    }

    fn rediscover(
        &self,
        _scope: &PreviewScope,
        target: &PreviewTargetRef,
        run_id: &str,
        expected: Option<&ManagedRunIdentity>,
    ) -> Result<RuntimeRediscovery, String> {
        let Some(live) = self.runs.lock().get(run_id).cloned() else {
            return Ok(RuntimeRediscovery::Absent);
        };
        let child = live.child.lock();
        if &live.target != target {
            return Ok(RuntimeRediscovery::Foreign);
        }
        if expected.is_some_and(|expected| expected != &child.identity) {
            return Ok(RuntimeRediscovery::Foreign);
        }
        Ok(RuntimeRediscovery::Exact(ManagedPreviewProcess {
            identity: child.identity.clone(),
            target: live.target.clone(),
            output: live.output.snapshot(),
        }))
    }

    fn stop(&self, process: &ManagedPreviewProcess) -> Result<(), String> {
        let Some(live) = self.live_exact(process) else {
            return Err("managed Preview process ownership was lost".into());
        };
        #[cfg(target_os = "linux")]
        managed_runner::revalidate_process_identity(&process.identity)?;
        let mut child = live.child.lock();
        child.stdin.take();
        let deadline = std::time::Instant::now() + STOP_TIMEOUT;
        loop {
            match child.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    child
                        .child
                        .kill()
                        .map_err(|error| format!("abort managed Preview supervisor: {error}"))?;
                    child
                        .child
                        .wait()
                        .map_err(|error| format!("reap managed Preview supervisor: {error}"))?;
                    break;
                }
                Err(error) => return Err(format!("reap managed Preview supervisor: {error}")),
            }
        }
        drop(child);
        self.runs.lock().remove(&process.identity.run_id);
        Ok(())
    }

    fn resolve_endpoint(
        &self,
        process: &ManagedPreviewProcess,
        output: &[u8],
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewEndpoint, super::endpoint::EndpointError> {
        let live = self
            .live_exact(process)
            .ok_or(super::endpoint::EndpointError::ForeignListener)?;
        let generation = live.child.lock().generation.clone();
        let inspector = RuntimeEndpointInspector {
            agent: self.agent.clone(),
            identity: process.identity.clone(),
            generation,
        };
        EndpointResolver::new(inspector).resolve(&process.identity, output, None, cancellation)
    }

    fn open(&self, url: &str) -> Result<(), String> {
        #[allow(deprecated)]
        self.app
            .shell()
            .open(url, None)
            .map_err(|error| format!("open Preview URL: {error}"))
    }

    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
}

fn drain_output(
    mut reader: impl Read + Send + 'static,
    output: Arc<BoundedOutput>,
    stream: &'static str,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name(format!("t-hub-preview-{stream}"))
        .spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => output.append(&buffer[..read]),
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("start managed Preview {stream} reader: {error}"))
}

#[cfg_attr(not(windows), allow(dead_code))]
struct RuntimeEndpointInspector {
    agent: crate::agent::AgentBridge,
    identity: ManagedRunIdentity,
    generation: String,
}

impl EndpointInspector for RuntimeEndpointInspector {
    fn listener_ownership(&self, port: u16) -> Result<Option<ListenerOwnership>, String> {
        #[cfg(target_os = "linux")]
        {
            return managed_runner::listener_ownership(port);
        }
        #[cfg(windows)]
        {
            let ownership = self.agent.inspect_preview_listener(
                &self.identity.run_id,
                &self.generation,
                port,
                self.identity.process_group_id,
                self.identity.process_group_started_at,
            )?;
            return Ok(ownership.map(|ownership| ListenerOwnership {
                process_group_id: ownership.process_group_id,
                process_group_started_at: ownership.process_group_started_at,
            }));
        }
        #[allow(unreachable_code)]
        Err("managed Preview listener inspection is unsupported on this platform".into())
    }

    fn probe(&self, url: &str, timeout: Duration, cancellation: &ProbeCancellation) -> bool {
        if cancellation.is_cancelled() {
            return false;
        }
        parse_http_socket(url)
            .and_then(|address| TcpStream::connect_timeout(&address, timeout).ok())
            .is_some()
    }

    fn backoff(&self, duration: Duration, cancellation: &ProbeCancellation) {
        let deadline = std::time::Instant::now() + duration;
        while !cancellation.is_cancelled() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn parse_http_socket(url: &str) -> Option<SocketAddr> {
    let authority = url.strip_prefix("http://")?.split('/').next()?;
    authority.parse().ok()
}
