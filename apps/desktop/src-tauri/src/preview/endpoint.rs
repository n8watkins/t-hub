//! Managed Preview endpoint validation and bounded reachability probing.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_PROBE_ATTEMPTS: usize = 4;
const PROBE_TIMEOUT_MS: u64 = 750;
const PROBE_BACKOFF_MS: [u64; MAX_PROBE_ATTEMPTS - 1] = [40, 120, 300];
const WSL_MAPPING_TTL_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedRunIdentity {
    pub run_id: String,
    pub process_group_id: u32,
    pub process_group_started_at: u64,
}

impl ManagedRunIdentity {
    pub fn validate(&self) -> Result<(), String> {
        if self.run_id.is_empty() || self.run_id.len() > 160 {
            return Err("managed Preview run id must contain 1 to 160 bytes".into());
        }
        if self.process_group_id == 0 || self.process_group_started_at == 0 {
            return Err("managed Preview process identity must be nonzero".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerOwnership {
    pub process_group_id: u32,
    pub process_group_started_at: u64,
}

impl ListenerOwnership {
    fn belongs_to(&self, run: &ManagedRunIdentity) -> bool {
        self.process_group_id == run.process_group_id
            && self.process_group_started_at == run.process_group_started_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointHint {
    pub port: u16,
    pub path_and_query: String,
}

impl EndpointHint {
    pub fn loopback_url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.path_and_query)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewEndpoint {
    pub hinted_url: String,
    pub advertised_url: String,
    pub reachable_url: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    NoManagedHint,
    ForeignListener,
    ListenerMissing,
    Unreachable,
    Cancelled,
    Inspection(String),
}

pub trait EndpointInspector: Send + Sync {
    fn listener_ownership(&self, port: u16) -> Result<Option<ListenerOwnership>, String>;
    fn probe(&self, url: &str, timeout: Duration, cancellation: &ProbeCancellation) -> bool;
    fn backoff(&self, duration: Duration, cancellation: &ProbeCancellation);
}

#[derive(Debug, Default)]
pub struct ProbeCancellation(AtomicBool);

impl ProbeCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Default)]
pub struct ProbeCoordinator {
    flights: Mutex<HashMap<String, Arc<ProbeFlight>>>,
}

#[derive(Default)]
struct ProbeFlight {
    result: Mutex<Option<bool>>,
    ready: Condvar,
}

impl ProbeCoordinator {
    fn run<F>(&self, key: String, cancellation: &ProbeCancellation, probe: F) -> Option<bool>
    where
        F: FnOnce() -> bool,
    {
        let (flight, leader) = {
            let mut flights = self
                .flights
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match flights.get(&key) {
                Some(flight) => (Arc::clone(flight), false),
                None => {
                    let flight = Arc::new(ProbeFlight::default());
                    flights.insert(key.clone(), Arc::clone(&flight));
                    (flight, true)
                }
            }
        };
        if leader {
            let result = probe();
            *flight
                .result
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(result);
            flight.ready.notify_all();
            self.flights
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&key);
            return Some(result);
        }

        let mut result = flight
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(result) = *result {
                return Some(result);
            }
            if cancellation.is_cancelled() {
                return None;
            }
            result = flight
                .ready
                .wait_timeout(result, Duration::from_millis(25))
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
    }
}

pub struct EndpointResolver<I> {
    inspector: I,
    probes: ProbeCoordinator,
}

impl<I: EndpointInspector> EndpointResolver<I> {
    pub fn new(inspector: I) -> Self {
        Self {
            inspector,
            probes: ProbeCoordinator::default(),
        }
    }

    pub fn resolve(
        &self,
        run: &ManagedRunIdentity,
        managed_output: &[u8],
        mapped_host: Option<&str>,
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewEndpoint, EndpointError> {
        if cancellation.is_cancelled() {
            return Err(EndpointError::Cancelled);
        }
        let (hinted_url, hint) = parse_managed_output(managed_output)?;
        let owner = self
            .inspector
            .listener_ownership(hint.port)
            .map_err(EndpointError::Inspection)?
            .ok_or(EndpointError::ListenerMissing)?;
        if !owner.belongs_to(run) {
            return Err(EndpointError::ForeignListener);
        }

        let advertised_url = hint.loopback_url();
        let mut candidates = vec![advertised_url.clone()];
        if let Some(host) = mapped_host {
            if valid_derived_host(host) {
                let mapped_host = host
                    .parse::<IpAddr>()
                    .map(|address| match address {
                        IpAddr::V4(_) => address.to_string(),
                        IpAddr::V6(_) => format!("[{address}]"),
                    })
                    .unwrap_or_else(|_| host.to_string());
                let mapped = format!("http://{mapped_host}:{}{}", hint.port, hint.path_and_query);
                if mapped != advertised_url {
                    candidates.push(mapped);
                }
            }
        }
        for candidate in candidates {
            let key = format!("{}:{candidate}", run.run_id);
            let reached = self.probes.run(key, cancellation, || {
                bounded_probe(&self.inspector, &candidate, cancellation)
            });
            match reached {
                Some(true) => {
                    if cancellation.is_cancelled() {
                        return Err(EndpointError::Cancelled);
                    }
                    let final_owner = self
                        .inspector
                        .listener_ownership(hint.port)
                        .map_err(EndpointError::Inspection)?
                        .ok_or(EndpointError::ListenerMissing)?;
                    if !final_owner.belongs_to(run) {
                        return Err(EndpointError::ForeignListener);
                    }
                    return Ok(PreviewEndpoint {
                        hinted_url,
                        advertised_url,
                        reachable_url: candidate,
                        port: hint.port,
                    });
                }
                Some(false) => {}
                None => return Err(EndpointError::Cancelled),
            }
        }
        Err(if cancellation.is_cancelled() {
            EndpointError::Cancelled
        } else {
            EndpointError::Unreachable
        })
    }
}

fn bounded_probe<I: EndpointInspector>(
    inspector: &I,
    url: &str,
    cancellation: &ProbeCancellation,
) -> bool {
    for attempt in 0..MAX_PROBE_ATTEMPTS {
        if cancellation.is_cancelled() {
            return false;
        }
        if inspector.probe(url, Duration::from_millis(PROBE_TIMEOUT_MS), cancellation) {
            return true;
        }
        if let Some(backoff_ms) = PROBE_BACKOFF_MS.get(attempt) {
            inspector.backoff(Duration::from_millis(*backoff_ms), cancellation);
        }
    }
    false
}

pub fn parse_managed_output(output: &[u8]) -> Result<(String, EndpointHint), EndpointError> {
    let tail = if output.len() > MAX_OUTPUT_BYTES {
        &output[output.len() - MAX_OUTPUT_BYTES..]
    } else {
        output
    };
    let text = String::from_utf8_lossy(tail);
    let mut cursor = 0usize;
    while let Some(offset) = text[cursor..].find("http://") {
        let start = cursor + offset;
        let token = text[start..]
            .split(|character: char| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(character, '\'' | '"' | ')' | ']' | '}' | '<' | '>')
            })
            .next()
            .unwrap_or_default()
            .trim_end_matches(['.', ',', ';']);
        if let Some(hint) = parse_safe_hint(token) {
            return Ok((token.to_string(), hint));
        }
        cursor = start.saturating_add("http://".len());
    }
    Err(EndpointError::NoManagedHint)
}

fn parse_safe_hint(value: &str) -> Option<EndpointHint> {
    let rest = value.strip_prefix("http://")?;
    if rest.contains('@') || rest.contains('#') || rest.chars().any(char::is_control) {
        return None;
    }
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let suffix = &rest[authority_end..];
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        let port = rest[close + 1..].strip_prefix(':')?.parse::<u16>().ok()?;
        (host, port)
    } else {
        let (host, port) = authority.rsplit_once(':')?;
        (host, port.parse::<u16>().ok()?)
    };
    if port == 0
        || !matches!(
            host.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "::"
        )
    {
        return None;
    }
    let path_and_query = match suffix {
        "" => "/".to_string(),
        suffix if suffix.starts_with('/') => suffix.to_string(),
        suffix if suffix.starts_with('?') => format!("/{suffix}"),
        _ => return None,
    };
    Some(EndpointHint {
        port,
        path_and_query,
    })
}

fn valid_derived_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        Ok(IpAddr::V6(address)) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
        Err(_) => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslNetworkSnapshot {
    pub distribution: String,
    pub boot_id: String,
    pub interfaces: Vec<String>,
}

impl WslNetworkSnapshot {
    pub fn fingerprint(&self) -> String {
        let mut interfaces = self.interfaces.clone();
        interfaces.sort();
        let mut digest = Sha256::new();
        for value in std::iter::once(&self.distribution)
            .chain(std::iter::once(&self.boot_id))
            .chain(interfaces.iter())
        {
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        format!("sha256:{:x}", digest.finalize())
    }
}

#[derive(Debug, Clone)]
struct WslMappingEntry {
    fingerprint: String,
    host: String,
    expires_at_ms: u64,
}

#[derive(Default)]
pub struct WslHostMappingCache {
    entry: Mutex<Option<WslMappingEntry>>,
}

impl WslHostMappingCache {
    pub fn resolve<F>(
        &self,
        snapshot: &WslNetworkSnapshot,
        now_ms: u64,
        resolve: F,
    ) -> Option<String>
    where
        F: FnOnce(&WslNetworkSnapshot) -> Option<String>,
    {
        let fingerprint = snapshot.fingerprint();
        let mut entry = self.entry.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(cached) = entry.as_ref() {
            if cached.fingerprint == fingerprint && now_ms < cached.expires_at_ms {
                return Some(cached.host.clone());
            }
        }
        let host = resolve(snapshot).filter(|host| valid_derived_host(host))?;
        *entry = Some(WslMappingEntry {
            fingerprint,
            host: host.clone(),
            expires_at_ms: now_ms.saturating_add(WSL_MAPPING_TTL_MS),
        });
        Some(host)
    }

    pub fn invalidate_failed_probe(&self, fingerprint: &str, host: &str) {
        let mut entry = self.entry.lock().unwrap_or_else(|error| error.into_inner());
        if entry
            .as_ref()
            .is_some_and(|entry| entry.fingerprint == fingerprint && entry.host == host)
        {
            *entry = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    struct FakeInspector {
        ownership: Mutex<VecDeque<Option<ListenerOwnership>>>,
        probes: Mutex<VecDeque<bool>>,
        calls: Mutex<Vec<String>>,
    }

    impl EndpointInspector for FakeInspector {
        fn listener_ownership(&self, _port: u16) -> Result<Option<ListenerOwnership>, String> {
            let mut ownership = self.ownership.lock().unwrap();
            if ownership.len() > 1 {
                Ok(ownership.pop_front().flatten())
            } else {
                Ok(ownership.front().cloned().flatten())
            }
        }

        fn probe(&self, url: &str, _timeout: Duration, _cancellation: &ProbeCancellation) -> bool {
            self.calls.lock().unwrap().push(url.to_string());
            self.probes.lock().unwrap().pop_front().unwrap_or(false)
        }

        fn backoff(&self, _duration: Duration, _cancellation: &ProbeCancellation) {}
    }

    fn run() -> ManagedRunIdentity {
        ManagedRunIdentity {
            run_id: "run-1".into(),
            process_group_id: 42,
            process_group_started_at: 100,
        }
    }

    fn inspector(probes: impl Into<VecDeque<bool>>) -> FakeInspector {
        FakeInspector {
            ownership: Mutex::new(VecDeque::from([Some(ListenerOwnership {
                process_group_id: 42,
                process_group_started_at: 100,
            })])),
            probes: Mutex::new(probes.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn accepts_only_safe_loopback_hints() {
        let (_, hint) = parse_managed_output(b"ready at http://localhost:4173/docs?q=1").unwrap();
        assert_eq!(hint.port, 4173);
        assert_eq!(hint.loopback_url(), "http://127.0.0.1:4173/docs?q=1");
        assert_eq!(
            parse_managed_output(b"https://localhost:1 http://example.com:4173"),
            Err(EndpointError::NoManagedHint)
        );
        assert_eq!(
            parse_managed_output(b"http://localhost:0 http://user@localhost:9"),
            Err(EndpointError::NoManagedHint)
        );
    }

    #[test]
    fn listener_must_belong_to_exact_managed_process_group() {
        let fake = inspector(VecDeque::from([true]));
        fake.ownership
            .lock()
            .unwrap()
            .front_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .process_group_started_at = 99;
        let resolver = EndpointResolver::new(fake);
        assert_eq!(
            resolver.resolve(
                &run(),
                b"http://localhost:4173",
                None,
                &ProbeCancellation::default()
            ),
            Err(EndpointError::ForeignListener)
        );
    }

    #[test]
    fn retries_are_bounded_and_mapped_reachability_is_separate_from_advertised_url() {
        let fake = inspector(VecDeque::from([false, false, false, false, true]));
        let resolver = EndpointResolver::new(fake);
        let endpoint = resolver
            .resolve(
                &run(),
                b"Local: http://0.0.0.0:5173/app",
                Some("172.30.1.2"),
                &ProbeCancellation::default(),
            )
            .unwrap();
        assert_eq!(endpoint.advertised_url, "http://127.0.0.1:5173/app");
        assert_eq!(endpoint.reachable_url, "http://172.30.1.2:5173/app");
        assert_eq!(resolver.inspector.calls.lock().unwrap().len(), 5);
    }

    #[test]
    fn listener_rebind_after_successful_probe_is_rejected() {
        let fake = inspector(VecDeque::from([true]));
        *fake.ownership.lock().unwrap() = VecDeque::from([
            Some(ListenerOwnership {
                process_group_id: 42,
                process_group_started_at: 100,
            }),
            Some(ListenerOwnership {
                process_group_id: 42,
                process_group_started_at: 101,
            }),
        ]);
        let resolver = EndpointResolver::new(fake);
        assert_eq!(
            resolver.resolve(
                &run(),
                b"http://localhost:4173",
                None,
                &ProbeCancellation::default(),
            ),
            Err(EndpointError::ForeignListener)
        );
    }

    #[test]
    fn cancellation_stops_before_inspection_or_probing() {
        let cancellation = ProbeCancellation::default();
        cancellation.cancel();
        let resolver = EndpointResolver::new(inspector(VecDeque::from([true])));
        assert_eq!(
            resolver.resolve(&run(), b"http://localhost:4173", None, &cancellation),
            Err(EndpointError::Cancelled)
        );
        assert!(resolver.inspector.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn concurrent_identical_probes_share_one_flight() {
        let coordinator = Arc::new(ProbeCoordinator::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let leader_coordinator = Arc::clone(&coordinator);
        let leader_calls = Arc::clone(&calls);
        let leader = std::thread::spawn(move || {
            leader_coordinator.run("run:url".into(), &ProbeCancellation::default(), || {
                leader_calls.fetch_add(1, Ordering::Relaxed);
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                true
            })
        });
        started_rx.recv().unwrap();
        let follower_coordinator = Arc::clone(&coordinator);
        let follower_calls = Arc::clone(&calls);
        let follower = std::thread::spawn(move || {
            follower_coordinator.run("run:url".into(), &ProbeCancellation::default(), || {
                follower_calls.fetch_add(1, Ordering::Relaxed);
                false
            })
        });
        for _ in 0..10_000 {
            let follower_joined = coordinator
                .flights
                .lock()
                .unwrap()
                .get("run:url")
                .is_some_and(|flight| Arc::strong_count(flight) >= 3);
            if follower_joined {
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            coordinator
                .flights
                .lock()
                .unwrap()
                .get("run:url")
                .is_some_and(|flight| Arc::strong_count(flight) >= 3),
            "follower did not join the in-flight probe"
        );
        release_tx.send(()).unwrap();
        assert_eq!(leader.join().unwrap(), Some(true));
        assert_eq!(follower.join().unwrap(), Some(true));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn wsl_mapping_cache_keys_ttl_to_distro_boot_and_interfaces() {
        let cache = WslHostMappingCache::default();
        assert!(!valid_derived_host("8.8.8.8"));
        assert!(valid_derived_host("172.30.1.2"));
        let snapshot = WslNetworkSnapshot {
            distribution: "Ubuntu".into(),
            boot_id: "boot-1".into(),
            interfaces: vec!["eth0=172.30.1.2".into(), "lo=127.0.0.1".into()],
        };
        let mut resolutions = 0;
        assert_eq!(
            cache.resolve(&snapshot, 10, |_| {
                resolutions += 1;
                Some("172.30.1.2".into())
            }),
            Some("172.30.1.2".into())
        );
        assert_eq!(
            cache.resolve(&snapshot, 20, |_| panic!("cache miss")),
            Some("172.30.1.2".into())
        );
        assert_eq!(resolutions, 1);
        cache.invalidate_failed_probe(&snapshot.fingerprint(), "172.30.1.2");
        assert_eq!(
            cache.resolve(&snapshot, 21, |_| Some("127.0.0.1".into())),
            Some("127.0.0.1".into())
        );
        let mut rebooted = snapshot;
        rebooted.boot_id = "boot-2".into();
        assert_eq!(
            cache.resolve(&rebooted, 22, |_| Some("172.30.1.3".into())),
            Some("172.30.1.3".into())
        );
    }
}
