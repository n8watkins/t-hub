//! Per-scope serialized Preview lifecycle service.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use super::discovery::{PreviewDiscovery, PreviewDiscoveryCache};
use super::endpoint::{EndpointError, ProbeCancellation};
use super::model::{
    PreviewOperation, PreviewOperationOutcome, PreviewOperationResult, PreviewScope, PreviewState,
    PreviewStatus, PreviewTarget, PreviewTargetId, PreviewTargetRef,
};
use super::profile::{
    PreviewIntent, PreviewIntentPhase, PreviewProfileStore, ProjectPreviewProfile,
    SelectedPreviewTarget,
};
use super::runtime::{ManagedPreviewProcess, PreviewRuntime, RuntimeObservation};

#[derive(Clone)]
struct ActiveRun {
    process: ManagedPreviewProcess,
    state: PreviewState,
    preview_url: Option<String>,
    reason: Option<String>,
}

pub struct PreviewService<R> {
    runtime: R,
    profiles: Arc<PreviewProfileStore>,
    discovery: PreviewDiscoveryCache,
    scope_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    active: Mutex<HashMap<String, ActiveRun>>,
}

impl<R: PreviewRuntime> PreviewService<R> {
    pub fn new(runtime: R, profiles: Arc<PreviewProfileStore>) -> Self {
        Self {
            runtime,
            profiles,
            discovery: PreviewDiscoveryCache::default(),
            scope_locks: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
        }
    }

    pub fn discover(&self, root: &Path) -> Result<PreviewDiscovery, String> {
        self.discovery.discover(root)
    }

    pub fn status(&self, scope: &PreviewScope) -> Result<PreviewStatus, String> {
        let scope_lock = self.scope_lock(scope);
        let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
        self.status_locked(scope, &ProbeCancellation::default())
    }

    pub fn select(
        &self,
        root: &Path,
        target_ref: &PreviewTargetRef,
        request_id: &str,
    ) -> Result<PreviewOperationResult, String> {
        validate_request_id(request_id)?;
        let scope_lock = self.scope_lock(&target_ref.scope);
        let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(replayed) = self.replay(request_id, PreviewOperation::Select, target_ref)? {
            return Ok(replayed);
        }
        let discovery = self.discovery.discover(root)?;
        let _ = resolve_target(&discovery, target_ref)?;
        self.prepare_intent(request_id, PreviewOperation::Select, target_ref, None)?;

        let selected = SelectedPreviewTarget {
            target_id: target_ref.target_id.clone(),
            discovery_fingerprint: target_ref.discovery_fingerprint.clone(),
        };
        let accepted_config_targets = discovery
            .targets
            .iter()
            .filter(|target| matches!(target.source, super::model::PreviewTargetSource::Config))
            .cloned()
            .collect();
        let profile_update =
            self.profiles
                .update_project(&target_ref.scope.project_id, |existing| {
                    let mut profile = existing.unwrap_or_else(|| ProjectPreviewProfile {
                        canonical_root_fingerprint: discovery.canonical_root_fingerprint.clone(),
                        selected_target: None,
                        workspace_overrides: Default::default(),
                        accepted_config_targets: Vec::new(),
                    });
                    if profile.canonical_root_fingerprint != discovery.canonical_root_fingerprint {
                        return Err("registered project root fingerprint changed".into());
                    }
                    if let Some(workspace_id) = target_ref.scope.workspace_id.as_ref() {
                        profile
                            .workspace_overrides
                            .insert(workspace_id.clone(), selected);
                    } else {
                        profile.selected_target = Some(selected);
                    }
                    profile.accepted_config_targets = accepted_config_targets;
                    Ok(profile)
                });
        if let Err(error) = profile_update {
            self.block_intent(request_id, &error)?;
            return Err(error);
        }
        let status = self.status_locked(&target_ref.scope, &ProbeCancellation::default())?;
        self.commit_intent(request_id, status.clone())?;
        Ok(result(
            PreviewOperation::Select,
            PreviewOperationOutcome::Applied,
            request_id,
            status,
        ))
    }

    pub fn start(
        &self,
        root: &Path,
        scope: &PreviewScope,
        requested: Option<&PreviewTargetRef>,
        request_id: &str,
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewOperationResult, String> {
        validate_request_id(request_id)?;
        let scope_lock = self.scope_lock(scope);
        let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
        if requested.is_some_and(|target_ref| target_ref.scope != *scope) {
            return Err("Preview target reference belongs to another scope".into());
        }
        if let Some(replayed) = self.replay_optional(
            request_id,
            PreviewOperation::Start,
            scope,
            requested.map(|target_ref| &target_ref.target_id),
        )? {
            return Ok(replayed);
        }
        if cancellation.is_cancelled() {
            return Err("Preview start was cancelled before spawning".into());
        }
        let discovery = self.discovery.discover(root)?;
        let target_ref = match requested {
            Some(target_ref) => target_ref.clone(),
            None => self.selected_ref(scope, &discovery)?,
        };
        let target = resolve_target(&discovery, &target_ref)?.clone();
        let run_id = Uuid::new_v4().to_string();
        self.prepare_intent(
            request_id,
            PreviewOperation::Start,
            &target_ref,
            Some(&run_id),
        )?;
        self.start_locked(
            &discovery,
            scope,
            &target_ref,
            &target,
            request_id,
            &run_id,
            PreviewOperation::Start,
            cancellation,
        )
    }

    pub fn stop(
        &self,
        scope: &PreviewScope,
        expected_run_id: Option<&str>,
        request_id: &str,
    ) -> Result<PreviewOperationResult, String> {
        validate_request_id(request_id)?;
        let scope_lock = self.scope_lock(scope);
        let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope.key())
            .cloned();
        let target_ref = active.as_ref().map(|run| run.process.target.clone());
        if let Some(replayed) =
            self.replay_optional(request_id, PreviewOperation::Stop, scope, None)?
        {
            return Ok(replayed);
        }
        self.prepare_optional_intent(
            request_id,
            PreviewOperation::Stop,
            scope,
            target_ref.as_ref().map(|target| &target.target_id),
            active
                .as_ref()
                .map(|run| run.process.identity.run_id.as_str()),
        )?;
        let Some(active) = active else {
            let status = PreviewStatus::stopped(scope.clone(), self.runtime.now_ms());
            self.commit_intent(request_id, status.clone())?;
            return Ok(result(
                PreviewOperation::Stop,
                PreviewOperationOutcome::Unchanged,
                request_id,
                status,
            ));
        };
        if expected_run_id.is_some_and(|expected| expected != active.process.identity.run_id) {
            let status = status_from_active(
                scope,
                &active,
                PreviewState::Stale,
                Some("requested run was replaced".into()),
                self.runtime.now_ms(),
            );
            self.commit_intent(request_id, status.clone())?;
            return Ok(result(
                PreviewOperation::Stop,
                PreviewOperationOutcome::Unchanged,
                request_id,
                status,
            ));
        }
        match self.runtime.observe(&active.process)? {
            RuntimeObservation::OwnershipLost => {
                let status = status_from_active(
                    scope,
                    &active,
                    PreviewState::Stale,
                    Some("managed process identity no longer owns its process group".into()),
                    self.runtime.now_ms(),
                );
                self.block_intent(request_id, "refused to stop a reused or foreign process")?;
                Ok(result(
                    PreviewOperation::Stop,
                    PreviewOperationOutcome::Unchanged,
                    request_id,
                    status,
                ))
            }
            RuntimeObservation::Exited { .. } => {
                self.active
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&scope.key());
                let status = PreviewStatus::stopped(scope.clone(), self.runtime.now_ms());
                self.commit_intent(request_id, status.clone())?;
                Ok(result(
                    PreviewOperation::Stop,
                    PreviewOperationOutcome::Recovered,
                    request_id,
                    status,
                ))
            }
            RuntimeObservation::Running { .. } => {
                if let Err(error) = self.runtime.stop(&active.process) {
                    self.block_intent(request_id, &error)?;
                    return Err(error);
                }
                self.active
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&scope.key());
                let status = PreviewStatus::stopped(scope.clone(), self.runtime.now_ms());
                self.commit_intent(request_id, status.clone())?;
                Ok(result(
                    PreviewOperation::Stop,
                    PreviewOperationOutcome::Applied,
                    request_id,
                    status,
                ))
            }
        }
    }

    pub fn restart(
        &self,
        root: &Path,
        scope: &PreviewScope,
        request_id: &str,
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewOperationResult, String> {
        validate_request_id(request_id)?;
        let scope_lock = self.scope_lock(scope);
        let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(replayed) =
            self.replay_optional(request_id, PreviewOperation::Restart, scope, None)?
        {
            return Ok(replayed);
        }
        if cancellation.is_cancelled() {
            return Err("Preview restart was cancelled before spawning".into());
        }
        let discovery = self.discovery.discover(root)?;
        let target_ref = match self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope.key())
            .map(|run| run.process.target.clone())
        {
            Some(target_ref) => target_ref,
            None => self.selected_ref(scope, &discovery)?,
        };
        let target = resolve_target(&discovery, &target_ref)?.clone();
        let run_id = Uuid::new_v4().to_string();
        self.prepare_intent(
            request_id,
            PreviewOperation::Restart,
            &target_ref,
            Some(&run_id),
        )?;
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope.key())
            .cloned();
        if let Some(active) = active {
            match self.runtime.observe(&active.process)? {
                RuntimeObservation::Running { .. } => {
                    if let Err(error) = self.runtime.stop(&active.process) {
                        self.block_intent(request_id, &error)?;
                        return Err(error);
                    }
                }
                RuntimeObservation::Exited { .. } | RuntimeObservation::OwnershipLost => {}
            }
            self.active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&scope.key());
        }
        self.start_locked(
            &discovery,
            scope,
            &target_ref,
            &target,
            request_id,
            &run_id,
            PreviewOperation::Restart,
            cancellation,
        )
    }

    pub fn refresh(
        &self,
        scope: &PreviewScope,
        request_id: &str,
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewOperationResult, String> {
        validate_request_id(request_id)?;
        let scope_lock = self.scope_lock(scope);
        let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(replayed) =
            self.replay_optional(request_id, PreviewOperation::Refresh, scope, None)?
        {
            return Ok(replayed);
        }
        self.prepare_optional_intent(request_id, PreviewOperation::Refresh, scope, None, None)?;
        let status = self.status_locked(scope, cancellation)?;
        self.commit_intent(request_id, status.clone())?;
        Ok(result(
            PreviewOperation::Refresh,
            PreviewOperationOutcome::Applied,
            request_id,
            status,
        ))
    }

    pub fn open(
        &self,
        scope: &PreviewScope,
        request_id: &str,
    ) -> Result<PreviewOperationResult, String> {
        validate_request_id(request_id)?;
        let scope_lock = self.scope_lock(scope);
        let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope.key())
            .cloned();
        let target_id = active.as_ref().map(|run| &run.process.target.target_id);
        if let Some(replayed) =
            self.replay_optional(request_id, PreviewOperation::Open, scope, None)?
        {
            return Ok(replayed);
        }
        self.prepare_optional_intent(request_id, PreviewOperation::Open, scope, target_id, None)?;
        let status = self.status_locked(scope, &ProbeCancellation::default())?;
        let Some(url) = status.preview_url.as_deref() else {
            self.commit_intent(request_id, status.clone())?;
            return Ok(result(
                PreviewOperation::Open,
                PreviewOperationOutcome::Unchanged,
                request_id,
                status,
            ));
        };
        if let Err(error) = self.runtime.open(url) {
            self.block_intent(request_id, &error)?;
            return Err(error);
        }
        self.commit_intent(request_id, status.clone())?;
        Ok(result(
            PreviewOperation::Open,
            PreviewOperationOutcome::Applied,
            request_id,
            status,
        ))
    }

    pub fn recover_incomplete(&self) -> Result<Vec<PreviewOperationResult>, String> {
        let mut recovered = Vec::new();
        for intent in self.profiles.recoverable_intents() {
            let scope_lock = self.scope_lock(&intent.scope);
            let _guard = scope_lock.lock().unwrap_or_else(|error| error.into_inner());
            if intent.phase == PreviewIntentPhase::EffectObserved {
                let Some(status) = intent.observed_status.clone() else {
                    self.block_intent(&intent.request_id, "observed intent has no status")?;
                    continue;
                };
                self.profiles.advance_intent(
                    &intent.request_id,
                    PreviewIntentPhase::Committed,
                    Some(status.clone()),
                    Some("recovered observed effect without replay".into()),
                    self.runtime.now_ms(),
                )?;
                recovered.push(result(
                    intent.operation,
                    PreviewOperationOutcome::Recovered,
                    &intent.request_id,
                    status,
                ));
                continue;
            }
            let active = self
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&intent.scope.key())
                .cloned();
            if intent.operation == PreviewOperation::Select {
                let selected_matches = self
                    .profiles
                    .project(&intent.scope.project_id)
                    .and_then(|profile| {
                        intent
                            .scope
                            .workspace_id
                            .as_ref()
                            .and_then(|workspace| profile.workspace_overrides.get(workspace))
                            .or(profile.selected_target.as_ref())
                            .cloned()
                    })
                    .is_some_and(|selected| Some(selected.target_id) == intent.target_id);
                if selected_matches {
                    let status =
                        self.status_locked(&intent.scope, &ProbeCancellation::default())?;
                    self.commit_intent(&intent.request_id, status.clone())?;
                    recovered.push(result(
                        intent.operation,
                        PreviewOperationOutcome::Recovered,
                        &intent.request_id,
                        status,
                    ));
                } else {
                    self.block_intent(
                        &intent.request_id,
                        "prepared selection does not match the durable profile",
                    )?;
                }
                continue;
            }
            if intent.operation == PreviewOperation::Refresh {
                let status = self.status_locked(&intent.scope, &ProbeCancellation::default())?;
                self.commit_intent(&intent.request_id, status.clone())?;
                recovered.push(result(
                    intent.operation,
                    PreviewOperationOutcome::Recovered,
                    &intent.request_id,
                    status,
                ));
                continue;
            }
            let Some(active) = active else {
                if intent.operation == PreviewOperation::Stop {
                    let status =
                        PreviewStatus::stopped(intent.scope.clone(), self.runtime.now_ms());
                    self.commit_intent(&intent.request_id, status.clone())?;
                    recovered.push(result(
                        intent.operation,
                        PreviewOperationOutcome::Recovered,
                        &intent.request_id,
                        status,
                    ));
                    continue;
                }
                self.block_intent(
                    &intent.request_id,
                    "prepared effect cannot be proven after restart",
                )?;
                continue;
            };
            if intent.run_id.as_deref() != Some(active.process.identity.run_id.as_str()) {
                self.block_intent(
                    &intent.request_id,
                    "prepared intent no longer identifies the active run",
                )?;
                continue;
            }
            match self.runtime.observe(&active.process)? {
                RuntimeObservation::Running { .. } => {
                    if let Err(error) = self.runtime.stop(&active.process) {
                        self.block_intent(&intent.request_id, &error)?;
                        continue;
                    }
                }
                RuntimeObservation::Exited { .. } => {}
                RuntimeObservation::OwnershipLost => {
                    self.block_intent(
                        &intent.request_id,
                        "recovery refused a reused or foreign process",
                    )?;
                    continue;
                }
            }
            self.active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&intent.scope.key());
            let status = PreviewStatus::stopped(intent.scope.clone(), self.runtime.now_ms());
            self.commit_intent(&intent.request_id, status.clone())?;
            recovered.push(result(
                intent.operation,
                PreviewOperationOutcome::Recovered,
                &intent.request_id,
                status,
            ));
        }
        Ok(recovered)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_locked(
        &self,
        discovery: &PreviewDiscovery,
        scope: &PreviewScope,
        target_ref: &PreviewTargetRef,
        target: &PreviewTarget,
        request_id: &str,
        run_id: &str,
        operation: PreviewOperation,
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewOperationResult, String> {
        let current = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope.key())
            .cloned();
        if let Some(current) = current {
            if current.process.target == *target_ref {
                if matches!(
                    self.runtime.observe(&current.process)?,
                    RuntimeObservation::Running { .. }
                ) {
                    let status = self.status_locked(scope, cancellation)?;
                    self.commit_intent(request_id, status.clone())?;
                    return Ok(result(
                        operation,
                        PreviewOperationOutcome::Unchanged,
                        request_id,
                        status,
                    ));
                }
            } else if matches!(
                self.runtime.observe(&current.process)?,
                RuntimeObservation::Running { .. }
            ) {
                if let Err(error) = self.runtime.stop(&current.process) {
                    self.block_intent(request_id, &error)?;
                    return Err(error);
                }
            }
            self.active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&scope.key());
        }

        let process =
            match self
                .runtime
                .spawn(scope, &discovery.canonical_root, target, target_ref, run_id)
            {
                Ok(process) => process,
                Err(error) => {
                    let status = PreviewStatus {
                        scope: scope.clone(),
                        state: PreviewState::Failed,
                        target_id: Some(target_ref.target_id.clone()),
                        run_id: Some(run_id.into()),
                        preview_url: None,
                        reason: Some(error),
                        observed_at_ms: self.runtime.now_ms(),
                    };
                    self.commit_intent(request_id, status.clone())?;
                    return Ok(result(
                        operation,
                        PreviewOperationOutcome::Applied,
                        request_id,
                        status,
                    ));
                }
            };
        if process.identity.run_id != run_id
            || process.identity.process_group_id == 0
            || process.identity.process_group_started_at == 0
            || process.target != *target_ref
        {
            let detail = "Preview runtime returned an invalid managed process identity";
            self.block_intent(request_id, detail)?;
            return Err(detail.into());
        }
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                scope.key(),
                ActiveRun {
                    process,
                    state: PreviewState::Starting,
                    preview_url: None,
                    reason: None,
                },
            );
        let status = self.status_locked(scope, cancellation)?;
        self.commit_intent(request_id, status.clone())?;
        Ok(result(
            operation,
            PreviewOperationOutcome::Applied,
            request_id,
            status,
        ))
    }

    fn status_locked(
        &self,
        scope: &PreviewScope,
        cancellation: &ProbeCancellation,
    ) -> Result<PreviewStatus, String> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&scope.key())
            .cloned();
        let Some(mut active) = active else {
            return Ok(PreviewStatus::stopped(scope.clone(), self.runtime.now_ms()));
        };
        match self.runtime.observe(&active.process)? {
            RuntimeObservation::Exited { code, detail } => {
                active.state = PreviewState::Failed;
                active.preview_url = None;
                active.reason = Some(format!("process exited {code:?}: {detail}"));
            }
            RuntimeObservation::OwnershipLost => {
                active.state = PreviewState::Stale;
                active.preview_url = None;
                active.reason = Some("managed process identity was lost".into());
            }
            RuntimeObservation::Running { output } => {
                match self
                    .runtime
                    .resolve_endpoint(&active.process, &output, cancellation)
                {
                    Ok(endpoint) => {
                        active.state = PreviewState::Running;
                        active.preview_url = Some(endpoint.reachable_url);
                        active.reason = None;
                    }
                    Err(EndpointError::Cancelled) => {
                        return Err("Preview endpoint probe was cancelled".into())
                    }
                    Err(EndpointError::ForeignListener) => {
                        active.state = PreviewState::Stale;
                        active.preview_url = None;
                        active.reason = Some("Preview port belongs to a foreign process".into());
                    }
                    Err(error) => {
                        active.state = PreviewState::Unreachable;
                        active.preview_url = None;
                        active.reason = Some(format!("Preview endpoint is unreachable: {error:?}"));
                    }
                }
            }
        }
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(scope.key(), active.clone());
        Ok(status_from_active(
            scope,
            &active,
            active.state,
            active.reason.clone(),
            self.runtime.now_ms(),
        ))
    }

    fn selected_ref(
        &self,
        scope: &PreviewScope,
        discovery: &PreviewDiscovery,
    ) -> Result<PreviewTargetRef, String> {
        let profile = self
            .profiles
            .project(&scope.project_id)
            .ok_or_else(|| "no Preview target is selected for this project".to_string())?;
        if profile.canonical_root_fingerprint != discovery.canonical_root_fingerprint {
            return Err("registered project root fingerprint changed".into());
        }
        let selected = scope
            .workspace_id
            .as_ref()
            .and_then(|workspace| profile.workspace_overrides.get(workspace))
            .or(profile.selected_target.as_ref())
            .ok_or_else(|| "no Preview target is selected for this scope".to_string())?;
        Ok(PreviewTargetRef {
            scope: scope.clone(),
            target_id: selected.target_id.clone(),
            discovery_fingerprint: selected.discovery_fingerprint.clone(),
        })
    }

    fn scope_lock(&self, scope: &PreviewScope) -> Arc<Mutex<()>> {
        self.scope_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(scope.key())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn replay(
        &self,
        request_id: &str,
        operation: PreviewOperation,
        target_ref: &PreviewTargetRef,
    ) -> Result<Option<PreviewOperationResult>, String> {
        self.replay_optional(
            request_id,
            operation,
            &target_ref.scope,
            Some(&target_ref.target_id),
        )
    }

    fn replay_optional(
        &self,
        request_id: &str,
        operation: PreviewOperation,
        scope: &PreviewScope,
        target_id: Option<&PreviewTargetId>,
    ) -> Result<Option<PreviewOperationResult>, String> {
        let Some(intent) = self.profiles.intent(request_id) else {
            return Ok(None);
        };
        if intent.operation != operation
            || intent.scope != *scope
            || target_id.is_some() && intent.target_id.as_ref() != target_id
        {
            return Err("request id is already bound to a different Preview operation".into());
        }
        let status = intent
            .observed_status
            .ok_or_else(|| "Preview request is incomplete and requires recovery".to_string())?;
        Ok(Some(result(
            operation,
            PreviewOperationOutcome::Unchanged,
            request_id,
            status,
        )))
    }

    fn prepare_intent(
        &self,
        request_id: &str,
        operation: PreviewOperation,
        target_ref: &PreviewTargetRef,
        run_id: Option<&str>,
    ) -> Result<(), String> {
        self.prepare_optional_intent(
            request_id,
            operation,
            &target_ref.scope,
            Some(&target_ref.target_id),
            run_id,
        )
    }

    fn prepare_optional_intent(
        &self,
        request_id: &str,
        operation: PreviewOperation,
        scope: &PreviewScope,
        target_id: Option<&PreviewTargetId>,
        run_id: Option<&str>,
    ) -> Result<(), String> {
        self.profiles.record_intent(PreviewIntent {
            request_id: request_id.into(),
            scope: scope.clone(),
            operation,
            target_id: target_id.cloned(),
            run_id: run_id.map(str::to_string),
            phase: PreviewIntentPhase::Prepared,
            observed_status: None,
            detail: None,
            updated_at_ms: self.runtime.now_ms(),
        })?;
        Ok(())
    }

    fn commit_intent(&self, request_id: &str, status: PreviewStatus) -> Result<(), String> {
        self.profiles.advance_intent(
            request_id,
            PreviewIntentPhase::EffectObserved,
            Some(status.clone()),
            None,
            self.runtime.now_ms(),
        )?;
        self.profiles.advance_intent(
            request_id,
            PreviewIntentPhase::Committed,
            Some(status),
            None,
            self.runtime.now_ms(),
        )?;
        Ok(())
    }

    fn block_intent(&self, request_id: &str, detail: &str) -> Result<(), String> {
        self.profiles.advance_intent(
            request_id,
            PreviewIntentPhase::RecoveryBlocked,
            None,
            Some(detail.into()),
            self.runtime.now_ms(),
        )?;
        Ok(())
    }
}

fn resolve_target<'a>(
    discovery: &'a PreviewDiscovery,
    target_ref: &PreviewTargetRef,
) -> Result<&'a PreviewTarget, String> {
    if target_ref.discovery_fingerprint != discovery.discovery_fingerprint {
        return Err("Preview target reference is stale; refresh discovery".into());
    }
    discovery
        .targets
        .iter()
        .find(|target| target.id == target_ref.target_id)
        .ok_or_else(|| "Preview target does not exist in this discovery result".to_string())
}

fn validate_request_id(request_id: &str) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 160 {
        return Err("Preview request id must contain 1 to 160 bytes".into());
    }
    Ok(())
}

fn status_from_active(
    scope: &PreviewScope,
    active: &ActiveRun,
    state: PreviewState,
    reason: Option<String>,
    observed_at_ms: u64,
) -> PreviewStatus {
    PreviewStatus {
        scope: scope.clone(),
        state,
        target_id: Some(active.process.target.target_id.clone()),
        run_id: Some(active.process.identity.run_id.clone()),
        preview_url: active.preview_url.clone(),
        reason,
        observed_at_ms,
    }
}

fn result(
    operation: PreviewOperation,
    outcome: PreviewOperationOutcome,
    request_id: &str,
    status: PreviewStatus,
) -> PreviewOperationResult {
    PreviewOperationResult {
        operation,
        outcome,
        request_id: request_id.into(),
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::endpoint::{ManagedRunIdentity, PreviewEndpoint};
    use crate::preview::runtime::RuntimeObservation;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Default)]
    struct FakeRuntime {
        state: Arc<FakeRuntimeState>,
    }

    #[derive(Default)]
    struct FakeRuntimeState {
        next_group: AtomicU32,
        now: AtomicU64,
        spawn_count: AtomicUsize,
        stop_count: AtomicUsize,
        observations: Mutex<HashMap<String, RuntimeObservation>>,
        opened: Mutex<Vec<String>>,
        fail_spawn: AtomicBool,
        endpoint_error: Mutex<Option<EndpointError>>,
    }

    impl FakeRuntime {
        fn spawn_count(&self) -> usize {
            self.state.spawn_count.load(Ordering::Relaxed)
        }

        fn stop_count(&self) -> usize {
            self.state.stop_count.load(Ordering::Relaxed)
        }

        fn set_observation(&self, run_id: &str, observation: RuntimeObservation) {
            self.state
                .observations
                .lock()
                .unwrap()
                .insert(run_id.into(), observation);
        }
    }

    impl PreviewRuntime for FakeRuntime {
        fn spawn(
            &self,
            _scope: &PreviewScope,
            _canonical_root: &Path,
            _target: &PreviewTarget,
            target_ref: &PreviewTargetRef,
            run_id: &str,
        ) -> Result<ManagedPreviewProcess, String> {
            if self.state.fail_spawn.load(Ordering::Relaxed) {
                return Err("spawn failed".into());
            }
            self.state.spawn_count.fetch_add(1, Ordering::Relaxed);
            let process_group_id = self.state.next_group.fetch_add(1, Ordering::Relaxed) + 10;
            let process = ManagedPreviewProcess {
                identity: ManagedRunIdentity {
                    run_id: run_id.into(),
                    process_group_id,
                    process_group_started_at: u64::from(process_group_id) + 100,
                },
                target: target_ref.clone(),
                output: b"http://localhost:4173".to_vec(),
            };
            self.set_observation(
                run_id,
                RuntimeObservation::Running {
                    output: process.output.clone(),
                },
            );
            Ok(process)
        }

        fn observe(&self, process: &ManagedPreviewProcess) -> Result<RuntimeObservation, String> {
            Ok(self
                .state
                .observations
                .lock()
                .unwrap()
                .get(&process.identity.run_id)
                .cloned()
                .unwrap_or(RuntimeObservation::OwnershipLost))
        }

        fn stop(&self, process: &ManagedPreviewProcess) -> Result<(), String> {
            let mut observations = self.state.observations.lock().unwrap();
            if !matches!(
                observations.get(&process.identity.run_id),
                Some(RuntimeObservation::Running { .. })
            ) {
                return Err("exact managed process identity is not running".into());
            }
            self.state.stop_count.fetch_add(1, Ordering::Relaxed);
            observations.insert(
                process.identity.run_id.clone(),
                RuntimeObservation::Exited {
                    code: Some(0),
                    detail: "stopped".into(),
                },
            );
            Ok(())
        }

        fn resolve_endpoint(
            &self,
            _process: &ManagedPreviewProcess,
            _output: &[u8],
            cancellation: &ProbeCancellation,
        ) -> Result<PreviewEndpoint, EndpointError> {
            if cancellation.is_cancelled() {
                return Err(EndpointError::Cancelled);
            }
            if let Some(error) = self.state.endpoint_error.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(PreviewEndpoint {
                hinted_url: "http://localhost:4173/".into(),
                advertised_url: "http://127.0.0.1:4173/".into(),
                reachable_url: "http://127.0.0.1:4173/".into(),
                port: 4173,
            })
        }

        fn open(&self, url: &str) -> Result<(), String> {
            self.state.opened.lock().unwrap().push(url.into());
            Ok(())
        }

        fn now_ms(&self) -> u64 {
            self.state.now.fetch_add(1, Ordering::Relaxed) + 1
        }
    }

    struct Fixture {
        root: PathBuf,
        scope: PreviewScope,
        target_ref: PreviewTargetRef,
        runtime: FakeRuntime,
        profiles: Arc<PreviewProfileStore>,
        service: PreviewService<FakeRuntime>,
    }

    fn fixture(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "t-hub-preview-service-{tag}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name":"app","scripts":{"dev":"vite"}}"#,
        )
        .unwrap();
        let profiles =
            Arc::new(PreviewProfileStore::open(root.join("state/preview-profiles.json")).unwrap());
        let runtime = FakeRuntime::default();
        let service = PreviewService::new(runtime.clone(), Arc::clone(&profiles));
        let discovery = service.discover(&root).unwrap();
        let scope = PreviewScope::new("project-1", None).unwrap();
        let target_ref = PreviewTargetRef {
            scope: scope.clone(),
            target_id: discovery.targets[0].id.clone(),
            discovery_fingerprint: discovery.discovery_fingerprint,
        };
        service.select(&root, &target_ref, "select-1").unwrap();
        Fixture {
            root,
            scope,
            target_ref,
            runtime,
            profiles,
            service,
        }
    }

    #[test]
    fn same_target_start_and_replayed_request_are_unchanged() {
        let fixture = fixture("idempotent");
        let first = fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                Some(&fixture.target_ref),
                "start-1",
                &ProbeCancellation::default(),
            )
            .unwrap();
        assert_eq!(first.outcome, PreviewOperationOutcome::Applied);
        assert_eq!(first.status.state, PreviewState::Running);
        fs::write(fixture.root.join("package.json"), "not json").unwrap();
        let replayed = fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                Some(&fixture.target_ref),
                "start-1",
                &ProbeCancellation::default(),
            )
            .unwrap();
        assert_eq!(replayed.outcome, PreviewOperationOutcome::Unchanged);
        fs::write(
            fixture.root.join("package.json"),
            r#"{"name":"app","scripts":{"dev":"vite"}}"#,
        )
        .unwrap();
        let same_target = fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                None,
                "start-2",
                &ProbeCancellation::default(),
            )
            .unwrap();
        assert_eq!(same_target.outcome, PreviewOperationOutcome::Unchanged);
        assert_eq!(fixture.runtime.spawn_count(), 1);
    }

    #[test]
    fn cancellation_before_start_has_no_process_or_intent_effect() {
        let fixture = fixture("cancelled");
        let cancellation = ProbeCancellation::default();
        cancellation.cancel();
        assert!(fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                None,
                "cancelled-start",
                &cancellation,
            )
            .is_err());
        assert_eq!(fixture.runtime.spawn_count(), 0);
        assert!(fixture.profiles.intent("cancelled-start").is_none());
    }

    #[test]
    fn concurrent_starts_are_serialized_per_scope() {
        let Fixture {
            root,
            scope,
            target_ref: _,
            runtime,
            profiles: _,
            service,
        } = fixture("serialized");
        let service = Arc::new(service);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for request_id in ["concurrent-1", "concurrent-2"] {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            let root = root.clone();
            let scope = scope.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                service
                    .start(
                        &root,
                        &scope,
                        None,
                        request_id,
                        &ProbeCancellation::default(),
                    )
                    .unwrap()
                    .outcome
            }));
        }
        barrier.wait();
        let mut outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        outcomes.sort_by_key(|outcome| match outcome {
            PreviewOperationOutcome::Applied => 0,
            PreviewOperationOutcome::Unchanged => 1,
            PreviewOperationOutcome::Recovered => 2,
        });
        assert_eq!(
            outcomes,
            vec![
                PreviewOperationOutcome::Applied,
                PreviewOperationOutcome::Unchanged
            ]
        );
        assert_eq!(runtime.spawn_count(), 1);
    }

    #[test]
    fn stopped_stop_is_unchanged_and_stale_run_cannot_stop_replacement() {
        let fixture = fixture("stale-stop");
        let stopped = fixture
            .service
            .stop(&fixture.scope, None, "stop-empty")
            .unwrap();
        assert_eq!(stopped.outcome, PreviewOperationOutcome::Unchanged);

        let first = fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                None,
                "start-first",
                &ProbeCancellation::default(),
            )
            .unwrap();
        let first_run = first.status.run_id.unwrap();
        let restarted = fixture
            .service
            .restart(
                &fixture.root,
                &fixture.scope,
                "restart",
                &ProbeCancellation::default(),
            )
            .unwrap();
        let replacement = restarted.status.run_id.clone().unwrap();
        assert_ne!(first_run, replacement);
        assert_eq!(fixture.runtime.stop_count(), 1);
        let stale = fixture
            .service
            .stop(&fixture.scope, Some(&first_run), "stale-stop")
            .unwrap();
        assert_eq!(stale.outcome, PreviewOperationOutcome::Unchanged);
        assert_eq!(stale.status.state, PreviewState::Stale);
        assert_eq!(fixture.runtime.stop_count(), 1);
        assert_eq!(
            fixture.service.status(&fixture.scope).unwrap().run_id,
            Some(replacement)
        );
    }

    #[test]
    fn ownership_loss_refuses_to_stop_foreign_process() {
        let fixture = fixture("foreign");
        let started = fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                None,
                "start",
                &ProbeCancellation::default(),
            )
            .unwrap();
        let run_id = started.status.run_id.unwrap();
        fixture
            .runtime
            .set_observation(&run_id, RuntimeObservation::OwnershipLost);
        let stopped = fixture
            .service
            .stop(&fixture.scope, Some(&run_id), "stop")
            .unwrap();
        assert_eq!(stopped.status.state, PreviewState::Stale);
        assert_eq!(fixture.runtime.stop_count(), 0);
        assert_eq!(
            fixture.profiles.intent("stop").unwrap().phase,
            PreviewIntentPhase::RecoveryBlocked
        );
    }

    #[test]
    fn recovery_cleans_exact_known_run_without_spawning() {
        let fixture = fixture("recover-known");
        let started = fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                None,
                "start",
                &ProbeCancellation::default(),
            )
            .unwrap();
        let run_id = started.status.run_id.unwrap();
        fixture
            .profiles
            .record_intent(PreviewIntent {
                request_id: "crashed-restart".into(),
                scope: fixture.scope.clone(),
                operation: PreviewOperation::Restart,
                target_id: Some(fixture.target_ref.target_id.clone()),
                run_id: Some(run_id),
                phase: PreviewIntentPhase::Prepared,
                observed_status: None,
                detail: None,
                updated_at_ms: 1,
            })
            .unwrap();
        let recovered = fixture.service.recover_incomplete().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].outcome, PreviewOperationOutcome::Recovered);
        assert_eq!(recovered[0].status.state, PreviewState::Stopped);
        assert_eq!(fixture.runtime.spawn_count(), 1);
        assert_eq!(fixture.runtime.stop_count(), 1);
    }

    #[test]
    fn recovery_never_respawns_an_unproven_prepared_start() {
        let fixture = fixture("recover-blocked");
        fixture
            .profiles
            .record_intent(PreviewIntent {
                request_id: "crashed-start".into(),
                scope: fixture.scope.clone(),
                operation: PreviewOperation::Start,
                target_id: Some(fixture.target_ref.target_id.clone()),
                run_id: Some("lost-run".into()),
                phase: PreviewIntentPhase::Prepared,
                observed_status: None,
                detail: None,
                updated_at_ms: 1,
            })
            .unwrap();
        assert!(fixture.service.recover_incomplete().unwrap().is_empty());
        assert_eq!(fixture.runtime.spawn_count(), 0);
        assert_eq!(
            fixture.profiles.intent("crashed-start").unwrap().phase,
            PreviewIntentPhase::RecoveryBlocked
        );
    }

    #[test]
    fn refresh_and_open_use_only_the_verified_reachable_url() {
        let fixture = fixture("open");
        fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                None,
                "start",
                &ProbeCancellation::default(),
            )
            .unwrap();
        let refreshed = fixture
            .service
            .refresh(&fixture.scope, "refresh", &ProbeCancellation::default())
            .unwrap();
        assert_eq!(refreshed.status.state, PreviewState::Running);
        let opened = fixture.service.open(&fixture.scope, "open").unwrap();
        assert_eq!(opened.outcome, PreviewOperationOutcome::Applied);
        assert_eq!(
            *fixture.runtime.state.opened.lock().unwrap(),
            vec!["http://127.0.0.1:4173/"]
        );
    }
}
