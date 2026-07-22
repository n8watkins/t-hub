//! Per-scope serialized Preview lifecycle service.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use super::discovery::{PreviewDiscovery, PreviewDiscoveryCache};
use super::endpoint::{EndpointError, ProbeCancellation};
use super::model::{
    PreviewOperation, PreviewOperationOutcome, PreviewOperationResult, PreviewScope, PreviewState,
    PreviewStatus, PreviewTarget, PreviewTargetRef,
};
use super::profile::{
    PreviewIntent, PreviewIntentPhase, PreviewProfileStore, ProjectPreviewProfile,
    SelectedPreviewTarget,
};
use super::runtime::{
    ManagedPreviewProcess, PreviewRuntime, RuntimeObservation, RuntimeRediscovery,
};

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
    scope_locks: Mutex<HashMap<PreviewScope, Arc<Mutex<()>>>>,
    active: Mutex<HashMap<PreviewScope, ActiveRun>>,
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
        self.prepare_intent(request_id, PreviewOperation::Select, target_ref, None, None)?;

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
        let target_ref = match requested {
            Some(target_ref) => target_ref.clone(),
            None => self.persisted_selected_ref(scope)?,
        };
        if let Some(replayed) = self.replay_optional(
            request_id,
            PreviewOperation::Start,
            scope,
            Some(&target_ref),
            None,
        )? {
            return Ok(replayed);
        }
        if cancellation.is_cancelled() {
            return Err("Preview start was cancelled before spawning".into());
        }
        let discovery = self.discovery.discover(root)?;
        if requested.is_none() {
            let _ = self.selected_ref(scope, &discovery)?;
        }
        let target = resolve_target(&discovery, &target_ref)?.clone();
        let run_id = Uuid::new_v4().to_string();
        let expected_stop = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(scope)
            .map(|active| active.process.clone());
        self.prepare_intent(
            request_id,
            PreviewOperation::Start,
            &target_ref,
            Some(&run_id),
            expected_stop.as_ref(),
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
            .get(scope)
            .cloned();
        let target_ref = active.as_ref().map(|run| run.process.target.clone());
        if let Some(replayed) = self.replay_optional(
            request_id,
            PreviewOperation::Stop,
            scope,
            None,
            expected_run_id,
        )? {
            return Ok(replayed);
        }
        self.prepare_optional_intent(
            request_id,
            PreviewOperation::Stop,
            scope,
            target_ref.as_ref(),
            active
                .as_ref()
                .map(|run| run.process.identity.run_id.as_str()),
            expected_run_id,
            active.as_ref().map(|run| &run.process),
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
                    .remove(scope);
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
                    .remove(scope);
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
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(scope)
            .cloned();
        let target_ref = match active.as_ref() {
            Some(active) => active.process.target.clone(),
            None => self.persisted_selected_ref(scope)?,
        };
        if let Some(replayed) = self.replay_optional(
            request_id,
            PreviewOperation::Restart,
            scope,
            Some(&target_ref),
            None,
        )? {
            return Ok(replayed);
        }
        if cancellation.is_cancelled() {
            return Err("Preview restart was cancelled before spawning".into());
        }
        let discovery = self.discovery.discover(root)?;
        if active.is_none() {
            let _ = self.selected_ref(scope, &discovery)?;
        }
        let target = resolve_target(&discovery, &target_ref)?.clone();
        let run_id = Uuid::new_v4().to_string();
        self.prepare_intent(
            request_id,
            PreviewOperation::Restart,
            &target_ref,
            Some(&run_id),
            active.as_ref().map(|active| &active.process),
        )?;
        if let Some(active) = active {
            match self.runtime.observe(&active.process)? {
                RuntimeObservation::Running { .. } => {
                    if let Err(error) = self.runtime.stop(&active.process) {
                        self.block_intent(request_id, &error)?;
                        return Err(error);
                    }
                }
                RuntimeObservation::Exited { .. } => {}
                RuntimeObservation::OwnershipLost => {
                    let detail = "refused to restart a reused or foreign Preview process";
                    self.block_intent(request_id, detail)?;
                    return Err(detail.into());
                }
            }
            self.active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(scope);
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
            self.replay_optional(request_id, PreviewOperation::Refresh, scope, None, None)?
        {
            return Ok(replayed);
        }
        self.prepare_optional_intent(
            request_id,
            PreviewOperation::Refresh,
            scope,
            None,
            None,
            None,
            None,
        )?;
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
            .get(scope)
            .cloned();
        if let Some(replayed) =
            self.replay_optional(request_id, PreviewOperation::Open, scope, None, None)?
        {
            return Ok(replayed);
        }
        self.prepare_optional_intent(
            request_id,
            PreviewOperation::Open,
            scope,
            active.as_ref().map(|run| &run.process.target),
            None,
            None,
            None,
        )?;
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
            if matches!(
                intent.operation,
                PreviewOperation::Refresh | PreviewOperation::Open
            ) {
                if intent.operation == PreviewOperation::Open
                    && intent.phase == PreviewIntentPhase::Prepared
                {
                    self.block_intent(
                        &intent.request_id,
                        "prepared open effect cannot be proven after restart",
                    )?;
                    continue;
                }
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
            if let Some(status) = self.recover_process_intent(&intent)? {
                recovered.push(result(
                    intent.operation,
                    PreviewOperationOutcome::Recovered,
                    &intent.request_id,
                    status,
                ));
            }
        }
        Ok(recovered)
    }

    fn recover_process_intent(
        &self,
        intent: &PreviewIntent,
    ) -> Result<Option<PreviewStatus>, String> {
        if intent.operation == PreviewOperation::Stop && intent.expected_stop_run.is_none() {
            let status = PreviewStatus::stopped(intent.scope.clone(), self.runtime.now_ms());
            self.commit_intent(&intent.request_id, status.clone())?;
            return Ok(Some(status));
        }
        let target = match intent_target_ref(intent) {
            Ok(target) => target,
            Err(error) => {
                self.block_intent(&intent.request_id, &error)?;
                return Ok(None);
            }
        };
        if intent.operation == PreviewOperation::Stop {
            let expected = intent.expected_stop_run.as_ref().expect("checked above");
            let expected_target = intent.expected_stop_target.as_ref().unwrap_or(&target);
            return self.recover_stop_identity(intent, expected_target, expected);
        }
        if !matches!(
            intent.operation,
            PreviewOperation::Start | PreviewOperation::Restart
        ) {
            self.block_intent(&intent.request_id, "unsupported Preview recovery operation")?;
            return Ok(None);
        }
        let Some(run_id) = intent.run_id.as_deref() else {
            self.block_intent(&intent.request_id, "prepared start has no durable run id")?;
            return Ok(None);
        };
        match self.runtime.rediscover(
            &intent.scope,
            &target,
            run_id,
            intent.managed_run.as_ref(),
        )? {
            RuntimeRediscovery::Exact(process) => {
                if !valid_rediscovered_process(
                    &process,
                    &target,
                    run_id,
                    intent.managed_run.as_ref(),
                ) {
                    self.block_intent(
                        &intent.request_id,
                        "rediscovered Preview process failed exact identity validation",
                    )?;
                    return Ok(None);
                }
                match self.runtime.observe(&process)? {
                    RuntimeObservation::OwnershipLost => {
                        self.block_intent(
                            &intent.request_id,
                            "rediscovered Preview process lost exact ownership",
                        )?;
                        Ok(None)
                    }
                    RuntimeObservation::Running { .. } | RuntimeObservation::Exited { .. } => {
                        self.active
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(
                                intent.scope.clone(),
                                ActiveRun {
                                    process,
                                    state: PreviewState::Starting,
                                    preview_url: None,
                                    reason: None,
                                },
                            );
                        let status =
                            self.status_locked(&intent.scope, &ProbeCancellation::default())?;
                        self.commit_intent(&intent.request_id, status.clone())?;
                        Ok(Some(status))
                    }
                }
            }
            RuntimeRediscovery::Absent => {
                if let Some(expected) = intent.expected_stop_run.as_ref() {
                    let Some(expected_target) = intent.expected_stop_target.as_ref() else {
                        self.block_intent(
                            &intent.request_id,
                            "expected stop identity has no durable target reference",
                        )?;
                        return Ok(None);
                    };
                    if intent.operation == PreviewOperation::Start && expected_target == &target {
                        match self.runtime.rediscover(
                            &intent.scope,
                            expected_target,
                            &expected.run_id,
                            Some(expected),
                        )? {
                            RuntimeRediscovery::Exact(process)
                                if valid_rediscovered_process(
                                    &process,
                                    expected_target,
                                    &expected.run_id,
                                    Some(expected),
                                ) =>
                            {
                                self.active
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .insert(
                                        intent.scope.clone(),
                                        ActiveRun {
                                            process,
                                            state: PreviewState::Starting,
                                            preview_url: None,
                                            reason: None,
                                        },
                                    );
                                let status = self
                                    .status_locked(&intent.scope, &ProbeCancellation::default())?;
                                self.commit_intent(&intent.request_id, status.clone())?;
                                return Ok(Some(status));
                            }
                            RuntimeRediscovery::Absent => {}
                            RuntimeRediscovery::Exact(_)
                            | RuntimeRediscovery::Ambiguous
                            | RuntimeRediscovery::Foreign => {
                                self.block_intent(
                                    &intent.request_id,
                                    "existing Preview run rediscovery was ambiguous or foreign",
                                )?;
                                return Ok(None);
                            }
                        }
                    } else {
                        return self.recover_stop_identity(intent, expected_target, expected);
                    }
                }
                let status = PreviewStatus::stopped(intent.scope.clone(), self.runtime.now_ms());
                self.commit_intent(&intent.request_id, status.clone())?;
                Ok(Some(status))
            }
            RuntimeRediscovery::Ambiguous | RuntimeRediscovery::Foreign => {
                self.block_intent(
                    &intent.request_id,
                    "Preview runtime rediscovery was ambiguous or foreign",
                )?;
                Ok(None)
            }
        }
    }

    fn recover_stop_identity(
        &self,
        intent: &PreviewIntent,
        target: &PreviewTargetRef,
        expected: &super::endpoint::ManagedRunIdentity,
    ) -> Result<Option<PreviewStatus>, String> {
        match self
            .runtime
            .rediscover(&intent.scope, target, &expected.run_id, Some(expected))?
        {
            RuntimeRediscovery::Exact(process) => {
                if !valid_rediscovered_process(&process, target, &expected.run_id, Some(expected)) {
                    self.block_intent(
                        &intent.request_id,
                        "rediscovered stop process failed exact identity validation",
                    )?;
                    return Ok(None);
                }
                match self.runtime.observe(&process)? {
                    RuntimeObservation::Running { .. } => {
                        if let Err(error) = self.runtime.stop(&process) {
                            self.block_intent(&intent.request_id, &error)?;
                            return Ok(None);
                        }
                    }
                    RuntimeObservation::Exited { .. } => {}
                    RuntimeObservation::OwnershipLost => {
                        self.block_intent(
                            &intent.request_id,
                            "recovery refused a reused or foreign process",
                        )?;
                        return Ok(None);
                    }
                }
            }
            RuntimeRediscovery::Absent => {}
            RuntimeRediscovery::Ambiguous | RuntimeRediscovery::Foreign => {
                self.block_intent(
                    &intent.request_id,
                    "Preview stop rediscovery was ambiguous or foreign",
                )?;
                return Ok(None);
            }
        }
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&intent.scope);
        let status = PreviewStatus::stopped(intent.scope.clone(), self.runtime.now_ms());
        self.commit_intent(&intent.request_id, status.clone())?;
        Ok(Some(status))
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
            .get(scope)
            .cloned();
        if let Some(current) = current {
            if current.process.target == *target_ref {
                match self.runtime.observe(&current.process)? {
                    RuntimeObservation::Running { .. } => {
                        let status = self.status_locked(scope, cancellation)?;
                        self.commit_intent(request_id, status.clone())?;
                        return Ok(result(
                            operation,
                            PreviewOperationOutcome::Unchanged,
                            request_id,
                            status,
                        ));
                    }
                    RuntimeObservation::OwnershipLost => {
                        let detail = "refused to replace a reused or foreign Preview process";
                        self.block_intent(request_id, detail)?;
                        return Err(detail.into());
                    }
                    RuntimeObservation::Exited { .. } => {}
                }
            } else {
                match self.runtime.observe(&current.process)? {
                    RuntimeObservation::Running { .. } => {
                        if let Err(error) = self.runtime.stop(&current.process) {
                            self.block_intent(request_id, &error)?;
                            return Err(error);
                        }
                    }
                    RuntimeObservation::OwnershipLost => {
                        let detail = "refused to replace a reused or foreign Preview process";
                        self.block_intent(request_id, detail)?;
                        return Err(detail.into());
                    }
                    RuntimeObservation::Exited { .. } => {}
                }
            }
            self.active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(scope);
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
        let starting = PreviewStatus {
            scope: scope.clone(),
            state: PreviewState::Starting,
            target_id: Some(target_ref.target_id.clone()),
            run_id: Some(process.identity.run_id.clone()),
            preview_url: None,
            reason: None,
            observed_at_ms: self.runtime.now_ms(),
        };
        if let Err(error) = self.profiles.observe_managed_run(
            request_id,
            process.identity.clone(),
            starting,
            self.runtime.now_ms(),
        ) {
            let _ = self.runtime.stop(&process);
            return Err(error);
        }
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                scope.clone(),
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
            .get(scope)
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
            .insert(scope.clone(), active.clone());
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
        self.persisted_selected_ref(scope)
    }

    fn persisted_selected_ref(&self, scope: &PreviewScope) -> Result<PreviewTargetRef, String> {
        let profile = self
            .profiles
            .project(&scope.project_id)
            .ok_or_else(|| "no Preview target is selected for this project".to_string())?;
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
            .entry(scope.clone())
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
            Some(target_ref),
            None,
        )
    }

    fn replay_optional(
        &self,
        request_id: &str,
        operation: PreviewOperation,
        scope: &PreviewScope,
        target_ref: Option<&PreviewTargetRef>,
        requested_stop_run_id: Option<&str>,
    ) -> Result<Option<PreviewOperationResult>, String> {
        let Some(intent) = self.profiles.intent(request_id) else {
            return Ok(None);
        };
        if intent.operation != operation
            || intent.scope != *scope
            || target_ref.is_some_and(|target| {
                intent.target_id.as_ref() != Some(&target.target_id)
                    || intent.discovery_fingerprint.as_deref()
                        != Some(target.discovery_fingerprint.as_str())
            })
            || intent.requested_stop_run_id.as_deref() != requested_stop_run_id
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
        expected_stop: Option<&ManagedPreviewProcess>,
    ) -> Result<(), String> {
        self.prepare_optional_intent(
            request_id,
            operation,
            &target_ref.scope,
            Some(target_ref),
            run_id,
            None,
            expected_stop,
        )
    }

    fn prepare_optional_intent(
        &self,
        request_id: &str,
        operation: PreviewOperation,
        scope: &PreviewScope,
        target_ref: Option<&PreviewTargetRef>,
        run_id: Option<&str>,
        requested_stop_run_id: Option<&str>,
        expected_stop: Option<&ManagedPreviewProcess>,
    ) -> Result<(), String> {
        self.profiles.record_intent(PreviewIntent {
            request_id: request_id.into(),
            scope: scope.clone(),
            operation,
            target_id: target_ref.map(|target| target.target_id.clone()),
            discovery_fingerprint: target_ref.map(|target| target.discovery_fingerprint.clone()),
            run_id: run_id.map(str::to_string),
            requested_stop_run_id: requested_stop_run_id.map(str::to_string),
            managed_run: None,
            expected_stop_run: expected_stop.map(|process| process.identity.clone()),
            expected_stop_target: expected_stop.map(|process| process.target.clone()),
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

fn intent_target_ref(intent: &PreviewIntent) -> Result<PreviewTargetRef, String> {
    Ok(PreviewTargetRef {
        scope: intent.scope.clone(),
        target_id: intent
            .target_id
            .clone()
            .ok_or_else(|| "Preview recovery intent has no target id".to_string())?,
        discovery_fingerprint: intent
            .discovery_fingerprint
            .clone()
            .ok_or_else(|| "Preview recovery intent has no discovery fingerprint".to_string())?,
    })
}

fn valid_rediscovered_process(
    process: &ManagedPreviewProcess,
    target: &PreviewTargetRef,
    run_id: &str,
    expected: Option<&super::endpoint::ManagedRunIdentity>,
) -> bool {
    process.identity.validate().is_ok()
        && process.identity.run_id == run_id
        && process.target == *target
        && expected.is_none_or(|expected| expected == &process.identity)
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
        processes: Mutex<HashMap<String, ManagedPreviewProcess>>,
        opened: Mutex<Vec<String>>,
        fail_spawn: AtomicBool,
        endpoint_error: Mutex<Option<EndpointError>>,
        ambiguous_rediscovery: AtomicBool,
    }

    impl FakeRuntime {
        fn restart_facade(&self) -> Self {
            Self {
                state: Arc::clone(&self.state),
            }
        }

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
            self.state
                .processes
                .lock()
                .unwrap()
                .insert(run_id.into(), process.clone());
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

        fn rediscover(
            &self,
            scope: &PreviewScope,
            target: &PreviewTargetRef,
            run_id: &str,
            expected: Option<&ManagedRunIdentity>,
        ) -> Result<RuntimeRediscovery, String> {
            if self.state.ambiguous_rediscovery.load(Ordering::Relaxed) {
                return Ok(RuntimeRediscovery::Ambiguous);
            }
            let Some(process) = self.state.processes.lock().unwrap().get(run_id).cloned() else {
                return Ok(RuntimeRediscovery::Absent);
            };
            if process.target.scope != *scope
                || process.target != *target
                || expected.is_some_and(|identity| identity != &process.identity)
            {
                return Ok(RuntimeRediscovery::Foreign);
            }
            Ok(RuntimeRediscovery::Exact(process))
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
        profile_path: PathBuf,
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
        let profile_path = root.join("state/preview-profiles.json");
        let profiles = Arc::new(PreviewProfileStore::open(&profile_path).unwrap());
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
            profile_path,
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
        assert_eq!(
            fixture
                .profiles
                .intent("start-1")
                .unwrap()
                .managed_run
                .as_ref()
                .map(|identity| identity.run_id.as_str()),
            first.status.run_id.as_deref()
        );
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
    fn replay_binds_target_fingerprint_and_requested_stop_run() {
        let fixture = fixture("replay-binding");
        let mut changed_fingerprint = fixture.target_ref.clone();
        changed_fingerprint.discovery_fingerprint = format!("sha256:{}", "f".repeat(64));
        assert!(fixture
            .service
            .select(&fixture.root, &changed_fingerprint, "select-1")
            .is_err());

        let started = fixture
            .service
            .start(
                &fixture.root,
                &fixture.scope,
                Some(&fixture.target_ref),
                "binding-start",
                &ProbeCancellation::default(),
            )
            .unwrap();
        let run_id = started.status.run_id.unwrap();
        fixture
            .service
            .stop(&fixture.scope, Some(&run_id), "binding-stop")
            .unwrap();
        assert!(fixture
            .service
            .stop(&fixture.scope, None, "binding-stop")
            .is_err());
        assert_eq!(fixture.runtime.spawn_count(), 1);
        assert_eq!(fixture.runtime.stop_count(), 1);
    }

    #[test]
    fn start_and_restart_refuse_to_replace_foreign_process_identity() {
        let start_fixture = fixture("foreign-start-replacement");
        let started = start_fixture
            .service
            .start(
                &start_fixture.root,
                &start_fixture.scope,
                Some(&start_fixture.target_ref),
                "initial-start",
                &ProbeCancellation::default(),
            )
            .unwrap();
        start_fixture.runtime.set_observation(
            started.status.run_id.as_deref().unwrap(),
            RuntimeObservation::OwnershipLost,
        );
        assert!(start_fixture
            .service
            .start(
                &start_fixture.root,
                &start_fixture.scope,
                Some(&start_fixture.target_ref),
                "foreign-replacement-start",
                &ProbeCancellation::default(),
            )
            .is_err());
        assert_eq!(start_fixture.runtime.spawn_count(), 1);
        assert_eq!(start_fixture.runtime.stop_count(), 0);
        assert_eq!(
            start_fixture
                .profiles
                .intent("foreign-replacement-start")
                .unwrap()
                .phase,
            PreviewIntentPhase::RecoveryBlocked
        );

        let restart_fixture = fixture("foreign-restart-replacement");
        let started = restart_fixture
            .service
            .start(
                &restart_fixture.root,
                &restart_fixture.scope,
                Some(&restart_fixture.target_ref),
                "initial-restart-start",
                &ProbeCancellation::default(),
            )
            .unwrap();
        restart_fixture.runtime.set_observation(
            started.status.run_id.as_deref().unwrap(),
            RuntimeObservation::OwnershipLost,
        );
        assert!(restart_fixture
            .service
            .restart(
                &restart_fixture.root,
                &restart_fixture.scope,
                "foreign-restart",
                &ProbeCancellation::default(),
            )
            .is_err());
        assert_eq!(restart_fixture.runtime.spawn_count(), 1);
        assert_eq!(restart_fixture.runtime.stop_count(), 0);
        assert_eq!(
            restart_fixture
                .profiles
                .intent("foreign-restart")
                .unwrap()
                .phase,
            PreviewIntentPhase::RecoveryBlocked
        );
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
            profile_path: _,
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
    fn formerly_colliding_scope_strings_have_independent_lifecycles() {
        let fixture = fixture("scope-collision");
        let project_scope = PreviewScope::new("a:b", None).unwrap();
        let workspace_scope = PreviewScope::new("a", Some("b".into())).unwrap();
        let project_target = PreviewTargetRef {
            scope: project_scope.clone(),
            target_id: fixture.target_ref.target_id.clone(),
            discovery_fingerprint: fixture.target_ref.discovery_fingerprint.clone(),
        };
        let workspace_target = PreviewTargetRef {
            scope: workspace_scope.clone(),
            target_id: fixture.target_ref.target_id.clone(),
            discovery_fingerprint: fixture.target_ref.discovery_fingerprint.clone(),
        };
        let project_run = fixture
            .service
            .start(
                &fixture.root,
                &project_scope,
                Some(&project_target),
                "scope-project",
                &ProbeCancellation::default(),
            )
            .unwrap();
        let workspace_run = fixture
            .service
            .start(
                &fixture.root,
                &workspace_scope,
                Some(&workspace_target),
                "scope-workspace",
                &ProbeCancellation::default(),
            )
            .unwrap();
        assert_ne!(project_run.status.run_id, workspace_run.status.run_id);
        assert_eq!(fixture.runtime.spawn_count(), 2);
        assert_eq!(
            fixture.service.status(&project_scope).unwrap().scope,
            project_scope
        );
        assert_eq!(
            fixture.service.status(&workspace_scope).unwrap().scope,
            workspace_scope
        );
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
    fn fresh_service_rediscovers_spawned_prepared_run_by_durable_identity() {
        let fixture = fixture("recover-known");
        let run_id = "prepared-run";
        fixture
            .profiles
            .record_intent(PreviewIntent {
                request_id: "crashed-start".into(),
                scope: fixture.scope.clone(),
                operation: PreviewOperation::Start,
                target_id: Some(fixture.target_ref.target_id.clone()),
                discovery_fingerprint: Some(fixture.target_ref.discovery_fingerprint.clone()),
                run_id: Some(run_id.into()),
                requested_stop_run_id: None,
                managed_run: None,
                expected_stop_run: None,
                expected_stop_target: None,
                phase: PreviewIntentPhase::Prepared,
                observed_status: None,
                detail: None,
                updated_at_ms: 1,
            })
            .unwrap();
        let discovery = fixture.service.discover(&fixture.root).unwrap();
        fixture
            .runtime
            .spawn(
                &fixture.scope,
                &discovery.canonical_root,
                &discovery.targets[0],
                &fixture.target_ref,
                run_id,
            )
            .unwrap();

        let restarted_profiles =
            Arc::new(PreviewProfileStore::open(&fixture.profile_path).unwrap());
        let restarted = PreviewService::new(
            fixture.runtime.restart_facade(),
            Arc::clone(&restarted_profiles),
        );
        let recovered = restarted.recover_incomplete().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].outcome, PreviewOperationOutcome::Recovered);
        assert_eq!(recovered[0].status.state, PreviewState::Running);
        assert_eq!(recovered[0].status.run_id.as_deref(), Some(run_id));
        assert_eq!(
            restarted.status(&fixture.scope).unwrap().state,
            PreviewState::Running
        );
        assert_eq!(fixture.runtime.spawn_count(), 1);
        assert_eq!(fixture.runtime.stop_count(), 0);
        assert_eq!(
            restarted_profiles.intent("crashed-start").unwrap().phase,
            PreviewIntentPhase::Committed
        );
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
                discovery_fingerprint: Some(fixture.target_ref.discovery_fingerprint.clone()),
                run_id: Some("lost-run".into()),
                requested_stop_run_id: None,
                managed_run: None,
                expected_stop_run: None,
                expected_stop_target: None,
                phase: PreviewIntentPhase::Prepared,
                observed_status: None,
                detail: None,
                updated_at_ms: 1,
            })
            .unwrap();
        let recovered = fixture.service.recover_incomplete().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status.state, PreviewState::Stopped);
        assert_eq!(fixture.runtime.spawn_count(), 0);
        assert_eq!(
            fixture.profiles.intent("crashed-start").unwrap().phase,
            PreviewIntentPhase::Committed
        );
    }

    #[test]
    fn fresh_service_blocks_ambiguous_process_rediscovery() {
        let fixture = fixture("recover-ambiguous");
        fixture
            .profiles
            .record_intent(PreviewIntent {
                request_id: "ambiguous-start".into(),
                scope: fixture.scope.clone(),
                operation: PreviewOperation::Start,
                target_id: Some(fixture.target_ref.target_id.clone()),
                discovery_fingerprint: Some(fixture.target_ref.discovery_fingerprint.clone()),
                run_id: Some("ambiguous-run".into()),
                requested_stop_run_id: None,
                managed_run: None,
                expected_stop_run: None,
                expected_stop_target: None,
                phase: PreviewIntentPhase::Prepared,
                observed_status: None,
                detail: None,
                updated_at_ms: 1,
            })
            .unwrap();
        fixture
            .runtime
            .state
            .ambiguous_rediscovery
            .store(true, Ordering::Relaxed);
        let restarted_profiles =
            Arc::new(PreviewProfileStore::open(&fixture.profile_path).unwrap());
        let restarted = PreviewService::new(
            fixture.runtime.restart_facade(),
            Arc::clone(&restarted_profiles),
        );
        assert!(restarted.recover_incomplete().unwrap().is_empty());
        assert_eq!(
            restarted.status(&fixture.scope).unwrap().state,
            PreviewState::Stopped
        );
        assert_eq!(
            restarted_profiles.intent("ambiguous-start").unwrap().phase,
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
