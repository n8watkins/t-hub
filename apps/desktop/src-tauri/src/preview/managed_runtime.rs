//! Production process owner for the provider-neutral Preview service.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tauri_plugin_shell::ShellExt;

use super::endpoint::{
    derived_wsl_hosts, EndpointError, EndpointInspector, EndpointResolver, ListenerOwnership,
    ManagedRunIdentity, PreviewEndpoint, ProbeCancellation, WslHostMappingCache,
    WslNetworkSnapshot,
};
use super::managed_runner::{self, BoundedOutput};
use super::model::{PreviewScope, PreviewTarget, PreviewTargetKind, PreviewTargetRef};
use super::runtime::{
    ManagedPreviewProcess, PreviewRuntime, RuntimeObservation, RuntimeRediscovery,
};
use super::supervisor::SupervisedPreviewChild;

const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_WSL_HOST_CANDIDATES: usize = 8;
#[cfg(windows)]
const WSL_NETWORK_SNAPSHOT_LIMIT: usize = 4 * 1024;

trait WslNetworkProvider: Send + Sync {
    fn snapshot(&self) -> Option<WslNetworkSnapshot>;
}

struct SystemWslNetworkProvider;

impl WslNetworkProvider for SystemWslNetworkProvider {
    fn snapshot(&self) -> Option<WslNetworkSnapshot> {
        system_wsl_network_snapshot()
    }
}

#[derive(Debug, Clone)]
struct ResolvedWslHost {
    fingerprint: String,
    host: String,
}

struct LiveRun {
    target: PreviewTargetRef,
    child: Mutex<SupervisedPreviewChild>,
    output: Arc<BoundedOutput>,
    stopped_acknowledged: AtomicBool,
    stop_ack_required: AtomicBool,
}

#[derive(Default)]
struct Admission {
    run_ids: HashSet<String>,
    targets: HashSet<PreviewTargetRef>,
}

#[derive(Clone)]
pub struct ManagedPreviewRuntime {
    agent: crate::agent::AgentBridge,
    runs: Arc<Mutex<HashMap<String, Arc<LiveRun>>>>,
    admission: Arc<Mutex<Admission>>,
    open_url: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
    wsl_network: Arc<dyn WslNetworkProvider>,
    wsl_hosts: Arc<WslHostMappingCache>,
}

impl ManagedPreviewRuntime {
    pub fn new(app: tauri::AppHandle, agent: crate::agent::AgentBridge) -> Self {
        let open_url = Arc::new(move |url: &str| {
            #[allow(deprecated)]
            app.shell()
                .open(url, None)
                .map_err(|error| format!("open Preview URL: {error}"))
        });
        Self {
            agent,
            runs: Arc::new(Mutex::new(HashMap::new())),
            admission: Arc::new(Mutex::new(Admission::default())),
            open_url,
            wsl_network: Arc::new(SystemWslNetworkProvider),
            wsl_hosts: Arc::new(WslHostMappingCache::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            agent: crate::agent::AgentBridge::new(),
            runs: Arc::new(Mutex::new(HashMap::new())),
            admission: Arc::new(Mutex::new(Admission::default())),
            open_url: Arc::new(|_| Ok(())),
            wsl_network: Arc::new(SystemWslNetworkProvider),
            wsl_hosts: Arc::new(WslHostMappingCache::default()),
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

    fn remove_live_exact(&self, run_id: &str, expected: &Arc<LiveRun>) {
        let mut runs = self.runs.lock();
        if runs
            .get(run_id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            runs.remove(run_id);
        }
    }

    fn resolved_wsl_host(&self, excluded: &HashSet<String>) -> Option<ResolvedWslHost> {
        let snapshot = self.wsl_network.snapshot()?;
        let fingerprint = snapshot.fingerprint();
        let host = self
            .wsl_hosts
            .resolve(&snapshot, self.now_ms(), |snapshot| {
                derived_wsl_hosts(snapshot)
                    .into_iter()
                    .find(|host| !excluded.contains(host))
            })?;
        Some(ResolvedWslHost { fingerprint, host })
    }

    fn invalidate_wsl_host(&self, mapping: &ResolvedWslHost) {
        self.wsl_hosts
            .invalidate_failed_probe(&mapping.fingerprint, &mapping.host);
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
        {
            let mut admission = self.admission.lock();
            let runs = self.runs.lock();
            if runs.contains_key(run_id) || admission.run_ids.contains(run_id) {
                return Err("managed Preview run id already exists".into());
            }
            if runs.values().any(|live| live.target == *target_ref)
                || admission.targets.contains(target_ref)
            {
                return Err("managed Preview target already has a runtime owner".into());
            }
            admission.run_ids.insert(run_id.to_string());
            admission.targets.insert(target_ref.clone());
        }
        let spawned = (|| {
            let prepared = match target.kind {
                PreviewTargetKind::PackageScript { .. } => {
                    managed_runner::typed_package_command(canonical_root, target, run_id)?
                }
                PreviewTargetKind::StaticSite { .. } => {
                    managed_runner::typed_static_command(canonical_root, target, run_id)?
                }
            };
            prepared.spawn_authenticated(AUTHENTICATION_TIMEOUT)
        })();
        let child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.release_admission(run_id, target_ref);
                return Err(error);
            }
        };
        let identity = child.identity.clone();
        let output = Arc::new(BoundedOutput::default());
        let generation = child.generation.clone();
        let live = Arc::new(LiveRun {
            target: target_ref.clone(),
            child: Mutex::new(child),
            output,
            stopped_acknowledged: AtomicBool::new(false),
            stop_ack_required: AtomicBool::new(true),
        });
        let readers = start_output_readers(&live, &generation);
        if let Err(error) = readers {
            live.stop_ack_required.store(false, Ordering::Release);
            live.child.lock().stdin.take();
            handoff_cleanup(Arc::clone(&live));
            self.release_admission(run_id, target_ref);
            return Err(error);
        }
        let process = ManagedPreviewProcess {
            identity: identity.clone(),
            target: target_ref.clone(),
            output: live.output.snapshot(),
        };
        {
            let mut admission = self.admission.lock();
            let mut runs = self.runs.lock();
            if runs.insert(run_id.to_string(), live).is_some() {
                unreachable!("run id admission prevents replacement");
            }
            admission.run_ids.remove(run_id);
            admission.targets.remove(target_ref);
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
            Some(status) => {
                drop(child);
                self.remove_live_exact(&process.identity.run_id, &live);
                Ok(RuntimeObservation::Exited {
                    code: status.code(),
                    detail: format!("managed Preview process exited with {status}"),
                })
            }
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
            if let Some(expected) = expected {
                return rediscover_durable_supervisor(run_id, expected);
            }
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
        let mut wait_error_reported = false;
        loop {
            match child.child.try_wait() {
                Ok(Some(_)) if live.stopped_acknowledged.load(Ordering::Acquire) => break,
                Ok(Some(_)) if std::time::Instant::now() < deadline => {
                    drop(child);
                    std::thread::sleep(Duration::from_millis(20));
                    child = live.child.lock();
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    drop(child);
                    std::thread::sleep(Duration::from_millis(20));
                    child = live.child.lock();
                }
                Ok(_) => {
                    drop(child);
                    handoff_cleanup(Arc::clone(&live));
                    return Err("managed Preview cleanup is still pending".into());
                }
                Err(error) if std::time::Instant::now() < deadline => {
                    if !wait_error_reported {
                        eprintln!(
                            "t-hub-preview: retained cleanup authority after wait error: {error}"
                        );
                        wait_error_reported = true;
                    }
                    drop(child);
                    std::thread::sleep(Duration::from_millis(20));
                    child = live.child.lock();
                }
                Err(_) => {
                    drop(child);
                    handoff_cleanup(Arc::clone(&live));
                    return Err("managed Preview cleanup observation is still pending".into());
                }
            }
        }
        drop(child);
        self.remove_live_exact(&process.identity.run_id, &live);
        Ok(())
    }

    fn resolve_endpoint(
        &self,
        process: &ManagedPreviewProcess,
        output: &[u8],
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewEndpoint, EndpointError> {
        let live = self
            .live_exact(process)
            .ok_or(EndpointError::ForeignListener)?;
        let generation = live.child.lock().generation.clone();
        let inspector = RuntimeEndpointInspector {
            agent: self.agent.clone(),
            identity: process.identity.clone(),
            generation,
        };
        let resolver = EndpointResolver::new(inspector);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut observed = output.to_vec();
        loop {
            let mut excluded_hosts = HashSet::new();
            match resolve_with_wsl_remap(
                &resolver,
                &process.identity,
                &observed,
                cancellation,
                self.resolved_wsl_host(&excluded_hosts),
                |failed| {
                    excluded_hosts.insert(failed.host.clone());
                    self.invalidate_wsl_host(failed);
                    self.resolved_wsl_host(&excluded_hosts)
                },
            ) {
                Err(EndpointError::NoManagedHint | EndpointError::ListenerMissing)
                    if std::time::Instant::now() < deadline && !cancellation.is_cancelled() =>
                {
                    std::thread::sleep(Duration::from_millis(25));
                    observed = live.output.snapshot();
                }
                result => return result,
            }
        }
    }

    fn open(&self, url: &str) -> Result<(), String> {
        (self.open_url)(url)
    }

    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
}

fn resolve_with_wsl_remap<I, F>(
    resolver: &EndpointResolver<I>,
    identity: &ManagedRunIdentity,
    output: &[u8],
    cancellation: &ProbeCancellation,
    mut mapping: Option<ResolvedWslHost>,
    mut remap: F,
) -> Result<PreviewEndpoint, EndpointError>
where
    I: EndpointInspector,
    F: FnMut(&ResolvedWslHost) -> Option<ResolvedWslHost>,
{
    let mut attempted_hosts = HashSet::new();
    loop {
        if let Some(mapping) = mapping.as_ref() {
            if !attempted_hosts.insert(mapping.host.clone())
                || attempted_hosts.len() > MAX_WSL_HOST_CANDIDATES
            {
                return Err(EndpointError::Unreachable);
            }
        }
        match resolver.resolve(
            identity,
            output,
            mapping.as_ref().map(|mapping| mapping.host.as_str()),
            cancellation,
        ) {
            Err(EndpointError::Unreachable)
                if mapping.is_some() && !cancellation.is_cancelled() =>
            {
                mapping = remap(mapping.as_ref().expect("checked above"));
            }
            result => return result,
        }
    }
}

#[cfg(windows)]
fn system_wsl_network_snapshot() -> Option<WslNetworkSnapshot> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SNAPSHOT_SCRIPT: &str = "cat /proc/sys/kernel/random/boot_id; printf '\\n'; hostname -I";
    let distribution = crate::files::host_distro();
    let mut command = Command::new(super::supervisor::trusted_wsl_path().ok()?);
    command
        .arg("-d")
        .arg(&distribution)
        .arg("-e")
        .arg("/bin/sh")
        .arg("-c")
        .arg(SNAPSHOT_SCRIPT)
        .creation_flags(CREATE_NO_WINDOW);
    let output = crate::bounded_exec::output_with_timeout_and_limit(
        command,
        crate::bounded_exec::WSL_PROBE_TIMEOUT,
        WSL_NETWORK_SNAPSHOT_LIMIT,
    )
    .ok()?;
    if !output.status.success() || !output.stderr.is_empty() {
        return None;
    }
    parse_wsl_network_snapshot(&distribution, &output.stdout)
}

#[cfg(not(windows))]
fn system_wsl_network_snapshot() -> Option<WslNetworkSnapshot> {
    None
}

#[cfg(any(windows, test))]
fn parse_wsl_network_snapshot(distribution: &str, output: &[u8]) -> Option<WslNetworkSnapshot> {
    let text = std::str::from_utf8(output).ok()?;
    let mut lines = text.lines();
    let boot_id = lines.next()?.trim();
    if boot_id.is_empty() || boot_id.len() > 128 {
        return None;
    }
    let interfaces = lines
        .flat_map(str::split_whitespace)
        .filter(|address| address.len() <= 64 && address.parse::<std::net::IpAddr>().is_ok())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if interfaces.is_empty() {
        return None;
    }
    Some(WslNetworkSnapshot {
        distribution: distribution.to_string(),
        boot_id: boot_id.to_string(),
        interfaces,
    })
}

impl ManagedPreviewRuntime {
    fn release_admission(&self, run_id: &str, target: &PreviewTargetRef) {
        let mut admission = self.admission.lock();
        admission.run_ids.remove(run_id);
        admission.targets.remove(target);
    }
}

fn start_output_readers(live: &Arc<LiveRun>, generation: &str) -> Result<(), String> {
    let mut child = live.child.lock();
    let stdout = child
        .stdout
        .take()
        .ok_or("managed Preview stdout reader is unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("managed Preview stderr reader is unavailable")?;
    spawn_output_reader(stdout, Arc::clone(live), None, "stdout")?;
    spawn_output_reader(
        stderr,
        Arc::clone(live),
        Some(format!("T_HUB_PREVIEW_STOPPED {generation}").into_bytes()),
        "stderr",
    )
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    live: Arc<LiveRun>,
    stopped_marker: Option<Vec<u8>>,
    stream: &'static str,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name(format!("t-hub-preview-{stream}"))
        .spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut marker_tail = Vec::new();
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        live.output.append(&buffer[..read]);
                        if let Some(marker) = stopped_marker.as_deref() {
                            marker_tail.extend_from_slice(&buffer[..read]);
                            if marker_tail
                                .windows(marker.len())
                                .any(|candidate| candidate == marker)
                            {
                                live.stopped_acknowledged.store(true, Ordering::Release);
                            }
                            if marker_tail.len() > marker.len() {
                                let keep = marker.len().saturating_sub(1);
                                marker_tail.drain(..marker_tail.len().saturating_sub(keep));
                            }
                        }
                    }
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("start managed Preview {stream} reader: {error}"))
}

fn cleanup_owner(name: &str) -> mpsc::Sender<Arc<LiveRun>> {
    let (sender, receiver) = mpsc::channel::<Arc<LiveRun>>();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || cleanup_owner_loop(receiver))
        .expect("spawn persistent managed Preview cleanup owner");
    sender
}

static CLEANUP_OWNER: LazyLock<mpsc::Sender<Arc<LiveRun>>> =
    LazyLock::new(|| cleanup_owner("t-hub-preview-runtime-cleanup"));
static CLEANUP_OWNER_FALLBACK: LazyLock<mpsc::Sender<Arc<LiveRun>>> =
    LazyLock::new(|| cleanup_owner("t-hub-preview-runtime-cleanup-fallback"));

fn handoff_cleanup(live: Arc<LiveRun>) {
    let live = match CLEANUP_OWNER.send(live) {
        Ok(()) => return,
        Err(error) => error.0,
    };
    if let Err(error) = CLEANUP_OWNER_FALLBACK.send(live) {
        let _ = Arc::into_raw(error.0);
    }
}

fn cleanup_owner_loop(receiver: mpsc::Receiver<Arc<LiveRun>>) {
    let mut active = Vec::new();
    loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(live) => active.push(live),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if active.is_empty() => return,
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        active.extend(receiver.try_iter());
        active.retain(|live| {
            let exited = live.child.lock().child.try_wait().ok().flatten().is_some();
            let acknowledged = !live.stop_ack_required.load(Ordering::Acquire)
                || live.stopped_acknowledged.load(Ordering::Acquire);
            !(exited && acknowledged)
        });
    }
}

fn rediscover_durable_supervisor(
    run_id: &str,
    expected: &ManagedRunIdentity,
) -> Result<RuntimeRediscovery, String> {
    rediscover_durable_supervisor_with_timeout(run_id, expected, STOP_TIMEOUT)
}

fn rediscover_durable_supervisor_with_timeout(
    run_id: &str,
    expected: &ManagedRunIdentity,
    timeout: Duration,
) -> Result<RuntimeRediscovery, String> {
    #[cfg(target_os = "linux")]
    {
        if managed_runner::revalidate_process_identity(expected).is_err() {
            return Ok(RuntimeRediscovery::Absent);
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let command = std::fs::read(format!("/proc/{}/cmdline", expected.process_group_id))
                .map_err(|error| format!("inspect durable managed Preview supervisor: {error}"))?;
            if !command
                .split(|byte| *byte == 0)
                .any(|argument| argument == run_id.as_bytes())
            {
                return Ok(RuntimeRediscovery::Foreign);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(RuntimeRediscovery::Ambiguous);
            }
            std::thread::sleep(Duration::from_millis(20));
            if managed_runner::revalidate_process_identity(expected).is_err() {
                return Ok(RuntimeRediscovery::Absent);
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (run_id, expected, timeout);
        Ok(RuntimeRediscovery::Ambiguous)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::model::{PreviewPackageManager, PreviewTargetId, PreviewTargetSource};
    use std::sync::Mutex as StdMutex;

    struct MappingInspector {
        calls: Arc<StdMutex<Vec<String>>>,
    }

    impl EndpointInspector for MappingInspector {
        fn listener_ownership(&self, _port: u16) -> Result<Option<ListenerOwnership>, String> {
            Ok(Some(ListenerOwnership {
                process_group_id: 42,
                process_group_started_at: 100,
            }))
        }

        fn probe(&self, url: &str, _timeout: Duration, _cancellation: &ProbeCancellation) -> bool {
            self.calls.lock().unwrap().push(url.to_string());
            url.contains("172.30.1.3")
        }

        fn backoff(&self, _duration: Duration, _cancellation: &ProbeCancellation) {}
    }

    #[test]
    fn parses_bounded_wsl_network_identity_for_host_mapping() {
        let snapshot = parse_wsl_network_snapshot(
            "Ubuntu",
            b"7b99545f-67c0-4f0c-b91a-17486fd38233\n172.30.1.2 127.0.0.1\n",
        )
        .unwrap();
        assert_eq!(snapshot.distribution, "Ubuntu");
        assert_eq!(snapshot.boot_id, "7b99545f-67c0-4f0c-b91a-17486fd38233");
        assert_eq!(snapshot.interfaces, ["172.30.1.2", "127.0.0.1"]);
        assert_eq!(derived_wsl_hosts(&snapshot), ["172.30.1.2"]);
        assert!(parse_wsl_network_snapshot("Ubuntu", b"\n172.30.1.2\n").is_none());
    }

    #[test]
    fn stale_wsl_address_is_invalidated_and_retried_with_fresh_mapping() {
        let inspector = MappingInspector {
            calls: Arc::new(StdMutex::new(Vec::new())),
        };
        let calls = Arc::clone(&inspector.calls);
        let resolver = EndpointResolver::new(inspector);
        let identity = ManagedRunIdentity {
            run_id: "run-mapping".into(),
            process_group_id: 42,
            process_group_started_at: 100,
        };
        let endpoint = resolve_with_wsl_remap(
            &resolver,
            &identity,
            b"ready at http://127.0.0.1:5173/app",
            &ProbeCancellation::default(),
            Some(ResolvedWslHost {
                fingerprint: "old-network".into(),
                host: "172.30.1.2".into(),
            }),
            |failed| {
                assert_eq!(failed.fingerprint, "old-network");
                assert_eq!(failed.host, "172.30.1.2");
                Some(ResolvedWslHost {
                    fingerprint: "new-network".into(),
                    host: "172.30.1.3".into(),
                })
            },
        )
        .unwrap();
        assert_eq!(endpoint.reachable_url, "http://172.30.1.3:5173/app");
        let calls = calls.lock().unwrap();
        assert!(calls.iter().any(|url| url.contains("172.30.1.2")));
        assert!(calls.iter().any(|url| url.contains("172.30.1.3")));
    }

    #[test]
    fn unchanged_snapshot_excludes_failed_host_and_uses_next_candidate() {
        let cache = WslHostMappingCache::default();
        let snapshot = WslNetworkSnapshot {
            distribution: "Ubuntu".into(),
            boot_id: "same-boot".into(),
            interfaces: vec!["172.30.1.2 172.30.1.3".into()],
        };
        let first = cache
            .resolve(&snapshot, 1, |snapshot| {
                derived_wsl_hosts(snapshot).into_iter().next()
            })
            .unwrap();
        assert_eq!(first, "172.30.1.2");
        cache.invalidate_failed_probe(&snapshot.fingerprint(), &first);
        let excluded = HashSet::from([first]);
        let second = cache
            .resolve(&snapshot, 2, |snapshot| {
                derived_wsl_hosts(snapshot)
                    .into_iter()
                    .find(|host| !excluded.contains(host))
            })
            .unwrap();
        assert_eq!(second, "172.30.1.3");
    }

    fn fixture() -> (
        tempfile::TempDir,
        PreviewScope,
        PreviewTarget,
        PreviewTargetRef,
    ) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "scripts": {
                    "dev": "node -e \"console.log('http://127.0.0.1:43191/'); setInterval(() => {}, 1000)\""
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let scope = PreviewScope::new("project-runtime", Some("workspace-runtime".into())).unwrap();
        let target = PreviewTarget {
            id: PreviewTargetId::parse("root:dev").unwrap(),
            label: "Dev".into(),
            source: PreviewTargetSource::Root,
            relative_root: String::new(),
            kind: PreviewTargetKind::PackageScript {
                package_manager: PreviewPackageManager::Npm,
                script: "dev".into(),
            },
            recommended: true,
        };
        let target_ref = PreviewTargetRef {
            scope: scope.clone(),
            target_id: target.id.clone(),
            discovery_fingerprint: "sha256:runtime-fixture".into(),
        };
        (root, scope, target, target_ref)
    }

    #[test]
    fn managed_runtime_admits_one_owner_and_stops_through_correlated_cleanup() {
        let runtime = ManagedPreviewRuntime::for_test();
        let (root, scope, target, target_ref) = fixture();
        let process = runtime
            .spawn(&scope, root.path(), &target, &target_ref, "runtime-stop-1")
            .unwrap();
        assert!(matches!(
            runtime.observe(&process).unwrap(),
            RuntimeObservation::Running { .. }
        ));
        assert!(runtime
            .spawn(
                &scope,
                root.path(),
                &target,
                &target_ref,
                "runtime-collision-2",
            )
            .unwrap_err()
            .contains("already has a runtime owner"));
        let mut foreign = process.clone();
        foreign.identity.process_group_started_at =
            foreign.identity.process_group_started_at.saturating_add(1);
        assert!(runtime
            .stop(&foreign)
            .unwrap_err()
            .contains("ownership was lost"));
        assert!(matches!(
            runtime.observe(&process).unwrap(),
            RuntimeObservation::Running { .. }
        ));
        runtime.stop(&process).unwrap();
        assert!(runtime.runs.lock().is_empty());
    }

    #[test]
    fn missing_memory_owner_never_turns_a_live_durable_supervisor_into_absent() {
        let runtime = ManagedPreviewRuntime::for_test();
        let (root, scope, target, target_ref) = fixture();
        let process = runtime
            .spawn(
                &scope,
                root.path(),
                &target,
                &target_ref,
                "runtime-recovery-1",
            )
            .unwrap();
        let retained = runtime
            .runs
            .lock()
            .remove(&process.identity.run_id)
            .unwrap();
        assert_eq!(
            rediscover_durable_supervisor_with_timeout(
                &process.identity.run_id,
                &process.identity,
                Duration::ZERO,
            )
            .unwrap(),
            RuntimeRediscovery::Ambiguous
        );
        retained.child.lock().stdin.take();
        handoff_cleanup(retained);
    }

    #[test]
    fn failed_spawn_releases_admission_without_replacing_an_owner() {
        let runtime = ManagedPreviewRuntime::for_test();
        let (root, scope, target, target_ref) = fixture();
        let missing = root.path().join("missing");
        assert!(runtime
            .spawn(&scope, &missing, &target, &target_ref, "runtime-retry-1",)
            .is_err());
        let process = runtime
            .spawn(&scope, root.path(), &target, &target_ref, "runtime-retry-1")
            .unwrap();
        runtime.stop(&process).unwrap();
    }

    #[test]
    fn reader_setup_failure_hands_the_intact_supervisor_to_cleanup() {
        let (root, _scope, target, target_ref) = fixture();
        let prepared =
            managed_runner::typed_package_command(root.path(), &target, "runtime-readers-1")
                .unwrap();
        let mut child = prepared
            .spawn_authenticated(AUTHENTICATION_TIMEOUT)
            .unwrap();
        child.stderr.take();
        let generation = child.generation.clone();
        let live = Arc::new(LiveRun {
            target: target_ref,
            child: Mutex::new(child),
            output: Arc::new(BoundedOutput::default()),
            stopped_acknowledged: AtomicBool::new(false),
            stop_ack_required: AtomicBool::new(true),
        });
        assert!(start_output_readers(&live, &generation).is_err());
        live.stop_ack_required.store(false, Ordering::Release);
        live.child.lock().stdin.take();
        handoff_cleanup(live);
    }

    #[test]
    fn http_probe_parser_accepts_only_explicit_socket_authorities() {
        assert_eq!(
            parse_http_socket("http://127.0.0.1:43191/path"),
            Some("127.0.0.1:43191".parse().unwrap())
        );
        assert!(parse_http_socket("https://127.0.0.1:43191/").is_none());
        assert!(parse_http_socket("http://localhost:43191/").is_none());
    }

    #[test]
    fn static_target_runs_under_the_same_supervisor_and_resolves_its_owned_endpoint() {
        let runtime = ManagedPreviewRuntime::for_test();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "STATIC RUNTIME SENTINEL").unwrap();
        let scope = PreviewScope::new("project-static", None).unwrap();
        let target = PreviewTarget {
            id: PreviewTargetId::parse("static:root").unwrap(),
            label: "Static".into(),
            source: PreviewTargetSource::Config,
            relative_root: String::new(),
            kind: PreviewTargetKind::StaticSite {
                entrypoint: "index.html".into(),
            },
            recommended: true,
        };
        let target_ref = PreviewTargetRef {
            scope: scope.clone(),
            target_id: target.id.clone(),
            discovery_fingerprint: "sha256:static-runtime".into(),
        };
        let process = runtime
            .spawn(
                &scope,
                root.path(),
                &target,
                &target_ref,
                "runtime-static-1",
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let endpoint = loop {
            let RuntimeObservation::Running { output } = runtime.observe(&process).unwrap() else {
                panic!("static Preview helper exited before endpoint resolution");
            };
            match runtime.resolve_endpoint(&process, &output, &ProbeCancellation::default()) {
                Ok(endpoint) => break endpoint,
                Err(super::super::endpoint::EndpointError::NoManagedHint)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("static Preview endpoint did not resolve: {error:?}"),
            }
        };
        let response = ureq::get(&endpoint.reachable_url).call().unwrap();
        assert!(response
            .into_string()
            .unwrap()
            .contains("STATIC RUNTIME SENTINEL"));
        runtime.stop(&process).unwrap();
    }
}
