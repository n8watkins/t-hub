//! Preview process ownership abstraction.

use std::path::Path;

use super::endpoint::{EndpointError, ManagedRunIdentity, PreviewEndpoint, ProbeCancellation};
use super::model::{PreviewScope, PreviewTarget, PreviewTargetRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPreviewProcess {
    pub identity: ManagedRunIdentity,
    pub target: PreviewTargetRef,
    pub output: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeObservation {
    Running { output: Vec<u8> },
    Exited { code: Option<i32>, detail: String },
    OwnershipLost,
}

pub trait PreviewRuntime: Send + Sync {
    /// Spawn one backend-selected typed target. Implementations derive the
    /// executable, arguments, environment, and working directory themselves.
    fn spawn(
        &self,
        scope: &PreviewScope,
        canonical_root: &Path,
        target: &PreviewTarget,
        target_ref: &PreviewTargetRef,
        run_id: &str,
    ) -> Result<ManagedPreviewProcess, String>;

    fn observe(&self, process: &ManagedPreviewProcess) -> Result<RuntimeObservation, String>;

    /// Stop only when the exact process-group identity still belongs to this
    /// managed run. Implementations must reject PID/group reuse.
    fn stop(&self, process: &ManagedPreviewProcess) -> Result<(), String>;

    fn resolve_endpoint(
        &self,
        process: &ManagedPreviewProcess,
        output: &[u8],
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewEndpoint, EndpointError>;

    fn open(&self, url: &str) -> Result<(), String>;

    fn now_ms(&self) -> u64;
}
