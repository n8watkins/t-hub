//! The `CaptainsRegistry` state-machine methods, split out of `control.rs` to
//! shrink that module. This is the inherent `impl CaptainsRegistry` block only -
//! the struct, its supporting types, the `#[cfg(test)]` helper impl, and the
//! `Default` impl stay in the parent module. Inherent methods resolve crate-wide,
//! so a bare `mod captains_registry;` include (no glob import) is all the parent
//! needs; `use super::*;` pulls the parent items the methods reference.

use super::*;

impl CaptainsRegistry {
    #[cfg(test)]
    pub(super) fn pause_dispatch(&self, boundary: &'static str) -> bool {
        let mut configured = self.dispatch_barrier.lock().unwrap();
        let barrier = configured.take();
        if barrier
            .as_ref()
            .is_some_and(|configured_barrier| configured_barrier.boundary != boundary)
        {
            *configured = barrier;
            return true;
        }
        drop(configured);
        if let Some(barrier) = barrier {
            let _ = barrier.reached.send(boundary);
            barrier.resume.recv().is_ok()
        } else {
            true
        }
    }

    #[cfg(test)]
    pub(super) fn set_dispatch_barrier(&self, barrier: Option<DispatchBarrier>) {
        *self.dispatch_barrier.lock().unwrap() = barrier;
    }
    pub(super) fn serialize_crew_powder_operation(
        &self,
        crew_session_id: &str,
        operation: CrewPowderOperationKind,
    ) -> CrewPowderOperationGuard<'_> {
        let mut inflight = self
            .powder_operations_inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while inflight.contains_key(crew_session_id) {
            inflight = self
                .powder_operation_ready
                .wait(inflight)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        inflight.insert(crew_session_id.to_string(), operation);
        CrewPowderOperationGuard {
            registry: self,
            crew_session_id: crew_session_id.to_string(),
        }
    }

    /// An empty, in-memory registry (tests / headless proofs - no persistence).
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CaptainsInner::default()),
            authority_epoch: next_authority_registry_epoch(),
            mutation: Mutex::new(()),
            provision: Mutex::new(()),
            git_initialization: Mutex::new(()),
            powder_operations_inflight: Mutex::new(std::collections::HashMap::new()),
            powder_operation_ready: Condvar::new(),
            workspace_projection_exclusions: Mutex::new(std::collections::HashMap::new()),
            #[cfg(test)]
            historical_scope_capture_hook: Mutex::new(None),
            #[cfg(test)]
            dispatch_barrier: Mutex::new(None),
            path: None,
            write_blocked: None,
            persist: Mutex::new(0),
            #[cfg(test)]
            persist_hook: Mutex::new(None),
            #[cfg(test)]
            fail_next_persist: Mutex::new(None),
        }
    }

    /// Load the registry from `path`, falling back to the last validated backup
    /// and quarantining a corrupt primary. A first run with no files starts empty;
    /// an unrecoverable corrupt file is reported before starting empty. An
    /// incompatible dispatch-release recovery is different from corruption: both
    /// primary and backup bytes are preserved and every mutation is blocked.
    pub fn load(path: PathBuf) -> Self {
        let backup = path.with_extension("json.bak");
        let primary = if !path.exists() && !backup.exists() {
            Ok(CaptainsSnapshot {
                schema_version: CAPTAINS_SCHEMA_VERSION,
                seq: 0,
                captains: Vec::new(),
                cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
                agent_sessions: Vec::new(),
                agent_checkpoints: Vec::new(),
                agent_events: Vec::new(),
                projects: Vec::new(),
                workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
                pending_fleet_operations: Vec::new(),
                retired_fleet_tile_ids: Vec::new(),
                pending_dispatch_claims: Vec::new(),
                pending_dispatch_releases: Vec::new(),
                pending_git_initializations: Vec::new(),
            })
        } else {
            Self::read_snapshot(&path)
        };
        let backup_probe = backup.exists().then(|| Self::read_snapshot(&backup));
        let mut recovered_from_backup = false;
        let mut write_blocked = None;
        let incompatible_recovery = match (&primary, &backup_probe) {
            (Err(error @ SnapshotReadError::IncompatibleRecovery { .. }), _) => {
                Some(error.to_string())
            }
            (_, Some(Err(error @ SnapshotReadError::IncompatibleRecovery { .. }))) => {
                Some(error.to_string())
            }
            _ => None,
        };
        let future_schema = match (&primary, &backup_probe) {
            (Err(error @ SnapshotReadError::UnsupportedSchema { .. }), _) => {
                Some(error.to_string())
            }
            (_, Some(Err(error @ SnapshotReadError::UnsupportedSchema { .. }))) => {
                Some(error.to_string())
            }
            _ => None,
        };
        let mut loaded = if let Some(reason) = incompatible_recovery {
            // A legacy or unknown recovery record can name an exact remote claim.
            // Never use an older backup, quarantine either copy, or expose a
            // partially loaded registry that could redispatch it.  Explicit safe
            // handling is required before any local or remote action resumes.
            write_blocked = Some(reason.clone());
            Err(SnapshotReadError::IncompatibleRecovery { path: path.clone() })
        } else if let Some(reason) = future_schema {
            // Either file may be the last copy written by a newer T-Hub. Preserve
            // both byte-for-byte and block every write. A supported primary remains
            // readable, but an unsupported primary is never replaced by an older
            // backup and neither file is mislabeled as corruption.
            write_blocked = Some(reason);
            primary
        } else {
            match primary {
                Ok(snapshot) => Ok(snapshot),
                Err(primary_error) => {
                    let backup_snapshot = backup_probe
                        .unwrap_or_else(|| Self::read_snapshot(&backup))
                        .map_err(|backup_error| {
                            SnapshotReadError::Invalid(format!(
                                "captains registry primary failed ({primary_error}); backup failed ({backup_error})"
                            ))
                        });
                    recovered_from_backup = backup_snapshot.is_ok();
                    if backup_snapshot.is_err() && path.exists() {
                        let quarantine = path.with_extension(format!("json.corrupt.{}", now_ms()));
                        let _ = std::fs::rename(&path, &quarantine);
                        eprintln!(
                            "t-hub-control: captains registry was quarantined at '{}': {primary_error}",
                            quarantine.display()
                        );
                    }
                    backup_snapshot
                }
            }
        };
        if let Ok(snapshot) = &mut loaded {
            if snapshot.schema_version < CAPTAINS_SCHEMA_VERSION
                && snapshot.cortana.legacy_orphan_provenance.is_none()
            {
                snapshot.cortana.legacy_orphan_provenance =
                    Self::recover_schema18_cortana_provenance(&path, snapshot);
            }
        }
        if recovered_from_backup {
            if path.exists() {
                let quarantine = path.with_extension(format!("json.corrupt.{}", now_ms()));
                let _ = std::fs::rename(&path, &quarantine);
                eprintln!(
                    "t-hub-control: recovered captains registry from '{}' and quarantined the invalid primary at '{}'",
                    backup.display(),
                    quarantine.display()
                );
            } else {
                eprintln!(
                    "t-hub-control: recovered missing captains registry from '{}'",
                    backup.display()
                );
            }
        }
        let inner = loaded
            .map_err(|error| {
                eprintln!("t-hub-control: starting with an empty captains registry: {error}");
                error
            })
            .ok()
            .map(|snap| {
                // D2/MED-6: the versioned reader accepts BOTH schema versions (the
                // field aliases + `deserialize_crew` upgrade a v0 record's shape) and
                // then reconciles the Cortana singleton from the live incumbent.
                let mut captains = snap.captains;
                Self::reconcile_on_load(&mut captains);
                let mut cortana = snap.cortana;
                Self::reconcile_cortana_on_load(&captains, &mut cortana);
                let workspaces = Self::reconcile_durable_workspaces(&captains, snap.workspaces);
                CaptainsInner {
                    captains,
                    cortana,
                    agent_sessions: snap.agent_sessions,
                    agent_checkpoints: snap.agent_checkpoints,
                    agent_events: snap.agent_events,
                    projects: snap.projects,
                    workspaces,
                    pending_fleet_operations: snap.pending_fleet_operations,
                    retired_fleet_tile_ids: snap.retired_fleet_tile_ids,
                    pending_dispatch_claims: snap.pending_dispatch_claims,
                    pending_dispatch_releases: snap.pending_dispatch_releases,
                    pending_git_initializations: snap.pending_git_initializations,
                    seq: snap.seq,
                    authority_generations: AuthorityGenerations::default(),
                }
            })
            .unwrap_or_default();
        // N3: seed the persist guard from the LOADED seq, not 0, so a stale
        // in-memory snapshot (seq <= what's already on disk) can't rewrite the file
        // redundantly on startup - the monotonic guard is correct from the first
        // write, not just after the first mutation.
        let loaded_seq = inner.seq;
        let registry = Self {
            inner: Mutex::new(inner),
            authority_epoch: next_authority_registry_epoch(),
            mutation: Mutex::new(()),
            provision: Mutex::new(()),
            git_initialization: Mutex::new(()),
            powder_operations_inflight: Mutex::new(std::collections::HashMap::new()),
            powder_operation_ready: Condvar::new(),
            workspace_projection_exclusions: Mutex::new(std::collections::HashMap::new()),
            #[cfg(test)]
            historical_scope_capture_hook: Mutex::new(None),
            #[cfg(test)]
            dispatch_barrier: Mutex::new(None),
            path: Some(path),
            write_blocked,
            persist: Mutex::new(loaded_seq),
            #[cfg(test)]
            persist_hook: Mutex::new(None),
            #[cfg(test)]
            fail_next_persist: Mutex::new(None),
        };
        registry.recover_pending_git_initializations();
        registry
    }

    pub(super) fn read_snapshot(path: &Path) -> Result<CaptainsSnapshot, SnapshotReadError> {
        let body = std::fs::read_to_string(path).map_err(|error| {
            SnapshotReadError::Invalid(format!("'{}' could not be read: {error}", path.display()))
        })?;
        let mut document: Value = serde_json::from_str(&body).map_err(|error| {
            SnapshotReadError::Invalid(format!("'{}' is invalid JSON: {error}", path.display()))
        })?;
        let schema_version = document
            .get("schemaVersion")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if schema_version > CAPTAINS_SCHEMA_VERSION as u64 {
            return Err(SnapshotReadError::UnsupportedSchema {
                path: path.to_path_buf(),
                version: schema_version as u32,
            });
        }
        let has_release_recovery = document
            .get("pendingDispatchReleases")
            .and_then(Value::as_array)
            .is_some_and(|releases| !releases.is_empty());
        let has_cortana_orphan_recovery = document
            .pointer("/cortana/recovery/kind")
            .and_then(Value::as_str)
            == Some("replacingOrphan");
        let has_cortana_legacy_provenance = document
            .pointer("/cortana/legacyOrphanProvenance")
            .is_some();
        let has_cortana_managed_owner = document.pointer("/cortana/owner").is_some();
        let has_cortana_managed_launch = document.pointer("/cortana/managedLaunch").is_some();
        let has_cortana_harness_process = document
            .pointer("/cortana/managedLaunch/harnessProcess")
            .is_some();
        let has_v2_cortana_managed_launch = document
            .pointer("/cortana/managedLaunch/version")
            .and_then(Value::as_u64)
            == Some(2);
        let has_cortana_expected_harness_launch = document
            .pointer("/cortana/managedLaunch/expectedHarnessLaunchProvenance")
            .is_some();
        let has_v3_cortana_managed_launch = document
            .pointer("/cortana/managedLaunch/version")
            .and_then(Value::as_u64)
            == Some(3);
        let has_v4_cortana_managed_launch = document
            .pointer("/cortana/managedLaunch/version")
            .and_then(Value::as_u64)
            == Some(4);
        let has_cortana_active_harness_attestation = document
            .pointer("/cortana/activeHarnessAttestation")
            .is_some();
        let has_cortana_active_harness_attestation_recovery = document
            .pointer("/cortana/activeHarnessAttestationRecovery")
            .is_some();
        let has_cortana_quarantine_ledger = document.pointer("/cortana/quarantineLedger").is_some();
        let has_trusted_harness_child = document
            .pointer(
                "/cortana/managedLaunch/expectedHarnessLaunchProvenance/trustedChildExecutable",
            )
            .is_some();
        let has_cortana_legacy_quarantine = document.pointer("/cortana/legacyQuarantine").is_some()
            || document
                .pointer("/cortana/recovery/kind")
                .and_then(Value::as_str)
                == Some("legacyUnownedQuarantined");
        if has_release_recovery
            && (schema_version < 18 || releases_contain_unknown_fields(&document))
        {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if has_cortana_orphan_recovery && schema_version < 23 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if has_cortana_legacy_provenance && schema_version < 22 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if (has_cortana_managed_owner || has_cortana_legacy_quarantine) && schema_version < 24 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if has_cortana_managed_launch && schema_version < 25 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if (has_cortana_harness_process || has_v2_cortana_managed_launch) && schema_version < 26 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if (has_cortana_expected_harness_launch || has_v3_cortana_managed_launch)
            && schema_version < 27
        {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if (has_trusted_harness_child || has_v4_cortana_managed_launch) && schema_version < 28 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if has_cortana_active_harness_attestation && schema_version < 29 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if has_cortana_active_harness_attestation_recovery && schema_version < 30 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if has_cortana_quarantine_ledger && schema_version < 31 {
            return Err(SnapshotReadError::IncompatibleRecovery {
                path: path.to_path_buf(),
            });
        }
        if !has_cortana_quarantine_ledger {
            if let Some(legacy_quarantine) = document.pointer("/cortana/legacyQuarantine").cloned()
            {
                let cortana = document
                    .get_mut("cortana")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| SnapshotReadError::IncompatibleRecovery {
                        path: path.to_path_buf(),
                    })?;
                cortana.insert(
                    "quarantineLedger".into(),
                    Value::Array(vec![legacy_quarantine]),
                );
                cortana.remove("legacyQuarantine");
            }
        }
        let has_recovery = has_release_recovery
            || has_cortana_orphan_recovery
            || has_cortana_legacy_provenance
            || has_cortana_managed_owner
            || has_cortana_managed_launch
            || has_cortana_active_harness_attestation
            || has_cortana_active_harness_attestation_recovery
            || has_cortana_quarantine_ledger
            || has_cortana_legacy_quarantine;
        let mut snapshot: CaptainsSnapshot = serde_json::from_value(document).map_err(|error| {
            if has_recovery {
                SnapshotReadError::IncompatibleRecovery {
                    path: path.to_path_buf(),
                }
            } else {
                SnapshotReadError::Invalid(format!("'{}' is invalid JSON: {error}", path.display()))
            }
        })?;
        if snapshot.schema_version > CAPTAINS_SCHEMA_VERSION {
            return Err(SnapshotReadError::UnsupportedSchema {
                path: path.to_path_buf(),
                version: snapshot.schema_version,
            });
        }
        migrate_project_identities(&mut snapshot).map_err(|error| {
            if has_recovery {
                SnapshotReadError::IncompatibleRecovery {
                    path: path.to_path_buf(),
                }
            } else {
                SnapshotReadError::Invalid(error)
            }
        })?;
        Self::validate_snapshot(&snapshot).map_err(|error| {
            if has_recovery {
                SnapshotReadError::IncompatibleRecovery {
                    path: path.to_path_buf(),
                }
            } else {
                SnapshotReadError::Invalid(error)
            }
        })?;
        Ok(snapshot)
    }

    pub(super) fn schema18_cortana_provenance(
        source: &CaptainsSnapshot,
        current: &CaptainsSnapshot,
    ) -> Option<crate::cortana_reconcile::CortanaLegacyOrphanProvenance> {
        if source.schema_version != 18
            || source.seq > current.seq
            || source.captains.iter().any(|captain| {
                captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            })
            || current.captains.iter().any(|captain| {
                captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            })
        {
            return None;
        }
        let identity_id = source.cortana.identity_id.as_deref()?.trim();
        let terminal_id = source.cortana.terminal_id.as_deref()?.trim();
        let harness = source.cortana.harness.as_deref()?.trim();
        let crate::cortana_reconcile::CortanaRecoveryState::Healthy { operation_id, .. } =
            &source.cortana.recovery
        else {
            return None;
        };
        if identity_id.is_empty()
            || harness.is_empty()
            || operation_id.trim().is_empty()
            || source.cortana.generation == 0
            || exact_cortana_tmux_target(terminal_id).is_err()
            || current.cortana.identity_id.as_deref() != Some(identity_id)
            || current.cortana.generation != source.cortana.generation
            || current.cortana.harness.as_deref() != Some(harness)
            || current
                .cortana
                .terminal_id
                .as_deref()
                .is_some_and(|current_terminal| current_terminal != terminal_id)
        {
            return None;
        }
        Some(crate::cortana_reconcile::CortanaLegacyOrphanProvenance {
            version: crate::cortana_reconcile::LEGACY_ORPHAN_PROVENANCE_VERSION,
            source_schema_version: 18,
            identity_id: identity_id.to_string(),
            terminal_id: terminal_id.to_string(),
            generation: source.cortana.generation,
            harness: harness.to_string(),
            healthy_operation_id: operation_id.trim().to_string(),
        })
    }

    pub(super) fn recover_schema18_cortana_provenance(
        path: &Path,
        current: &CaptainsSnapshot,
    ) -> Option<crate::cortana_reconcile::CortanaLegacyOrphanProvenance> {
        let mut candidates = Vec::new();
        if let Some(provenance) = Self::schema18_cortana_provenance(current, current) {
            candidates.push(provenance);
        }
        let parent = path.parent()?;
        let file_name = path.file_name()?.to_string_lossy();
        let prefix = format!("{file_name}.migration-v");
        let mut backups = std::fs::read_dir(parent)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                (name.starts_with(&prefix) && name.ends_with(".bak")).then(|| entry.path())
            })
            .collect::<Vec<_>>();
        backups.sort();
        for backup in backups {
            let Ok(source) = Self::read_snapshot(&backup) else {
                continue;
            };
            if let Some(provenance) = Self::schema18_cortana_provenance(&source, current) {
                candidates.push(provenance);
            }
        }
        candidates.sort_by(|left, right| {
            (
                left.terminal_id.as_str(),
                left.identity_id.as_str(),
                left.generation,
                left.harness.as_str(),
                left.healthy_operation_id.as_str(),
            )
                .cmp(&(
                    right.terminal_id.as_str(),
                    right.identity_id.as_str(),
                    right.generation,
                    right.harness.as_str(),
                    right.healthy_operation_id.as_str(),
                ))
        });
        candidates.dedup();
        (candidates.len() == 1).then(|| candidates.remove(0))
    }

    pub(super) fn validate_snapshot(snapshot: &CaptainsSnapshot) -> Result<(), String> {
        let strict_runtime_identity =
            snapshot.schema_version >= STRICT_RUNTIME_IDENTITY_SCHEMA_VERSION;
        if let Some(provenance) = &snapshot.cortana.legacy_orphan_provenance {
            let exact_binding = provenance.version
                == crate::cortana_reconcile::LEGACY_ORPHAN_PROVENANCE_VERSION
                && provenance.source_schema_version == 18
                && !provenance.identity_id.trim().is_empty()
                && !provenance.harness.trim().is_empty()
                && !provenance.healthy_operation_id.trim().is_empty()
                && provenance.generation > 0
                && exact_cortana_tmux_target(&provenance.terminal_id).is_ok()
                && snapshot.cortana.identity_id.as_deref() == Some(provenance.identity_id.as_str())
                && snapshot.cortana.generation == provenance.generation
                && snapshot.cortana.harness.as_deref() == Some(provenance.harness.as_str())
                && snapshot
                    .cortana
                    .terminal_id
                    .as_deref()
                    .is_none_or(|terminal_id| terminal_id == provenance.terminal_id)
                && !matches!(
                    snapshot.cortana.recovery,
                    crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
                )
                && !snapshot.captains.iter().any(|captain| {
                    captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
                });
            if snapshot.schema_version < 22 || !exact_binding {
                return Err(
                    "durable Cortana has invalid schema-v22 legacy orphan provenance".into(),
                );
            }
        }
        let mut project_ids = std::collections::HashSet::new();
        let mut roots = std::collections::HashSet::new();
        for project in &snapshot.projects {
            if project.project_id.trim().is_empty()
                || !project_ids.insert(project.project_id.as_str())
            {
                return Err("captains registry contains an empty or duplicate projectId".into());
            }
            let identity = project.root_path.as_deref().unwrap_or(&project.repo_root);
            if identity.trim().is_empty() || !roots.insert(identity) {
                return Err("captains registry contains an empty or duplicate repoRoot".into());
            }
            if !identity.starts_with('/') || identity.starts_with("//") {
                return Err(format!(
                    "project '{}' has a non-absolute rootPath",
                    project.project_id
                ));
            }
            if !matches!(
                project.vcs_capability.as_deref(),
                Some("git") | Some("none")
            ) {
                return Err(format!(
                    "project '{}' has an invalid vcsCapability",
                    project.project_id
                ));
            }
            if project.vcs_capability.as_deref() == Some("git") && project.git_main_root.is_none() {
                return Err(format!(
                    "project '{}' is Git-enabled but has no gitMainRoot",
                    project.project_id
                ));
            }
            if project.name.trim().is_empty() {
                return Err(format!(
                    "project '{}' has an empty name",
                    project.project_id
                ));
            }
            if let Some(powder) = &project.powder {
                if powder.connection_profile.trim().is_empty()
                    || powder.repository.trim().is_empty()
                {
                    return Err(format!(
                        "project '{}' has an incomplete Powder binding",
                        project.project_id
                    ));
                }
                if powder.event_cursor < 0 {
                    return Err(format!(
                        "project '{}' has a negative Powder event cursor",
                        project.project_id
                    ));
                }
            }
        }
        let mut git_init_operations = std::collections::HashSet::new();
        let mut git_init_roots = std::collections::HashSet::new();
        for intent in &snapshot.pending_git_initializations {
            if intent.version != GIT_INIT_INTENT_VERSION
                || intent.operation_id.trim().is_empty()
                || !git_init_operations.insert(intent.operation_id.as_str())
                || intent.root_path.trim().is_empty()
                || !git_init_roots.insert(intent.root_path.as_str())
                || !intent.root_path.starts_with('/')
                || intent.root_path.starts_with("//")
                || intent.name.trim().is_empty()
                || intent.project_id.trim().is_empty()
                || intent.owner_identity.trim().is_empty()
                || intent.marker_nonce.trim().is_empty()
                || intent.created_at == 0
                || !matches!(
                    intent.phase.as_str(),
                    "intent_written"
                        | "git_initialized"
                        | "cleanup_pending"
                        | "recovery_blocked"
                        | "foreign_git"
                )
            {
                return Err(
                    "captains registry contains an invalid Git initialization intent".into(),
                );
            }
        }
        let mut agent_session_ids = std::collections::HashSet::new();
        for agent in &snapshot.agent_sessions {
            agent.validate()?;
            if !agent_session_ids.insert(agent.agent_session_id.as_str()) {
                return Err(format!(
                    "captains registry contains duplicate agentSessionId '{}'",
                    agent.agent_session_id
                ));
            }
            if !project_ids.contains(agent.project_id.as_str()) {
                return Err(format!(
                    "agent session '{}' references unknown projectId '{}'",
                    agent.agent_session_id, agent.project_id
                ));
            }
        }
        let agent_ids: std::collections::HashSet<_> = snapshot
            .agent_sessions
            .iter()
            .map(|agent| agent.agent_session_id.as_str())
            .collect();
        for checkpoint in &snapshot.agent_checkpoints {
            checkpoint.validate()?;
            if !agent_ids.contains(checkpoint.agent_session_id.as_str()) {
                return Err(format!(
                    "agent checkpoint references unknown agentSessionId '{}'",
                    checkpoint.agent_session_id
                ));
            }
        }
        for event in &snapshot.agent_events {
            if event.cursor == 0 || event.kind.trim().is_empty() {
                return Err("agent event has an invalid cursor or kind".into());
            }
            if !agent_ids.contains(event.agent_session_id.as_str()) {
                return Err(format!(
                    "agent event references unknown agentSessionId '{}'",
                    event.agent_session_id
                ));
            }
            if let Some(checkpoint) = &event.checkpoint {
                checkpoint.validate()?;
            }
        }
        let mut pending_claim_scopes = std::collections::HashSet::new();
        for intent in &snapshot.pending_dispatch_claims {
            if intent.project_id.trim().is_empty()
                || intent.connection_profile.trim().is_empty()
                || intent.repository.trim().is_empty()
                || intent.card_id.trim().is_empty()
                || intent.configured_agent.trim().is_empty()
                || intent.operation_id.trim().is_empty()
            {
                return Err(
                    "captains registry contains an incomplete pending dispatch claim".into(),
                );
            }
            if !pending_claim_scopes.insert((
                intent.connection_profile.as_str(),
                intent.repository.as_str(),
                intent.card_id.as_str(),
            )) {
                return Err(
                    "captains registry contains duplicate pending dispatch claim scope".into(),
                );
            }
        }
        let mut pending_release_crews = std::collections::HashSet::new();
        let mut pending_release_claims = std::collections::HashSet::new();
        for recovery in &snapshot.pending_dispatch_releases {
            if !is_canonical_dispatch_recovery_identity(&recovery.crew_session_id)
                || !is_canonical_dispatch_recovery_identity(&recovery.project_id)
                || !is_canonical_dispatch_recovery_identity(&recovery.connection_profile)
                || !is_canonical_dispatch_recovery_endpoint_identity(
                    &recovery.connection_endpoint_identity,
                )
                || !is_canonical_dispatch_recovery_identity(&recovery.repository)
                || !is_canonical_dispatch_recovery_identity(&recovery.card_id)
                || !is_canonical_dispatch_recovery_identity(&recovery.run_id)
                || !is_canonical_dispatch_recovery_identity(&recovery.agent)
                || !is_canonical_dispatch_recovery_identity(&recovery.operation_id)
            {
                return Err(
                    "captains registry contains an incomplete pending dispatch release".into(),
                );
            }
            if !pending_release_crews.insert(recovery.crew_session_id.as_str())
                || !pending_release_claims.insert((
                    recovery.connection_profile.as_str(),
                    recovery.repository.as_str(),
                    recovery.card_id.as_str(),
                    recovery.run_id.as_str(),
                    recovery.agent.as_str(),
                ))
            {
                return Err(
                    "captains registry contains duplicate pending dispatch release recovery".into(),
                );
            }
        }
        let mut ships = std::collections::HashSet::new();
        let mut terminals = std::collections::HashSet::new();
        let mut assignment_ids = std::collections::HashSet::new();
        let mut workspace_owners = std::collections::HashMap::new();
        let mut cortana_count = 0;
        for captain in &snapshot.captains {
            if captain.ship_slug.trim().is_empty() || !ships.insert(captain.ship_slug.as_str()) {
                return Err("captains registry contains an empty or duplicate shipSlug".into());
            }
            if snapshot.schema_version >= 14 {
                if captain.assignment_id.trim().is_empty()
                    || !assignment_ids.insert(captain.assignment_id.as_str())
                {
                    return Err(
                        "captains registry contains an empty or duplicate assignmentId".into(),
                    );
                }
                normalize_captain_display_name(&captain.display_name)?;
            }
            if captain.role == FleetRole::Cortana {
                cortana_count += 1;
                if cortana_count > 1 {
                    return Err("captains registry contains multiple Cortana records".into());
                }
            }
            if snapshot.schema_version >= STRICT_RUNTIME_IDENTITY_SCHEMA_VERSION
                && ((captain.role == FleetRole::Cortana) != (captain.ship_slug == CORTANA_SLUG))
            {
                return Err(
                    "captains registry must use the reserved Cortana role and slug together".into(),
                );
            }
            validate_runtime_identity(
                &format!("ship '{}'", captain.ship_slug),
                captain.harness.as_deref(),
                captain.provider.as_deref(),
                captain.provider_session_id.as_deref(),
                captain.claude_uuid.as_deref(),
                strict_runtime_identity,
            )?;
            if let Some(terminal) = captain.terminal_id.as_deref() {
                if terminal.trim().is_empty() || !terminals.insert(terminal) {
                    return Err("captains registry assigns one terminal more than once".into());
                }
            }
            match (&captain.state, &captain.terminal_id) {
                (ClaimState::Active, None) => {
                    return Err(format!(
                        "active ship '{}' has no terminalId",
                        captain.ship_slug
                    ));
                }
                (ClaimState::Orphaned { .. } | ClaimState::Vacant, Some(_)) => {
                    return Err(format!(
                        "inactive ship '{}' still has a terminalId",
                        captain.ship_slug
                    ));
                }
                _ => {}
            }
            let mut tabs = std::collections::HashSet::new();
            if captain
                .workspace_tab_ids
                .iter()
                .any(|tab| tab.trim().is_empty() || !tabs.insert(tab.as_str()))
            {
                return Err(format!(
                    "ship '{}' has an empty or duplicate workspace tab",
                    captain.ship_slug
                ));
            }
            for workspace_id in &captain.workspace_tab_ids {
                if workspace_id == CAPTAIN_WORKSPACE_ID {
                    return Err(format!(
                        "ship '{}' cannot own Captain Workspace as a Work Workspace",
                        captain.ship_slug
                    ));
                }
                if let Some(owner) =
                    workspace_owners.insert(workspace_id.as_str(), captain.ship_slug.as_str())
                {
                    return Err(format!(
                        "Work Workspace '{workspace_id}' is already owned by ship '{owner}' and cannot also be owned by ship '{}'",
                        captain.ship_slug
                    ));
                }
            }
            if let Some(project_id) = captain.project_id.as_deref() {
                if !project_ids.contains(project_id) {
                    return Err(format!(
                        "Captain references unknown projectId '{project_id}'"
                    ));
                }
                if captain
                    .assignment
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                    || captain
                        .harness
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(format!(
                        "ship '{}' has a project but no durable assignment or harness",
                        captain.ship_slug
                    ));
                }
            }
            for crew in &captain.crew {
                if crew.terminal_id.trim().is_empty()
                    || !terminals.insert(crew.terminal_id.as_str())
                {
                    return Err("captains registry assigns one terminal more than once".into());
                }
                validate_runtime_identity(
                    &format!("Crew '{}'", crew.terminal_id),
                    crew.harness.as_deref(),
                    crew.provider.as_deref(),
                    crew.provider_session_id.as_deref(),
                    crew.claude_uuid.as_deref(),
                    strict_runtime_identity,
                )?;
                for (field, value) in [
                    ("provider", crew.provider.as_deref()),
                    ("providerSessionId", crew.provider_session_id.as_deref()),
                    ("conversationId", crew.conversation_id.as_deref()),
                    ("resumePoint", crew.resume_point.as_deref()),
                    ("task", crew.task.as_deref()),
                    ("harness", crew.harness.as_deref()),
                    ("worktreePath", crew.worktree_path.as_deref()),
                    ("branch", crew.branch.as_deref()),
                ] {
                    if value.is_some_and(|value| value.trim().is_empty()) {
                        return Err(format!("Crew '{}' has an empty {field}", crew.terminal_id));
                    }
                }
                if let Some(work) = &crew.powder_work {
                    if work.card_id.trim().is_empty() || work.run_id.trim().is_empty() {
                        return Err(format!(
                            "Crew '{}' has an incomplete Powder work binding",
                            crew.terminal_id
                        ));
                    }
                    if work
                        .agent
                        .as_deref()
                        .is_some_and(|agent| agent.trim().is_empty())
                    {
                        return Err(format!(
                            "Crew '{}' has an empty Powder agent identity",
                            crew.terminal_id
                        ));
                    }
                    if let Some(intent) = &work.mutation_intent {
                        validate_powder_mutation_intent(&crew.terminal_id, work, intent)?;
                    }
                    match &work.state {
                        PowderWorkState::Active => {}
                        PowderWorkState::CompletionPending {
                            request_digest,
                            since,
                        } => {
                            validate_completion_marker(
                                &crew.terminal_id,
                                request_digest,
                                *since,
                                "pending",
                            )?;
                        }
                        PowderWorkState::Completed {
                            request_digest,
                            completed_at,
                        } => {
                            validate_completion_marker(
                                &crew.terminal_id,
                                request_digest,
                                *completed_at,
                                "completed",
                            )?;
                        }
                    }
                }
            }
        }
        if snapshot.schema_version >= 18 {
            let durable = &snapshot.cortana;
            for (field, value) in [
                ("identityId", durable.identity_id.as_deref()),
                ("terminalId", durable.terminal_id.as_deref()),
                ("harness", durable.harness.as_deref()),
                ("providerSessionId", durable.provider_session_id.as_deref()),
                ("conversationId", durable.conversation_id.as_deref()),
                ("checkpoint", durable.checkpoint.as_deref()),
            ] {
                if value.is_some_and(|value| value.trim().is_empty()) {
                    return Err(format!("durable Cortana has an empty {field}"));
                }
            }
            if durable.identity_id.is_some() != (durable.generation > 0) {
                return Err(
                    "durable Cortana identity and positive generation must be recorded together"
                        .into(),
                );
            }
            if durable
                .owner
                .as_ref()
                .is_some_and(|owner| !valid_cortana_managed_owner(owner))
                || snapshot.schema_version >= 24
                    && matches!(
                        durable.recovery,
                        crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
                    )
                    && (durable.owner.is_none() || durable.managed_launch.is_some())
            {
                return Err("durable Cortana has an invalid managed owner".into());
            }
            if durable
                .managed_launch
                .as_ref()
                .is_some_and(|launch| !valid_cortana_managed_launch(launch))
            {
                return Err("durable Cortana has an invalid managed launch intent".into());
            }
            if durable
                .active_harness_attestation
                .as_ref()
                .is_some_and(|attestation| {
                    !valid_cortana_active_harness_attestation(durable, attestation)
                })
            {
                return Err("durable Cortana has an invalid active Harness attestation".into());
            }
            if durable
                .active_harness_attestation_recovery
                .as_ref()
                .is_some_and(|recovery| {
                    !valid_cortana_active_harness_attestation_recovery(durable, recovery)
                })
            {
                return Err(
                    "durable Cortana has an invalid active Harness attestation recovery".into(),
                );
            }
            if durable.quarantine_ledger.len() > MAX_CORTANA_QUARANTINE_RECORDS {
                return Err("durable Cortana quarantine ledger exceeds its bound".into());
            }
            for (index, quarantine) in durable.quarantine_ledger.iter().enumerate() {
                if quarantine.terminal_id.trim().is_empty()
                    || quarantine.identity_id.trim().is_empty()
                    || quarantine.generation == 0
                    || quarantine.harness.trim().is_empty()
                    || !quarantine.authority_revoked
                    || quarantine.quarantined_at == 0
                    || !valid_cortana_effect_identity(&quarantine.tmux)
                {
                    return Err("durable Cortana quarantine ledger has invalid evidence".into());
                }
                if durable.quarantine_ledger[..index].iter().any(|prior| {
                    prior.terminal_id == quarantine.terminal_id
                        || prior.identity_id == quarantine.identity_id
                        || prior.tmux == quarantine.tmux
                }) {
                    return Err("durable Cortana quarantine ledger has conflicting evidence".into());
                }
            }
            if let Some(launch) = durable.managed_launch.as_ref() {
                let recovery_operation_id = match &durable.recovery {
                    crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                        operation_id,
                        ..
                    }
                    | crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                        operation_id,
                        ..
                    }
                    | crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                        operation_id,
                        ..
                    }
                    | crate::cortana_reconcile::CortanaRecoveryState::Degraded {
                        operation_id,
                        ..
                    }
                    | crate::cortana_reconcile::CortanaRecoveryState::Healthy {
                        operation_id,
                        ..
                    } => Some(operation_id.as_str()),
                    crate::cortana_reconcile::CortanaRecoveryState::Uninitialized => None,
                };
                if recovery_operation_id != Some(launch.operation_id.as_str()) {
                    return Err(
                        "durable Cortana managed launch and recovery operation disagree".into(),
                    );
                }
                match launch.phase {
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared => {
                        if durable.owner.is_some()
                            && durable.terminal_id.as_deref() == Some(launch.terminal_id.as_str())
                        {
                            return Err(
                                "prepared Cortana launch already publishes an owner binding".into(),
                            );
                        }
                    }
                    crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed => {
                        let owner = durable
                            .owner
                            .as_ref()
                            .ok_or("owner-observed Cortana launch has no durable managed owner")?;
                        if durable.terminal_id.as_deref() != Some(launch.terminal_id.as_str())
                            || owner.unit_name != launch.unit_name
                            || owner.launch_nonce != launch.launch_nonce
                        {
                            return Err("observed Cortana launch and owner binding disagree".into());
                        }
                        if let Some(process) = launch.harness_process.as_ref() {
                            if process.provider != launch.harness
                                || process.tmux_session_id != owner.tmux.tmux_session_id
                                || process.tmux_session_created != owner.tmux.tmux_session_created
                                || process.tmux_window_id != owner.tmux.tmux_window_id
                                || process.tmux_pane_id != owner.tmux.tmux_pane_id
                                || process.pane_pid != owner.tmux.pane_pid
                                || process.pane_start_ticks != owner.tmux.pane_start_ticks
                                || process.cgroup_path != owner.cgroup_path
                            {
                                return Err(
                                    "observed Cortana Harness process and owner disagree".into()
                                );
                            }
                        }
                    }
                }
            }
            match &durable.recovery {
                crate::cortana_reconcile::CortanaRecoveryState::Uninitialized => {}
                crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                    operation_id,
                    started_at,
                } => {
                    if operation_id.trim().is_empty() || *started_at == 0 {
                        return Err("durable Cortana has an invalid recovery operation".into());
                    }
                }
                crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                    operation_id,
                    started_at,
                    orphan_terminal_id,
                    orphan_identity_id,
                    orphan_generation,
                    harness,
                    effect_identity,
                    managed_basis,
                    replacement_identity_id,
                } => {
                    let managed_basis_valid = managed_basis.as_ref().is_none_or(|basis| {
                        basis.version == crate::cortana_reconcile::MANAGED_QUARANTINE_BASIS_VERSION
                            && basis.claim_ship_slug == CORTANA_SLUG
                            && !basis.claim_assignment_id.trim().is_empty()
                            && basis.claim_terminal_id == *orphan_terminal_id
                            && basis.claim_harness == *harness
                            && same_cortana_tmux_generation(&basis.owner.tmux, effect_identity)
                            && valid_cortana_managed_owner(&basis.owner)
                            && basis.active_harness_attestation
                                == durable.active_harness_attestation
                            && basis.replacement_generation == orphan_generation.saturating_add(1)
                            && basis.prior_ledger_count == durable.quarantine_ledger.len()
                            && basis.prior_ledger_sha256
                                == cortana_quarantine_ledger_sha256(&durable.quarantine_ledger)
                            && durable.owner.as_ref() == Some(&basis.owner)
                            && snapshot
                                .captains
                                .iter()
                                .filter(|captain| {
                                    captain.role == FleetRole::Cortana
                                        && captain.state == ClaimState::Active
                                })
                                .count()
                                == 1
                            && snapshot.captains.iter().any(|captain| {
                                captain.role == FleetRole::Cortana
                                    && captain.state == ClaimState::Active
                                    && captain.ship_slug == basis.claim_ship_slug
                                    && captain.assignment_id == basis.claim_assignment_id
                                    && captain.terminal_id.as_deref()
                                        == Some(basis.claim_terminal_id.as_str())
                                    && captain.harness.as_deref()
                                        == Some(basis.claim_harness.as_str())
                            })
                            && basis.workspace_ids
                                == snapshot
                                    .workspaces
                                    .iter()
                                    .filter(|workspace| {
                                        workspace
                                            .tile_ids
                                            .iter()
                                            .any(|tile| tile == &basis.claim_terminal_id)
                                    })
                                    .map(|workspace| workspace.id.clone())
                                    .collect::<Vec<_>>()
                    });
                    if operation_id.trim().is_empty()
                        || *started_at == 0
                        || orphan_terminal_id.trim().is_empty()
                        || orphan_identity_id.trim().is_empty()
                        || *orphan_generation == 0
                        || harness.trim().is_empty()
                        || replacement_identity_id
                            .as_deref()
                            .is_some_and(|identity_id| identity_id.trim().is_empty())
                        || durable.terminal_id.as_deref() != Some(orphan_terminal_id.as_str())
                        || durable.identity_id.as_deref() != Some(orphan_identity_id.as_str())
                        || durable.generation != *orphan_generation
                        || durable.harness.as_deref() != Some(harness.as_str())
                        || snapshot.schema_version < 23
                        || !valid_cortana_effect_identity(effect_identity)
                        || !managed_basis_valid
                        || snapshot.schema_version >= 31
                            && durable.owner.is_some()
                            && managed_basis.is_none()
                    {
                        return Err(
                            "durable Cortana has an invalid orphan replacement operation".into(),
                        );
                    }
                }
                crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                    operation_id,
                    quarantined_at,
                    legacy_terminal_id,
                    legacy_generation,
                    replacement_identity_id,
                } => {
                    let quarantine = durable.quarantine_ledger.iter().find(|quarantine| {
                        quarantine.terminal_id == *legacy_terminal_id
                            && quarantine.generation == *legacy_generation
                            && quarantine.quarantined_at == *quarantined_at
                    });
                    if operation_id.trim().is_empty()
                        || *quarantined_at == 0
                        || legacy_terminal_id.trim().is_empty()
                        || *legacy_generation == 0
                        || replacement_identity_id
                            .as_deref()
                            .is_some_and(|identity_id| identity_id.trim().is_empty())
                        || quarantine.is_none_or(|quarantine| {
                            quarantine.terminal_id != *legacy_terminal_id
                                || quarantine.generation != *legacy_generation
                                || quarantine.identity_id.trim().is_empty()
                                || quarantine.harness.trim().is_empty()
                                || !quarantine.authority_revoked
                                || quarantine.quarantined_at != *quarantined_at
                                || !valid_cortana_effect_identity(&quarantine.tmux)
                        })
                    {
                        return Err("durable Cortana has an invalid legacy quarantine".into());
                    }
                }
                crate::cortana_reconcile::CortanaRecoveryState::Healthy {
                    operation_id,
                    verified_at,
                } => {
                    if operation_id.trim().is_empty()
                        || *verified_at == 0
                        || durable.identity_id.is_none()
                        || durable.terminal_id.is_none()
                        || snapshot.schema_version >= 29
                            && durable.active_harness_attestation.is_none()
                        || durable.active_harness_attestation_recovery.is_some()
                    {
                        return Err("durable Cortana has an incomplete healthy state".into());
                    }
                }
                crate::cortana_reconcile::CortanaRecoveryState::Degraded {
                    operation_id,
                    reason,
                    detected_at,
                } => {
                    if operation_id.trim().is_empty()
                        || reason.trim().is_empty()
                        || *detected_at == 0
                    {
                        return Err("durable Cortana has an invalid degraded state".into());
                    }
                }
            }
        }
        if snapshot.schema_version >= 15 {
            if snapshot.pending_fleet_operations.len() > MAX_PENDING_FLEET_OPERATIONS {
                return Err("captains registry contains too many pending Fleet operations".into());
            }
            let mut operation_ids = std::collections::HashSet::new();
            for operation in &snapshot.pending_fleet_operations {
                if operation.operation_id.trim().is_empty()
                    || !operation_ids.insert(operation.operation_id.as_str())
                    || operation.created_at == 0
                {
                    return Err(
                        "captains registry contains an invalid pending Fleet operation".into(),
                    );
                }
            }
            let mut workspace_ids = std::collections::HashSet::new();
            let mut placed_tiles = std::collections::HashSet::new();
            let mut captain_workspace_count = 0;
            for workspace in &snapshot.workspaces {
                if workspace.id.trim().is_empty()
                    || workspace.name.trim().is_empty()
                    || !workspace_ids.insert(workspace.id.as_str())
                {
                    return Err("captains registry contains an empty or duplicate Workspace".into());
                }
                if workspace.kind == WorkspaceKind::Captain {
                    captain_workspace_count += 1;
                    if workspace.id != CAPTAIN_WORKSPACE_ID
                        || workspace.name != CAPTAIN_WORKSPACE_NAME
                        || workspace.owner.is_some()
                    {
                        return Err(
                            "captains registry contains a non-canonical Captain Workspace".into(),
                        );
                    }
                } else {
                    if let Some(owner) = &workspace.owner {
                        if !snapshot.captains.iter().any(|captain| {
                            captain.ship_slug == owner.ship_slug
                                && captain.assignment_id == owner.assignment_id
                                && captain.project_id.as_deref() == Some(owner.project_id.as_str())
                                && captain.workspace_tab_ids.contains(&workspace.id)
                        }) {
                            return Err(format!(
                                "Work Workspace '{}' owner does not match one Captain Assignment",
                                workspace.id
                            ));
                        }
                    }
                }
                for tile in &workspace.tile_ids {
                    if tile.trim().is_empty() || !placed_tiles.insert(tile.as_str()) {
                        return Err(format!(
                            "Fleet Workspace state places terminal '{tile}' more than once"
                        ));
                    }
                }
            }
            if captain_workspace_count != 1 {
                return Err("captains registry must contain exactly one Captain Workspace".into());
            }
        }
        if snapshot.schema_version >= 16 {
            if snapshot.retired_fleet_tile_ids.len() > MAX_RETIRED_FLEET_TILES {
                return Err("captains registry contains too many retired Fleet terminals".into());
            }
            let mut retired = std::collections::HashSet::new();
            for terminal_id in &snapshot.retired_fleet_tile_ids {
                if terminal_id.trim().is_empty() || !retired.insert(terminal_id.as_str()) {
                    return Err(
                        "captains registry contains an invalid retired Fleet terminal".into(),
                    );
                }
                if snapshot
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.tile_ids.contains(terminal_id))
                {
                    return Err(format!(
                        "retired Fleet terminal '{terminal_id}' remains placed in a Workspace"
                    ));
                }
            }
        }
        if snapshot.schema_version < 18 && !snapshot.pending_dispatch_releases.is_empty() {
            return Err(
                "captains registry release recovery requires schema version 18 or newer".into(),
            );
        }
        for recovery in &snapshot.pending_dispatch_releases {
            let matches = snapshot
                .captains
                .iter()
                .filter(|captain| {
                    captain.project_id.as_deref() == Some(recovery.project_id.as_str())
                })
                .flat_map(|captain| {
                    captain
                        .crew
                        .iter()
                        .filter(move |crew| crew.terminal_id == recovery.crew_session_id)
                })
                .filter(|crew| {
                    matches!(crew.state, CrewState::CleanupPending { .. })
                        && crew.powder_work.as_ref().is_some_and(|work| {
                            work.card_id == recovery.card_id
                                && work.run_id == recovery.run_id
                                && work.agent.as_deref() == Some(recovery.agent.as_str())
                                && work.dispatch_release_recovery
                        })
                })
                .count();
            if matches != 1 {
                return Err(format!(
                    "pending dispatch release for Crew '{}' lacks exactly one matching CleanupPending Crew binding",
                    recovery.crew_session_id
                ));
            }
        }
        for captain in &snapshot.captains {
            for crew in &captain.crew {
                let Some(work) = &crew.powder_work else {
                    continue;
                };
                if !work.dispatch_release_recovery {
                    continue;
                }
                let matches = snapshot
                    .pending_dispatch_releases
                    .iter()
                    .filter(|recovery| {
                        captain.project_id.as_deref() == Some(recovery.project_id.as_str())
                            && recovery.crew_session_id == crew.terminal_id
                            && recovery.card_id == work.card_id
                            && recovery.run_id == work.run_id
                            && work.agent.as_deref() == Some(recovery.agent.as_str())
                    })
                    .count();
                if !matches!(crew.state, CrewState::CleanupPending { .. }) || matches != 1 {
                    return Err(format!(
                        "Crew '{}' has an orphaned frozen-scope dispatch release recovery",
                        crew.terminal_id
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, CaptainsInner> {
        // Same poisoned-lock policy as TabRegistry: the data is a plain Vec, so
        // recovering the guard and continuing is safe.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(super) fn provision_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        // Lock order contract: callers that also need dispatch admission must
        // acquire `ControlContext::dispatch_admission` first. Reconciliation may
        // inspect under this lock alone, but it must release and retry in global
        // order before creating a replacement runtime.
        self.provision.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Roll back only the claim created by a failed provisioning operation. The
    /// exact-record comparison prevents this compensation from overwriting a
    /// concurrent refresh, while unrelated registry mutations are preserved.
    pub(super) fn rollback_provisioned_claim(
        &self,
        terminal_id: &str,
        current_claim: &CaptainRecord,
        previous_claim: Option<CaptainRecord>,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let position = current
            .captains
            .iter()
            .position(|captain| captain.terminal_id.as_deref() == Some(terminal_id))
            .ok_or("provision rollback refused: claimed terminal is no longer registered")?;
        if &current.captains[position] != current_claim {
            return Err("provision rollback refused: claimed Captain changed concurrently".into());
        }
        current.captains.remove(position);
        if let Some(previous_claim) = previous_claim {
            current.captains.push(previous_claim);
        }
        current.seq = previous.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    /// Snapshot the registry for persistence - a cheap clone taken under the
    /// caller's already-held `inner` lock. The (potentially slow) disk write then
    /// happens in [`persist`](Self::persist) AFTER the lock is dropped.
    pub(super) fn snapshot_for_persist(g: &CaptainsInner) -> CaptainsSnapshot {
        CaptainsSnapshot {
            schema_version: CAPTAINS_SCHEMA_VERSION,
            seq: g.seq,
            captains: g.captains.clone(),
            cortana: g.cortana.clone(),
            agent_sessions: g.agent_sessions.clone(),
            agent_checkpoints: g.agent_checkpoints.clone(),
            agent_events: g.agent_events.clone(),
            projects: g.projects.clone(),
            workspaces: g.workspaces.clone(),
            pending_fleet_operations: g.pending_fleet_operations.clone(),
            retired_fleet_tile_ids: g.retired_fleet_tile_ids.clone(),
            pending_dispatch_claims: g.pending_dispatch_claims.clone(),
            pending_dispatch_releases: g.pending_dispatch_releases.clone(),
            pending_git_initializations: g.pending_git_initializations.clone(),
        }
    }

    /// Load-time reconciliation of the Cortana singleton (item-2 D2/MED-6). A legacy
    /// `ship_slug == "cortana"` captain claim (the pre-item-2 slug hack) is the LIVE
    /// apex incumbent, so seed the first-class `role: Cortana` FROM it rather than
    /// defaulting it to `Captain` (which would leave the singleton with zero holders).
    /// Idempotent: a v1 record that is already `Cortana` stays so. Defensive against a
    /// corrupt file with two exact-`cortana` slugs (prior uniqueness prevented it):
    /// keep the first as the Active singleton and orphan the rest, so the "one Active
    /// Cortana" invariant holds and an operator resolves the duplicate.
    pub(super) fn reconcile_on_load(caps: &mut [FleetIdentity]) {
        let mut seen_cortana = false;
        for c in caps.iter_mut() {
            if c.assignment_id.trim().is_empty() {
                c.assignment_id = assignment_id_for(c.project_id.as_deref(), &c.ship_slug);
            }
            if c.display_name.trim().is_empty() {
                c.display_name = c.ship_slug.clone();
            }
            c.workspace_tab_ids.retain(|id| id != CAPTAIN_WORKSPACE_ID);
            let mut seen_workspaces = std::collections::HashSet::new();
            c.workspace_tab_ids
                .retain(|id| seen_workspaces.insert(id.clone()));
            reconcile_legacy_runtime_identity(
                &mut c.harness,
                &mut c.provider,
                &mut c.provider_session_id,
                &mut c.claude_uuid,
            );
            for crew in &mut c.crew {
                reconcile_legacy_runtime_identity(
                    &mut crew.harness,
                    &mut crew.provider,
                    &mut crew.provider_session_id,
                    &mut crew.claude_uuid,
                );
            }
            if c.ship_slug == CORTANA_SLUG {
                c.role = FleetRole::Cortana;
                if seen_cortana {
                    c.state = ClaimState::Orphaned { since: now_ms() };
                    c.terminal_id = None;
                } else {
                    seen_cortana = true;
                }
            }
        }
    }

    pub(super) fn reconcile_cortana_on_load(
        captains: &[FleetIdentity],
        durable: &mut crate::cortana_reconcile::CortanaDurableIdentity,
    ) {
        if matches!(
            durable.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Healthy { .. }
        ) && durable.active_harness_attestation.is_none()
        {
            durable.recovery = crate::cortana_reconcile::CortanaRecoveryState::Degraded {
                operation_id: "startup-active-attestation-migration".into(),
                reason: "legacy Healthy Cortana has no active Harness attestation".into(),
                detected_at: now_ms().max(1),
            };
        }
        let incumbent = captains
            .iter()
            .find(|captain| captain.role == FleetRole::Cortana);
        if let Some(incumbent) = incumbent {
            if durable.terminal_id.is_none() {
                durable.terminal_id = incumbent.terminal_id.clone();
            }
            if durable.harness.is_none() {
                durable.harness = incumbent
                    .provider
                    .clone()
                    .or_else(|| incumbent.harness.clone());
            }
            if durable.provider_session_id.is_none() {
                durable.provider_session_id = incumbent.provider_session_id.clone();
            }
            if durable.conversation_id.is_none() {
                durable.conversation_id = incumbent.conversation_id.clone();
            }
            if durable.checkpoint.is_none() {
                durable.checkpoint = incumbent.resume_point.clone();
            }
        } else if durable.terminal_id.is_some()
            && durable.managed_launch.is_none()
            && !matches!(
                durable.recovery,
                crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. }
            )
        {
            durable.recovery = crate::cortana_reconcile::CortanaRecoveryState::Degraded {
                operation_id: "startup-load".into(),
                reason: "durable Cortana points to a runtime with no Fleet registry claim".into(),
                detected_at: now_ms().max(1),
            };
        }
    }

    pub(super) fn reconcile_durable_workspaces(
        captains: &[CaptainRecord],
        mut workspaces: Vec<FleetWorkspaceRecord>,
    ) -> Vec<FleetWorkspaceRecord> {
        workspaces.retain(|workspace| workspace.id != CAPTAIN_WORKSPACE_ID);
        for workspace in &mut workspaces {
            if workspace.owner.is_none() {
                if let Some(captain) = captains.iter().find(|captain| {
                    captain.project_id.is_some()
                        && captain.workspace_tab_ids.contains(&workspace.id)
                }) {
                    workspace.owner = Some(FleetWorkspaceOwner {
                        project_id: captain.project_id.clone().unwrap(),
                        assignment_id: captain.assignment_id.clone(),
                        ship_slug: captain.ship_slug.clone(),
                    });
                }
            }
        }
        workspaces.retain(|workspace| {
            workspace.kind == WorkspaceKind::Work
                && workspace.owner.as_ref().is_none_or(|owner| {
                    captains.iter().any(|captain| {
                        captain.ship_slug == owner.ship_slug
                            && captain.assignment_id == owner.assignment_id
                            && captain.project_id.as_deref() == Some(owner.project_id.as_str())
                            && captain.workspace_tab_ids.contains(&workspace.id)
                    })
                })
        });
        let known_fleet_tiles = captains
            .iter()
            .flat_map(|captain| {
                captain
                    .terminal_id
                    .iter()
                    .chain(captain.crew.iter().map(|crew| &crew.terminal_id))
            })
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        for workspace in &mut workspaces {
            workspace
                .tile_ids
                .retain(|tile| !known_fleet_tiles.contains(tile));
        }
        let supervisors = captains
            .iter()
            .filter(|captain| captain.state == ClaimState::Active)
            .filter_map(|captain| captain.terminal_id.clone())
            .collect::<Vec<_>>();
        workspaces.push(FleetWorkspaceRecord {
            tile_ids: supervisors,
            ..FleetWorkspaceRecord::captain_workspace()
        });
        for captain in captains {
            let Some(project_id) = captain.project_id.as_ref() else {
                continue;
            };
            for workspace_id in &captain.workspace_tab_ids {
                if let Some(workspace) = workspaces
                    .iter_mut()
                    .find(|workspace| workspace.id == *workspace_id)
                {
                    if workspace.owner.is_none() {
                        workspace.owner = Some(FleetWorkspaceOwner {
                            project_id: project_id.clone(),
                            assignment_id: captain.assignment_id.clone(),
                            ship_slug: captain.ship_slug.clone(),
                        });
                    }
                    continue;
                }
                workspaces.push(FleetWorkspaceRecord {
                    id: workspace_id.clone(),
                    name: workspace_id.clone(),
                    kind: WorkspaceKind::Work,
                    owner: Some(FleetWorkspaceOwner {
                        project_id: project_id.clone(),
                        assignment_id: captain.assignment_id.clone(),
                        ship_slug: captain.ship_slug.clone(),
                    }),
                    tile_ids: Vec::new(),
                });
            }
        }
        for captain in captains {
            for crew in &captain.crew {
                if matches!(crew.state, CrewState::Removed { .. }) {
                    continue;
                }
                let Some(workspace_id) = crew.workspace_tab_id.as_ref() else {
                    continue;
                };
                if let Some(workspace) = workspaces
                    .iter_mut()
                    .find(|workspace| workspace.id == *workspace_id)
                {
                    workspace.tile_ids.push(crew.terminal_id.clone());
                }
            }
        }
        workspaces
    }

    pub fn workspace_projection(&self) -> Vec<TabRecord> {
        let current = self.lock();
        let excluded = self
            .workspace_projection_exclusions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        current
            .workspaces
            .iter()
            .map(|workspace| {
                let mut projected = workspace.as_tab_record();
                projected
                    .tile_ids
                    .retain(|tile| !excluded.contains_key(tile));
                projected
            })
            .collect()
    }

    fn apply_gone_workspace_tiles_at(
        current: &mut CaptainsInner,
        gone: &std::collections::HashSet<String>,
        since: u64,
    ) {
        for workspace in &mut current.workspaces {
            workspace.tile_ids.retain(|tile| !gone.contains(tile));
        }
        for captain in &mut current.captains {
            if captain
                .terminal_id
                .as_ref()
                .is_some_and(|terminal| gone.contains(terminal))
            {
                captain.state = ClaimState::Orphaned { since };
                captain.terminal_id = None;
                for crew in &mut captain.crew {
                    if matches!(crew.state, CrewState::Active) {
                        crew.state = CrewState::Orphaned { since };
                    }
                }
            }
            for crew in &mut captain.crew {
                if !gone.contains(&crew.terminal_id) {
                    continue;
                }
                crew.workspace_tab_id = None;
                if !matches!(
                    crew.state,
                    CrewState::CleanupPending { .. } | CrewState::Removed { .. }
                ) {
                    crew.state = CrewState::Removed { since };
                }
            }
        }
    }

    fn apply_workspace_projection_exclusions(
        current: &mut CaptainsInner,
        exclusions: &std::collections::HashMap<String, u64>,
    ) {
        let mut ordered = exclusions.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| left.1.cmp(right.1).then_with(|| left.0.cmp(right.0)));
        for (terminal_id, since) in ordered {
            Self::apply_gone_workspace_tiles_at(
                current,
                &std::collections::HashSet::from([terminal_id.clone()]),
                *since,
            );
        }
    }

    fn clear_workspace_projection_exclusion(&self, terminal_id: &str) {
        self.workspace_projection_exclusions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(terminal_id);
    }

    /// Reconcile every persisted terminal placement that is definitively gone
    /// before the workspace projection becomes authoritative for a new app run.
    /// Durable Captain and Crew history is retained, including frozen cleanup
    /// recovery. If persistence is unavailable, an in-memory exclusion keeps the
    /// initial and all later authoritative reads live-only until a successful
    /// mutation durably incorporates the same cleanup.
    pub fn prune_gone_workspace_tiles(
        &self,
        is_live: impl Fn(&str) -> bool,
    ) -> Result<Vec<String>, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let gone = current
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tile_ids.iter())
            .filter(|tile| !is_live(tile))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if gone.is_empty() {
            return Ok(Vec::new());
        }
        let gone = gone.into_iter().collect::<std::collections::HashSet<_>>();
        let since = now_ms();
        self.workspace_projection_exclusions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(gone.iter().cloned().map(|terminal_id| (terminal_id, since)));
        Self::apply_gone_workspace_tiles_at(&mut current, &gone, since);
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        let mut pruned = gone.into_iter().collect::<Vec<_>>();
        pruned.sort();
        Ok(pruned)
    }

    pub(super) fn create_workspace(
        &self,
        id: &str,
        name: &str,
        owner: Option<&FleetWorkspaceOwner>,
    ) -> Result<FleetWorkspaceRecord, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
        {
            return Err(format!("Workspace '{id}' already exists"));
        }
        let previous = current.clone();
        if let Some(owner) = owner {
            let captain = current
                .captains
                .iter_mut()
                .find(|captain| {
                    captain.ship_slug == owner.ship_slug
                        && captain.assignment_id == owner.assignment_id
                        && captain.project_id.as_deref() == Some(owner.project_id.as_str())
                })
                .ok_or("Workspace owner no longer resolves to one Captain Assignment")?;
            captain.workspace_tab_ids.push(id.to_string());
        }
        let workspace = FleetWorkspaceRecord {
            id: id.to_string(),
            name: name.to_string(),
            kind: WorkspaceKind::Work,
            owner: owner.cloned(),
            tile_ids: Vec::new(),
        };
        current.workspaces.push(workspace.clone());
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(workspace)
    }

    pub(super) fn adopt_unowned_workspace_projection(
        &self,
        tabs: &[TabRecord],
    ) -> Result<bool, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let mut changed = false;
        for tab in tabs {
            if tab.kind() != WorkspaceKind::Work
                || current
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == tab.id)
            {
                continue;
            }
            current.workspaces.push(FleetWorkspaceRecord {
                id: tab.id.clone(),
                name: tab.name.clone(),
                kind: WorkspaceKind::Work,
                owner: None,
                tile_ids: tab.tile_ids.clone(),
            });
            changed = true;
        }
        if !changed {
            return Ok(false);
        }
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(true)
    }

    pub(super) fn rename_workspace(&self, id: &str, name: &str) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let workspace = current
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .ok_or_else(|| format!("rename_tab: unknown durable Workspace '{id}'"))?;
        if workspace.kind == WorkspaceKind::Captain {
            return Err("rename_tab: Captain Workspace cannot be renamed".into());
        }
        if workspace.name == name {
            return Ok(());
        }
        workspace.name = name.to_string();
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    pub(super) fn move_workspace_tile(
        &self,
        tile_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current
            .retired_fleet_tile_ids
            .iter()
            .any(|retired| retired == tile_id)
        {
            return Err(format!("retired terminal '{tile_id}' cannot be moved"));
        }
        if !current
            .workspaces
            .iter()
            .any(|workspace| workspace.id == workspace_id)
        {
            return Err(format!(
                "move_tile: unknown durable Workspace '{workspace_id}'"
            ));
        }
        if current.captains.iter().any(|captain| {
            captain.terminal_id.as_deref() == Some(tile_id) && workspace_id != CAPTAIN_WORKSPACE_ID
        }) {
            return Err(format!(
                "Captain terminal '{tile_id}' belongs to Captain Workspace"
            ));
        }
        if current.captains.iter().any(|captain| {
            captain.crew.iter().any(|crew| {
                crew.terminal_id == tile_id && matches!(crew.state, CrewState::Removed { .. })
            })
        }) {
            return Err(format!("removed Crew terminal '{tile_id}' cannot be moved"));
        }
        let previous = current.clone();
        for workspace in &mut current.workspaces {
            workspace.tile_ids.retain(|tile| tile != tile_id);
        }
        current
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .expect("target checked under the same lock")
            .tile_ids
            .push(tile_id.to_string());
        for captain in &mut current.captains {
            if let Some(crew) = captain
                .crew
                .iter_mut()
                .find(|crew| crew.terminal_id == tile_id)
            {
                crew.workspace_tab_id =
                    (workspace_id != CAPTAIN_WORKSPACE_ID).then(|| workspace_id.to_string());
                if crew.workspace_tab_id.is_some()
                    && matches!(crew.state, CrewState::NeedsAssignment { .. })
                {
                    crew.state = CrewState::Active;
                }
            }
        }
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    pub(super) fn prepare_close_terminal_operation(
        &self,
        terminal_id: &str,
        expected_seq: u64,
        frozen_powder_release: Option<PendingDispatchRelease>,
    ) -> Result<PendingFleetOperation, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if let Some(existing) = current.pending_fleet_operations.iter().find(|operation| {
            matches!(
                &operation.payload,
                PendingFleetOperationPayload::CloseTerminal {
                    terminal_id: pending_terminal,
                    ..
                } if pending_terminal == terminal_id
            )
        }) {
            return Ok(existing.clone());
        }
        if current.seq != expected_seq {
            return Err(format!(
                "terminal close authority changed after scope capture (expected seq {expected_seq}, current seq {})",
                current.seq
            ));
        }
        let previous = current.clone();
        let powder_release = match frozen_powder_release {
            Some(recovery) => {
                if recovery.crew_session_id != terminal_id
                    || recovery.state != PendingDispatchReleaseState::Prepared
                {
                    return Err(
                        "terminal close Powder recovery does not match the prepared terminal"
                            .into(),
                    );
                }
                if let Some(existing) = current
                    .pending_dispatch_releases
                    .iter()
                    .find(|existing| existing.crew_session_id == terminal_id)
                {
                    if existing != &recovery {
                        return Err(format!(
                            "Crew session '{terminal_id}' already has a different frozen Powder cleanup scope"
                        ));
                    }
                    Some(existing.clone())
                } else {
                    let crew = current
                        .captains
                        .iter_mut()
                        .filter(|captain| {
                            captain.project_id.as_deref() == Some(recovery.project_id.as_str())
                        })
                        .flat_map(|captain| captain.crew.iter_mut())
                        .find(|crew| crew.terminal_id == terminal_id)
                        .ok_or_else(|| {
                            format!(
                                "terminal close refused because Crew session '{terminal_id}' is no longer in the frozen Project"
                            )
                        })?;
                    let work = crew.powder_work.as_mut().ok_or_else(|| {
                        format!(
                            "terminal close refused because Crew session '{terminal_id}' lost its Powder binding"
                        )
                    })?;
                    if work.card_id != recovery.card_id
                        || work.run_id != recovery.run_id
                        || work.agent.as_deref() != Some(recovery.agent.as_str())
                    {
                        return Err(format!(
                            "terminal close refused because Crew session '{terminal_id}' no longer owns the frozen Powder claim"
                        ));
                    }
                    crew.state = CrewState::CleanupPending { since: now_ms() };
                    work.dispatch_release_recovery = true;
                    current.pending_dispatch_releases.push(recovery.clone());
                    Some(recovery)
                }
            }
            None => current
                .pending_dispatch_releases
                .iter()
                .find(|release| release.crew_session_id == terminal_id)
                .cloned(),
        };
        let operation = PendingFleetOperation {
            operation_id: format!("close-terminal:{}", uuid::Uuid::new_v4().simple()),
            expected_seq: current.seq,
            phase: PendingFleetOperationPhase::Prepared,
            created_at: now_ms(),
            payload: PendingFleetOperationPayload::CloseTerminal {
                terminal_id: terminal_id.to_string(),
                powder_release,
            },
        };
        current.pending_fleet_operations.push(operation.clone());
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(operation)
    }

    pub(super) fn pending_close_terminal_operation(
        &self,
        terminal_id: &str,
    ) -> Option<PendingFleetOperation> {
        self.lock()
            .pending_fleet_operations
            .iter()
            .find(|operation| {
                matches!(
                    &operation.payload,
                    PendingFleetOperationPayload::CloseTerminal {
                        terminal_id: pending_terminal,
                        ..
                    } if pending_terminal == terminal_id
                )
            })
            .cloned()
    }

    #[allow(dead_code)]
    pub(super) fn close_operation_owns_dispatch_release(
        &self,
        recovery: &PendingDispatchRelease,
    ) -> bool {
        self.lock()
            .pending_fleet_operations
            .iter()
            .any(|operation| {
                let PendingFleetOperationPayload::CloseTerminal {
                    terminal_id,
                    powder_release: Some(owned),
                } = &operation.payload
                else {
                    return false;
                };
                let mut expected = recovery.clone();
                expected.state = owned.state;
                terminal_id == &recovery.crew_session_id && owned == &expected
            })
    }

    pub(super) fn prepare_commission_operation(
        &self,
        terminal_id: &str,
        project_id: &str,
        assignment: &str,
        ship_slug: &str,
        harness: &str,
    ) -> Result<PendingFleetOperation, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current.pending_fleet_operations.iter().any(|operation| {
            matches!(
                operation.payload,
                PendingFleetOperationPayload::CommissionCaptain { .. }
            )
        }) {
            return Err(
                "commission_captain: another Captain commission is pending recovery".into(),
            );
        }
        let previous = current.clone();
        let operation = PendingFleetOperation {
            operation_id: format!("commission-captain:{}", uuid::Uuid::new_v4().simple()),
            expected_seq: current.seq,
            phase: PendingFleetOperationPhase::Prepared,
            created_at: now_ms(),
            payload: PendingFleetOperationPayload::CommissionCaptain {
                terminal_id: terminal_id.to_string(),
                project_id: project_id.to_string(),
                assignment: assignment.to_string(),
                ship_slug: ship_slug.to_string(),
                harness: harness.to_string(),
                identity_id: None,
            },
        };
        current.pending_fleet_operations.push(operation.clone());
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(operation)
    }

    pub(super) fn bind_commission_operation_identity(
        &self,
        operation_id: &str,
        identity_id: &str,
    ) -> Result<PendingFleetOperation, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let operation = current
            .pending_fleet_operations
            .iter_mut()
            .find(|operation| operation.operation_id == operation_id)
            .ok_or("commission_captain: durable commission intent disappeared")?;
        let PendingFleetOperationPayload::CommissionCaptain {
            identity_id: pending_identity,
            ..
        } = &mut operation.payload
        else {
            return Err("commission_captain: durable intent has the wrong kind".into());
        };
        if pending_identity.is_some() {
            return Err("commission_captain: durable identity reservation already exists".into());
        }
        *pending_identity = Some(identity_id.to_string());
        let result = operation.clone();
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn abort_close_terminal_operation(&self, operation_id: &str) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let before = current.pending_fleet_operations.len();
        current
            .pending_fleet_operations
            .retain(|operation| operation.operation_id != operation_id);
        if current.pending_fleet_operations.len() == before {
            return Ok(());
        }
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    pub(super) fn commit_close_terminal_operation(
        &self,
        operation: &PendingFleetOperation,
        retain_crew_binding: bool,
        released_recovery: Option<&PendingDispatchRelease>,
    ) -> Result<CloseTerminalCommitResult, String> {
        let terminal_id = match &operation.payload {
            PendingFleetOperationPayload::CloseTerminal { terminal_id, .. } => terminal_id,
            _ => return Err("Fleet operation is not a terminal close".into()),
        };
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let Some(pending) = current
            .pending_fleet_operations
            .iter()
            .find(|pending| pending.operation_id == operation.operation_id)
        else {
            return Err(format!(
                "terminal close operation '{}' is no longer pending",
                operation.operation_id
            ));
        };
        if pending != operation || pending.phase != PendingFleetOperationPhase::Prepared {
            return Err(format!(
                "terminal close operation '{}' changed before commit",
                operation.operation_id
            ));
        }
        let operation_recovery = match &operation.payload {
            PendingFleetOperationPayload::CloseTerminal { powder_release, .. } => {
                powder_release.as_ref()
            }
            _ => None,
        };
        if released_recovery.is_some() && released_recovery != operation_recovery {
            return Err(format!(
                "terminal close operation '{}' does not own the released Powder recovery",
                operation.operation_id
            ));
        }
        if let Some(recovery) = released_recovery {
            let durable_recovery = current
                .pending_dispatch_releases
                .iter()
                .find(|pending| pending.crew_session_id == *terminal_id)
                .ok_or_else(|| {
                    format!(
                        "terminal close operation '{}' lost its durable Powder recovery",
                        operation.operation_id
                    )
                })?;
            let mut expected_recovery = recovery.clone();
            expected_recovery.state = PendingDispatchReleaseState::InFlight;
            if durable_recovery != &expected_recovery {
                return Err(format!(
                    "terminal close operation '{}' Powder recovery changed before finalization",
                    operation.operation_id
                ));
            }
        }

        let now = now_ms();
        let mut captain_state_changed = false;
        for captain in &mut current.captains {
            if captain.terminal_id.as_deref() == Some(terminal_id.as_str()) {
                captain.state = ClaimState::Orphaned { since: now };
                captain.terminal_id = None;
                for crew in &mut captain.crew {
                    if matches!(crew.state, CrewState::Active) {
                        crew.state = CrewState::Orphaned { since: now };
                    }
                }
                captain_state_changed = true;
            }
            for crew in &mut captain.crew {
                if crew.terminal_id != *terminal_id {
                    continue;
                }
                let next_state = if retain_crew_binding {
                    CrewState::CleanupPending { since: now }
                } else {
                    CrewState::Removed { since: now }
                };
                if crew.state != next_state {
                    crew.state = next_state;
                    captain_state_changed = true;
                }
                if let Some(recovery) = released_recovery {
                    let matches = crew.powder_work.as_ref().is_some_and(|work| {
                        work.card_id == recovery.card_id
                            && work.run_id == recovery.run_id
                            && work.agent.as_deref() == Some(recovery.agent.as_str())
                            && work.dispatch_release_recovery
                    });
                    if !matches {
                        return Err(format!(
                            "Crew session '{terminal_id}' Powder binding changed before terminal close commit"
                        ));
                    }
                    crew.powder_work = None;
                    captain_state_changed = true;
                } else if operation_recovery.is_some() && crew.powder_work.is_some() {
                    // A pre-retirement close operation may contain a frozen
                    // Powder release. Retire that legacy local binding without
                    // replaying its network effect.
                    crew.powder_work = None;
                    captain_state_changed = true;
                }
            }
        }
        if let Some(recovery) = released_recovery.or(operation_recovery) {
            current
                .pending_dispatch_releases
                .retain(|pending| pending.crew_session_id != recovery.crew_session_id);
        }
        let mut workspace_changed = false;
        for workspace in &mut current.workspaces {
            let before = workspace.tile_ids.len();
            workspace.tile_ids.retain(|tile| tile != terminal_id);
            workspace_changed |= workspace.tile_ids.len() != before;
        }
        current
            .pending_fleet_operations
            .retain(|pending| pending.operation_id != operation.operation_id);
        if !current.retired_fleet_tile_ids.contains(terminal_id) {
            if current.retired_fleet_tile_ids.len() == MAX_RETIRED_FLEET_TILES {
                current.retired_fleet_tile_ids.remove(0);
            }
            current.retired_fleet_tile_ids.push(terminal_id.clone());
        }
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(CloseTerminalCommitResult {
            captain_state_changed,
            workspace_changed,
        })
    }

    pub(super) fn close_workspace(
        &self,
        workspace_id: &str,
        force: bool,
        expected_owner: Option<&FleetWorkspaceOwner>,
    ) -> Result<CloseWorkspaceResult, String> {
        if workspace_id == CAPTAIN_WORKSPACE_ID {
            return Err("close_tab: Captain Workspace cannot be closed".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let workspace_index = current
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| format!("close_tab: unknown durable Workspace '{workspace_id}'"))?;
        let workspace = current.workspaces[workspace_index].clone();
        if workspace.kind != WorkspaceKind::Work {
            return Err("close_tab: Captain Workspace cannot be closed".into());
        }
        if current
            .workspaces
            .iter()
            .filter(|workspace| workspace.kind == WorkspaceKind::Work)
            .count()
            <= 1
        {
            return Err(
                "close_tab: refusing to close the last tab (the final Work Workspace)".into(),
            );
        }
        if workspace.owner.as_ref() != expected_owner && expected_owner.is_some() {
            return Err("acl: close_tab Workspace owner changed before durable commit".into());
        }
        if !workspace.tile_ids.is_empty() && !force {
            return Err(format!(
                "close_tab: tab '{workspace_id}' still holds {} tile(s); close its terminals first (close_terminal) or pass force: true",
                workspace.tile_ids.len()
            ));
        }

        let previous = current.clone();
        let owner = workspace.owner.clone();
        let owner_candidate_ids = current
            .workspaces
            .iter()
            .filter(|candidate| {
                candidate.id != workspace_id
                    && candidate.kind == WorkspaceKind::Work
                    && candidate.owner.as_ref() == owner.as_ref()
            })
            .map(|candidate| candidate.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut captains_changed = false;
        for captain in &mut current.captains {
            let owns_target = owner.as_ref().map_or_else(
                || captain.workspace_tab_ids.contains(&workspace.id),
                |owner| {
                    captain.ship_slug == owner.ship_slug
                        && captain.assignment_id == owner.assignment_id
                        && captain.project_id.as_deref() == Some(owner.project_id.as_str())
                },
            );
            if !owns_target {
                continue;
            }
            let before = captain.workspace_tab_ids.len();
            captain
                .workspace_tab_ids
                .retain(|candidate| candidate != workspace_id);
            captains_changed |= captain.workspace_tab_ids.len() != before;
            let candidates = captain
                .workspace_tab_ids
                .iter()
                .filter(|candidate| owner_candidate_ids.contains(candidate.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            for crew in &mut captain.crew {
                if crew.workspace_tab_id.as_deref() != Some(workspace_id) {
                    continue;
                }
                let live_assignable = matches!(
                    crew.state,
                    CrewState::Active | CrewState::NeedsAssignment { .. }
                );
                if live_assignable && candidates.len() == 1 {
                    crew.workspace_tab_id = Some(candidates[0].clone());
                    crew.state = CrewState::Active;
                } else {
                    crew.workspace_tab_id = None;
                    if live_assignable {
                        crew.state = CrewState::NeedsAssignment { since: now_ms() };
                    }
                }
                captains_changed = true;
            }
        }
        current.workspaces.remove(workspace_index);
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(CloseWorkspaceResult {
            removed_tile_ids: workspace.tile_ids,
            captains_changed,
        })
    }

    /// Fallible write-through of a snapshot to disk, WITHOUT the `inner` lock
    /// held (Incident D). Serialized by the dedicated `persist` mutex - never
    /// taken together with `inner` - so a stalled state write can't wedge a
    /// registry reader or the spawn hot path. The `persist` mutex also guards the
    /// last revision that reached disk: a snapshot older than what already landed
    /// is dropped, so two writers that dropped `inner` in one order but reach disk
    /// in the other never regress the file. A write failure is returned to the
    /// mutation, which restores its prior in-memory snapshot and fails the command.
    ///
    /// ATOMIC (temp + rename), mirroring `voice.rs`: the loader treats a corrupt
    /// file as empty (silently dropping every claim), so a crash mid-write must
    /// never leave a torn file. We write a full body to a unique temp path, then
    /// `rename` it over the target - `rename` replaces atomically (on Windows too,
    /// MOVEFILE_REPLACE_EXISTING), so a reader/loader always sees either the old
    /// complete file or the new complete file, never a partial one.
    pub(super) fn persist(&self, snap: CaptainsSnapshot) -> Result<(), String> {
        record_project_probe(5);
        if let Some(reason) = &self.write_blocked {
            return Err(format!(
                "captains registry is read-only until T-Hub is upgraded: {reason}"
            ));
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        // The ONLY lock held across the disk write. Never nested inside `inner`.
        let mut last = self.persist.lock().unwrap_or_else(|p| p.into_inner());
        if snap.seq < *last {
            // A newer revision already reached disk; this stale snapshot must not
            // clobber it.
            return Ok(());
        }
        if path.exists() {
            if let Ok(existing) = Self::read_snapshot(path) {
                if existing.schema_version < CAPTAINS_SCHEMA_VERSION {
                    let file_name = path
                        .file_name()
                        .ok_or_else(|| "captains registry path has no file name".to_string())?
                        .to_string_lossy();
                    let prefix = format!("{file_name}.migration-v{CAPTAINS_SCHEMA_VERSION}.");
                    let parent = path
                        .parent()
                        .ok_or_else(|| "captains registry path has no parent".to_string())?;
                    let already_backed_up = std::fs::read_dir(parent)
                        .map_err(|error| {
                            format!(
                                "captains registry migration backup directory '{}' could not be read: {error}",
                                parent.display()
                            )
                        })?
                        .flatten()
                        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix));
                    if !already_backed_up {
                        let backup = parent.join(format!("{prefix}{}.bak", now_ms()));
                        std::fs::copy(path, &backup).map_err(|error| {
                            format!(
                                "captains registry migration backup '{}' could not be written: {error}",
                                backup.display()
                            )
                        })?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            if let Err(error) = std::fs::set_permissions(
                                &backup,
                                std::fs::Permissions::from_mode(0o600),
                            ) {
                                let _ = std::fs::remove_file(&backup);
                                return Err(format!(
                                    "captains registry migration backup permissions on '{}' failed: {error}",
                                    backup.display()
                                ));
                            }
                        }
                    }
                }
            }
        }
        // Test seam: stand in for a slow/stalled disk write, holding `persist` but
        // NOT `inner`, so a test can prove a concurrent reader/mutator is unblocked.
        #[cfg(test)]
        if let Some(hook) = self.persist_hook.lock().unwrap().as_ref() {
            hook();
        }
        #[cfg(test)]
        if let Some(reason) = self.fail_next_persist.lock().unwrap().take() {
            return Err(reason);
        }
        let body = serde_json::to_vec_pretty(&snap)
            .map_err(|error| format!("captains registry serialize failed: {error}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "captains registry directory '{}' could not be created: {error}",
                    parent.display()
                )
            })?;
        }
        // A unique temp name (pid + a process-wide counter) so two writers can
        // never interleave on the same temp file - each renames its own complete
        // body; last rename wins whole.
        static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::write(&tmp, &body).map_err(|error| {
            format!(
                "captains registry temp write to '{}' failed: {error}",
                tmp.display()
            )
        })?;
        // MED-4: item-2's BIND writes a per-session SECRET (the widened identity
        // binding) into this store, so 0600 it - the captains `persist` inherited the
        // process umask before (the 0600 discipline lived only in `write_handshake`
        // for control.json). Set it on the temp file BEFORE the atomic rename so the
        // target is never briefly world-readable. Best-effort (unix only), mirroring
        // `identity::write_atomic`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(
                |error| {
                    let _ = std::fs::remove_file(&tmp);
                    format!(
                        "captains registry permissions on '{}' failed: {error}",
                        tmp.display()
                    )
                },
            )?;
        }
        if path.exists() && Self::read_snapshot(path).is_ok() {
            let backup = path.with_extension("json.bak");
            std::fs::copy(path, &backup).map_err(|error| {
                let _ = std::fs::remove_file(&tmp);
                format!(
                    "captains registry backup '{}' could not be written: {error}",
                    backup.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600)).map_err(
                    |error| {
                        let _ = std::fs::remove_file(&tmp);
                        format!(
                            "captains registry backup permissions on '{}' failed: {error}",
                            backup.display()
                        )
                    },
                )?;
            }
        }
        if let Err(error) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "captains registry rename to '{}' failed: {error}",
                path.display()
            ));
        }
        *last = snap.seq;
        Ok(())
    }

    pub(super) fn commit_mutation(
        &self,
        mut current: std::sync::MutexGuard<'_, CaptainsInner>,
        previous: CaptainsInner,
    ) -> Result<(), String> {
        let mut candidate = current.clone();
        let projection_exclusions = self
            .workspace_projection_exclusions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Self::apply_workspace_projection_exclusions(&mut candidate, &projection_exclusions);
        candidate.workspaces =
            Self::reconcile_durable_workspaces(&candidate.captains, candidate.workspaces);
        let generation_changes = AuthorityGenerationChanges::between(&previous, &candidate);
        let generation_result = candidate.authority_generations.advance(generation_changes);
        *current = previous;
        drop(current);
        generation_result?;
        let snap = Self::snapshot_for_persist(&candidate);
        Self::validate_snapshot(&snap)?;
        self.persist(snap)?;
        let still_placed = candidate
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tile_ids.iter())
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        *self.lock() = candidate;
        self.workspace_projection_exclusions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|terminal_id, _| still_placed.contains(terminal_id));
        Ok(())
    }

    /// Install the test-only persist hook (see [`persist_hook`](Self::persist_hook)).
    #[cfg(test)]
    pub(super) fn set_persist_hook(&self, hook: Box<dyn Fn() + Send + Sync>) {
        *self.persist_hook.lock().unwrap() = Some(hook);
    }

    #[cfg(test)]
    pub(super) fn fail_next_persist(&self, reason: impl Into<String>) {
        *self.fail_next_persist.lock().unwrap() = Some(reason.into());
    }

    /// The full versioned snapshot (`list_captains` + every `sync_captains` forward).
    pub fn snapshot(&self) -> CaptainsSnapshot {
        let g = self.lock();
        let mut current = g.clone();
        let projection_exclusions = self
            .workspace_projection_exclusions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::apply_workspace_projection_exclusions(&mut current, &projection_exclusions);
        CaptainsSnapshot {
            schema_version: CAPTAINS_SCHEMA_VERSION,
            seq: current.seq,
            captains: current.captains,
            cortana: current.cortana,
            agent_sessions: current.agent_sessions,
            agent_checkpoints: current.agent_checkpoints,
            agent_events: current.agent_events,
            projects: current.projects,
            workspaces: current.workspaces,
            pending_fleet_operations: current.pending_fleet_operations,
            retired_fleet_tile_ids: current.retired_fleet_tile_ids,
            pending_dispatch_claims: current.pending_dispatch_claims,
            pending_dispatch_releases: current.pending_dispatch_releases,
            pending_git_initializations: current.pending_git_initializations,
        }
    }

    pub fn cortana_identity(&self) -> crate::cortana_reconcile::CortanaDurableIdentity {
        self.lock().cortana.clone()
    }

    pub(super) fn begin_cortana_recovery(
        &self,
        operation_id: &str,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let operation_id = operation_id.trim();
        if operation_id.is_empty() {
            return Err("reconcile_cortana requires a stable non-empty operationId".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if let Some(launch) = current.cortana.managed_launch.as_ref() {
            if launch.operation_id == operation_id {
                return Ok(current.cortana.clone());
            }
            return Err(format!(
                "reconcile_cortana operation '{}' owns the durable managed launch",
                launch.operation_id
            ));
        }
        if let Some(recovery) = current.cortana.active_harness_attestation_recovery.as_ref() {
            if recovery.operation_id == operation_id {
                return Ok(current.cortana.clone());
            }
            return Err(format!(
                "reconcile_cortana operation '{}' owns the active attestation recovery",
                recovery.operation_id
            ));
        }
        if let crate::cortana_reconcile::CortanaRecoveryState::Recovering {
            operation_id: active,
            ..
        } = &current.cortana.recovery
        {
            if active == operation_id {
                return Ok(current.cortana.clone());
            }
            return Err(format!(
                "reconcile_cortana operation '{active}' is already recovering the singleton"
            ));
        }
        if let crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
            operation_id: active,
            ..
        } = &current.cortana.recovery
        {
            if active == operation_id {
                return Ok(current.cortana.clone());
            }
            return Err(format!(
                "reconcile_cortana operation '{active}' is already replacing an exact orphan"
            ));
        }
        if let crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
            operation_id: active,
            ..
        } = &current.cortana.recovery
        {
            if active == operation_id {
                return Ok(current.cortana.clone());
            }
            return Err(format!(
                "reconcile_cortana operation '{active}' is already replacing a quarantined legacy runtime"
            ));
        }
        let previous = current.clone();
        current.cortana.recovery = crate::cortana_reconcile::CortanaRecoveryState::Recovering {
            operation_id: operation_id.to_string(),
            started_at: now_ms().max(1),
        };
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn bind_cortana_orphan_replacement_identity(
        &self,
        operation_id: &str,
        identity_id: &str,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let (active, replacement_identity_id) = match &mut current.cortana.recovery {
            crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                operation_id,
                replacement_identity_id,
                ..
            }
            | crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                operation_id,
                replacement_identity_id,
                ..
            } => (operation_id, replacement_identity_id),
            _ => return Err("reconcile_cortana: durable replacement intent disappeared".into()),
        };
        if active != operation_id {
            return Err("reconcile_cortana: durable orphan replacement operation changed".into());
        }
        if let Some(existing) = replacement_identity_id.as_deref() {
            if existing == identity_id {
                return Ok(current.cortana.clone());
            }
            return Err(
                "reconcile_cortana: orphan replacement identity is already reserved".into(),
            );
        }
        *replacement_identity_id = Some(identity_id.to_string());
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn prepare_cortana_orphan_replacement(
        &self,
        operation_id: &str,
        terminal_id: &str,
        identity_id: &str,
        generation: u64,
        harness: &str,
        effect_identity: crate::cortana_reconcile::CortanaOrphanEffectIdentity,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if !matches!(
            &current.cortana.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                operation_id: active,
                ..
            } if active == operation_id
        ) || current.cortana.identity_id.as_deref() != Some(identity_id)
            || current.cortana.generation != generation
            || current.cortana.harness.as_deref() != Some(harness)
            || !valid_cortana_effect_identity(&effect_identity)
        {
            return Err("test orphan replacement evidence changed before prepare".into());
        }
        let managed_basis = if let Some(owner) = current.cortana.owner.as_ref() {
            if !same_cortana_tmux_generation(&owner.tmux, &effect_identity) {
                return Err(
                    "managed orphan replacement owner evidence changed before prepare".into(),
                );
            }
            let claims = current
                .captains
                .iter()
                .filter(|captain| {
                    captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
                })
                .collect::<Vec<_>>();
            if claims.len() != 1
                || claims[0].terminal_id.as_deref() != Some(terminal_id)
                || claims[0].harness.as_deref() != Some(harness)
            {
                return Err("managed orphan replacement claim changed before prepare".into());
            }
            let claim = claims[0];
            Some(Box::new(
                crate::cortana_reconcile::CortanaManagedQuarantineBasis {
                    version: crate::cortana_reconcile::MANAGED_QUARANTINE_BASIS_VERSION,
                    claim_ship_slug: claim.ship_slug.clone(),
                    claim_assignment_id: claim.assignment_id.clone(),
                    claim_terminal_id: terminal_id.to_string(),
                    claim_harness: harness.to_string(),
                    owner: owner.clone(),
                    active_harness_attestation: current.cortana.active_harness_attestation.clone(),
                    replacement_generation: generation.saturating_add(1),
                    prior_ledger_count: current.cortana.quarantine_ledger.len(),
                    prior_ledger_sha256: cortana_quarantine_ledger_sha256(
                        &current.cortana.quarantine_ledger,
                    ),
                    workspace_ids: current
                        .workspaces
                        .iter()
                        .filter(|workspace| {
                            workspace.tile_ids.iter().any(|tile| tile == terminal_id)
                        })
                        .map(|workspace| workspace.id.clone())
                        .collect(),
                },
            ))
        } else {
            None
        };
        let previous = current.clone();
        current.cortana.terminal_id = Some(terminal_id.to_string());
        current.cortana.legacy_orphan_provenance = None;
        current.cortana.recovery =
            crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                operation_id: operation_id.to_string(),
                started_at: now_ms().max(1),
                orphan_terminal_id: terminal_id.to_string(),
                orphan_identity_id: identity_id.to_string(),
                orphan_generation: generation,
                harness: harness.to_string(),
                effect_identity,
                managed_basis,
                replacement_identity_id: None,
            };
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn quarantine_legacy_cortana(
        &self,
        operation_id: &str,
        terminal_id: &str,
        identity_id: &str,
        generation: u64,
        harness: &str,
        tmux: crate::cortana_reconcile::CortanaOrphanEffectIdentity,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        if operation_id.trim().is_empty()
            || terminal_id.trim().is_empty()
            || identity_id.trim().is_empty()
            || generation == 0
            || harness.trim().is_empty()
            || !valid_cortana_effect_identity(&tmux)
        {
            return Err("cannot quarantine incomplete legacy Cortana evidence".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if let crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
            operation_id: active,
            legacy_terminal_id,
            legacy_generation,
            ..
        } = &current.cortana.recovery
        {
            if active == operation_id
                && legacy_terminal_id == terminal_id
                && *legacy_generation == generation
                && current.cortana.quarantine_ledger.iter().any(|quarantine| {
                    quarantine.terminal_id == terminal_id
                        && quarantine.identity_id == identity_id
                        && quarantine.generation == generation
                        && quarantine.harness == harness
                        && quarantine.tmux == tmux
                        && quarantine.authority_revoked
                })
            {
                return Ok(current.cortana.clone());
            }
            return Err("a different legacy Cortana quarantine is already durable".into());
        }
        let matches_durable = current.cortana.identity_id.as_deref() == Some(identity_id)
            && current.cortana.generation == generation
            && current.cortana.harness.as_deref() == Some(harness);
        let adopts_uninitialized = current.cortana.identity_id.is_none()
            && current.cortana.generation == 0
            && current.cortana.terminal_id.is_none()
            && matches!(
                current.cortana.recovery,
                crate::cortana_reconcile::CortanaRecoveryState::Recovering { .. }
            );
        if !matches_durable && !adopts_uninitialized {
            return Err("legacy Cortana identity changed before quarantine".into());
        }
        if current.cortana.quarantine_ledger.len() >= MAX_CORTANA_QUARANTINE_RECORDS {
            return Err("Cortana quarantine ledger is full; no authority was changed".into());
        }
        if current.cortana.quarantine_ledger.iter().any(|quarantine| {
            quarantine.terminal_id == terminal_id
                || quarantine.identity_id == identity_id
                || quarantine.tmux == tmux
        }) {
            return Err("Cortana quarantine evidence conflicts with an existing record".into());
        }
        let managed_basis = match &current.cortana.recovery {
            crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                operation_id: active,
                orphan_terminal_id,
                orphan_identity_id,
                orphan_generation,
                harness: prepared_harness,
                effect_identity,
                managed_basis,
                ..
            } if active == operation_id
                && orphan_terminal_id == terminal_id
                && orphan_identity_id == identity_id
                && *orphan_generation == generation
                && prepared_harness == harness
                && *effect_identity == tmux =>
            {
                managed_basis.clone()
            }
            crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan { .. } => {
                return Err("Cortana quarantine WAL evidence changed before commit".into());
            }
            _ => None,
        };
        if let Some(basis) = managed_basis.as_ref() {
            if !managed_cortana_quarantine_basis_matches(
                &current,
                basis,
                terminal_id,
                identity_id,
                generation,
                harness,
                &tmux,
            ) {
                return Err("managed Cortana quarantine basis changed before commit".into());
            }
        }
        let quarantined_at = now_ms().max(1);
        let active_cortana_claims = current
            .captains
            .iter()
            .enumerate()
            .filter(|(_, captain)| {
                captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if managed_basis.is_some() && active_cortana_claims.len() != 1
            || managed_basis.is_none() && active_cortana_claims.len() > 1
            || active_cortana_claims.first().is_some_and(|index| {
                current.captains[*index].terminal_id.as_deref() != Some(terminal_id)
            })
        {
            return Err("legacy Cortana quarantine Fleet claim is ambiguous".into());
        }
        let previous = current.clone();
        if let Some(index) = active_cortana_claims.first().copied() {
            let claim = &mut current.captains[index];
            claim.state = ClaimState::Orphaned {
                since: quarantined_at,
            };
            claim.terminal_id = None;
            for crew in claim.crew.iter_mut() {
                if matches!(crew.state, CrewState::Active) {
                    crew.state = CrewState::Orphaned {
                        since: quarantined_at,
                    };
                }
            }
        }
        for workspace in &mut current.workspaces {
            workspace.tile_ids.retain(|tile| tile != terminal_id);
        }
        if !current
            .retired_fleet_tile_ids
            .iter()
            .any(|tile| tile == terminal_id)
        {
            if current.retired_fleet_tile_ids.len() == MAX_RETIRED_FLEET_TILES {
                current.retired_fleet_tile_ids.remove(0);
            }
            current.retired_fleet_tile_ids.push(terminal_id.to_string());
        }
        current.cortana.identity_id = Some(identity_id.to_string());
        current.cortana.generation = generation;
        current.cortana.harness = Some(harness.to_string());
        current.cortana.owner = None;
        current.cortana.active_harness_attestation = None;
        current.cortana.active_harness_attestation_recovery = None;
        current.cortana.terminal_id = None;
        current.cortana.legacy_orphan_provenance = None;
        current
            .cortana
            .quarantine_ledger
            .push(crate::cortana_reconcile::CortanaLegacyQuarantine {
                terminal_id: terminal_id.to_string(),
                identity_id: identity_id.to_string(),
                generation,
                harness: harness.to_string(),
                tmux,
                authority_revoked: true,
                quarantined_at,
            });
        current.cortana.recovery =
            crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                operation_id: operation_id.to_string(),
                quarantined_at,
                legacy_terminal_id: terminal_id.to_string(),
                legacy_generation: generation,
                replacement_identity_id: None,
            };
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn validate_cortana_managed_quarantine_basis(
        &self,
        operation_id: &str,
        terminal_id: &str,
        identity_id: &str,
        generation: u64,
        harness: &str,
        effect_identity: &crate::cortana_reconcile::CortanaOrphanEffectIdentity,
        basis: &crate::cortana_reconcile::CortanaManagedQuarantineBasis,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let current = self.lock();
        let wal_matches = matches!(
            &current.cortana.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                operation_id: active,
                orphan_terminal_id,
                orphan_identity_id,
                orphan_generation,
                harness: prepared_harness,
                effect_identity: prepared_effect,
                managed_basis: Some(prepared_basis),
                ..
            } if active == operation_id
                && orphan_terminal_id == terminal_id
                && orphan_identity_id == identity_id
                && *orphan_generation == generation
                && prepared_harness == harness
                && prepared_effect == effect_identity
                && prepared_basis.as_ref() == basis
        );
        if wal_matches
            && managed_cortana_quarantine_basis_matches(
                &current,
                basis,
                terminal_id,
                identity_id,
                generation,
                harness,
                effect_identity,
            )
        {
            Ok(())
        } else {
            Err("managed Cortana quarantine basis changed before authority burn".into())
        }
    }

    #[cfg(test)]
    pub(super) fn set_cortana_quarantine_claim_assignment_for_test(
        &self,
        assignment_id: &str,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let claim = current
            .captains
            .iter_mut()
            .find(|captain| {
                captain.role == FleetRole::Cortana && captain.state == ClaimState::Active
            })
            .ok_or("test managed quarantine claim disappeared")?;
        claim.assignment_id = assignment_id.to_string();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn set_cortana_quarantine_attestation_for_test(
        &self,
        attestation: Option<crate::cortana_reconcile::CortanaActiveHarnessAttestation>,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        current.cortana.active_harness_attestation = attestation;
        Ok(())
    }

    pub(super) fn mark_cortana_degraded(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<(), String> {
        if operation_id.trim().is_empty() || reason.trim().is_empty() {
            return Err("degraded Cortana state requires an operationId and reason".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if let Some(launch) = current.cortana.managed_launch.as_ref() {
            if launch.operation_id != operation_id {
                return Err(
                    "degraded Cortana operation does not match the durable managed launch".into(),
                );
            }
            return Ok(());
        }
        if current
            .cortana
            .active_harness_attestation_recovery
            .as_ref()
            .is_some_and(|recovery| recovery.operation_id == operation_id)
        {
            return Ok(());
        }
        if matches!(
            &current.cortana.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                operation_id: active,
                ..
            }
            | crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                operation_id: active,
                ..
            } if active == operation_id
        ) {
            // The replacement record is the durable write-ahead authorization
            // for an exact external effect. Never erase it with a presentation
            // error; the next startup must resume the same transaction.
            return Ok(());
        }
        let previous = current.clone();
        current.cortana.recovery = crate::cortana_reconcile::CortanaRecoveryState::Degraded {
            operation_id: operation_id.to_string(),
            reason: reason.to_string(),
            detected_at: now_ms().max(1),
        };
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    pub(super) fn prepare_cortana_active_attestation_recovery(
        &self,
        recovery: crate::cortana_reconcile::CortanaActiveHarnessAttestationRecovery,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current.cortana.active_harness_attestation_recovery.as_ref() == Some(&recovery) {
            return Ok(current.cortana.clone());
        }
        if current
            .cortana
            .active_harness_attestation_recovery
            .is_some()
            || current.cortana.active_harness_attestation.is_some()
            || current.cortana.managed_launch.is_some()
        {
            return Err("a different Cortana attestation transaction is already durable".into());
        }
        let claim_matches = current
            .captains
            .iter()
            .filter(|claim| claim.role == FleetRole::Cortana && claim.state == ClaimState::Active)
            .collect::<Vec<_>>();
        if !matches!(
            claim_matches.as_slice(),
            [claim]
                if claim.terminal_id.as_deref() == Some(recovery.terminal_id.as_str())
                    && claim.provider.as_deref().or(claim.harness.as_deref())
                        == Some(recovery.harness.as_str())
        ) {
            return Err("Cortana attestation recovery has no exact active Fleet claim".into());
        }
        current.cortana.active_harness_attestation_recovery = Some(recovery);
        if !valid_cortana_active_harness_attestation_recovery(
            &current.cortana,
            current
                .cortana
                .active_harness_attestation_recovery
                .as_ref()
                .expect("assigned above"),
        ) {
            return Err("Cortana attestation recovery evidence changed before prepare".into());
        }
        // `previous` must describe the state before the WAL assignment.
        let mut previous = current.clone();
        previous.cortana.active_harness_attestation_recovery = None;
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn commit_cortana_active_attestation_recovery(
        &self,
        expected: &crate::cortana_reconcile::CortanaActiveHarnessAttestationRecovery,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current.cortana.active_harness_attestation_recovery.as_ref() != Some(expected)
            || !valid_cortana_active_harness_attestation_recovery(&current.cortana, expected)
        {
            return Err("Cortana attestation recovery changed before commit".into());
        }
        let previous = current.clone();
        current.cortana.active_harness_attestation =
            Some(crate::cortana_reconcile::CortanaActiveHarnessAttestation {
                version: crate::cortana_reconcile::ACTIVE_HARNESS_ATTESTATION_VERSION,
                expected_launch_provenance: expected.expected_launch_provenance.clone(),
                process: expected.process.clone(),
            });
        current.cortana.active_harness_attestation_recovery = None;
        current.cortana.recovery = crate::cortana_reconcile::CortanaRecoveryState::Healthy {
            operation_id: expected.operation_id.clone(),
            verified_at: now_ms().max(1),
        };
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn complete_cortana_keep(
        &self,
        operation_id: &str,
        expected: &crate::cortana_reconcile::CortanaDurableIdentity,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current.cortana != *expected
            || !matches!(
                &current.cortana.recovery,
                crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                    operation_id: active,
                    ..
                } if active == operation_id
            )
            || current.cortana.active_harness_attestation.is_none()
            || current
                .cortana
                .active_harness_attestation_recovery
                .is_some()
            || current.cortana.managed_launch.is_some()
        {
            return Err("Cortana Keep authority changed before completion".into());
        }
        let previous = current.clone();
        current.cortana.recovery = crate::cortana_reconcile::CortanaRecoveryState::Healthy {
            operation_id: operation_id.to_string(),
            verified_at: now_ms().max(1),
        };
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn commit_cortana_runtime(
        &self,
        operation_id: &str,
        identity_id: &str,
        generation: u64,
        terminal_id: &str,
        harness: &str,
        provider_session_id: Option<&str>,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        if operation_id.trim().is_empty()
            || identity_id.trim().is_empty()
            || generation == 0
            || terminal_id.trim().is_empty()
            || harness.trim().is_empty()
        {
            return Err("cannot commit an incomplete Cortana runtime identity".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let incumbent = current
            .captains
            .iter()
            .find(|captain| {
                captain.role == FleetRole::Cortana
                    && captain.terminal_id.as_deref() == Some(terminal_id)
                    && captain.state == ClaimState::Active
            })
            .ok_or("cannot commit Cortana before its active Fleet claim is authoritative")?;
        if incumbent
            .provider
            .as_deref()
            .or(incumbent.harness.as_deref())
            != Some(harness)
        {
            return Err("Cortana runtime harness does not match its Fleet claim".into());
        }
        if let crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
            operation_id: active_operation_id,
            orphan_terminal_id,
            orphan_generation,
            harness: replacement_harness,
            replacement_identity_id,
            ..
        } = &current.cortana.recovery
        {
            if active_operation_id != operation_id
                || replacement_identity_id.as_deref() != Some(identity_id)
                || generation != orphan_generation.saturating_add(1)
                || terminal_id == orphan_terminal_id
                || harness != replacement_harness
            {
                return Err(
                    "cannot commit Cortana runtime outside its durable orphan replacement intent"
                        .into(),
                );
            }
        }
        if let crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
            operation_id: active_operation_id,
            legacy_terminal_id,
            legacy_generation,
            replacement_identity_id,
            ..
        } = &current.cortana.recovery
        {
            if active_operation_id != operation_id
                || replacement_identity_id.as_deref() != Some(identity_id)
                || generation != legacy_generation.saturating_add(1)
                || terminal_id == legacy_terminal_id
            {
                return Err(
                    "cannot commit Cortana runtime outside its durable legacy quarantine".into(),
                );
            }
        }
        #[cfg(test)]
        if current.cortana.owner.is_none() {
            current.cortana.owner = Some(synthetic_cortana_managed_owner());
        }
        if current.cortana.owner.is_none() {
            return Err("cannot commit Cortana without a durable managed owner".into());
        }
        #[cfg(test)]
        if current.cortana.managed_launch.is_none() {
            let owner = current
                .cortana
                .owner
                .as_ref()
                .expect("owner synthesized above");
            current.cortana.managed_launch =
                Some(crate::cortana_reconcile::CortanaManagedLaunchIntent {
                    version: 4,
                    operation_id: operation_id.to_string(),
                    terminal_id: terminal_id.to_string(),
                    tmux_target: tmux_target(terminal_id),
                    identity_id: identity_id.to_string(),
                    generation,
                    harness: harness.to_string(),
                    unit_name: owner.unit_name.clone(),
                    launch_nonce: owner.launch_nonce.clone(),
                    tools: owner.tools.clone(),
                    phase: crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed,
                    expected_harness_launch_provenance: Some(
                        synthetic_cortana_expected_harness_launch(harness),
                    ),
                    harness_process: Some(synthetic_cortana_harness_process(owner, harness)),
                });
        }
        let launch = current
            .cortana
            .managed_launch
            .as_ref()
            .ok_or("cannot commit Cortana without an observed managed launch")?;
        if launch.version != 4
            || launch.phase != crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
            || launch
                .expected_harness_launch_provenance
                .as_ref()
                .is_none_or(|expected| expected.provider != launch.harness)
            || launch.harness_process.is_none()
            || launch.operation_id != operation_id
            || launch.identity_id != identity_id
            || launch.generation != generation
            || launch.terminal_id != terminal_id
            || launch.harness != harness
        {
            return Err("cannot commit Cortana outside its observed managed launch".into());
        }
        let active_harness_attestation =
            crate::cortana_reconcile::CortanaActiveHarnessAttestation {
                version: crate::cortana_reconcile::ACTIVE_HARNESS_ATTESTATION_VERSION,
                expected_launch_provenance: launch
                    .expected_harness_launch_provenance
                    .clone()
                    .expect("validated observed launch has expected provenance"),
                process: launch
                    .harness_process
                    .clone()
                    .expect("validated observed launch has process evidence"),
            };
        current.cortana.identity_id = Some(identity_id.to_string());
        current.cortana.generation = generation;
        current.cortana.terminal_id = Some(terminal_id.to_string());
        current.cortana.harness = Some(harness.to_string());
        current.cortana.legacy_orphan_provenance = None;
        current.cortana.managed_launch = None;
        current.cortana.active_harness_attestation = Some(active_harness_attestation);
        current.cortana.active_harness_attestation_recovery = None;
        if let Some(provider_session_id) = provider_session_id {
            current.cortana.provider_session_id = Some(provider_session_id.to_string());
            current.cortana.conversation_id = Some(provider_session_id.to_string());
        }
        current.cortana.recovery = crate::cortana_reconcile::CortanaRecoveryState::Healthy {
            operation_id: operation_id.to_string(),
            verified_at: now_ms().max(1),
        };
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn record_cortana_runtime_owner(
        &self,
        operation_id: &str,
        terminal_id: &str,
        owner: crate::cortana_reconcile::CortanaManagedOwnerToken,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        if operation_id.trim().is_empty()
            || terminal_id.trim().is_empty()
            || owner.version != crate::cortana_reconcile::MANAGED_OWNER_TOKEN_VERSION
        {
            return Err("cannot record an invalid Cortana runtime owner".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let active_operation = match &current.cortana.recovery {
            crate::cortana_reconcile::CortanaRecoveryState::Recovering { operation_id, .. }
            | crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                operation_id,
                ..
            }
            | crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                operation_id,
                ..
            } => operation_id,
            _ => return Err("Cortana owner cannot be recorded outside recovery".into()),
        };
        if active_operation != operation_id {
            return Err("Cortana owner operation does not match durable recovery".into());
        }
        let launch = current
            .cortana
            .managed_launch
            .as_ref()
            .ok_or("Cortana owner has no durable prepared launch")?;
        if launch.operation_id != operation_id
            || launch.terminal_id != terminal_id
            || launch.unit_name != owner.unit_name
            || launch.launch_nonce != owner.launch_nonce
            || launch.phase != crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared
        {
            return Err("Cortana owner does not match its durable prepared launch".into());
        }
        if let Some(existing) = current.cortana.owner.as_ref() {
            return if existing == &owner {
                Ok(current.cortana.clone())
            } else {
                Err("a different Cortana runtime owner is already durable".into())
            };
        }
        let launch_version = launch.version;
        let previous = current.clone();
        current.cortana.owner = Some(owner);
        current.cortana.terminal_id = Some(terminal_id.to_string());
        current
            .cortana
            .managed_launch
            .as_mut()
            .expect("checked above")
            .phase = if launch_version == 1 {
            crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
        } else {
            crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
        };
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    #[cfg(test)]
    pub(super) fn replace_cortana_runtime_owner_for_test(
        &self,
        expected: &crate::cortana_reconcile::CortanaManagedOwnerToken,
        replacement: crate::cortana_reconcile::CortanaManagedOwnerToken,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current.cortana.owner.as_ref() != Some(expected) {
            return Err("test Cortana owner changed before replacement".into());
        }
        let previous = current.clone();
        current.cortana.owner = Some(replacement);
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    pub(super) fn record_cortana_expected_harness_launch_provenance(
        &self,
        operation_id: &str,
        terminal_id: &str,
        identity_id: &str,
        generation: u64,
        expected: crate::harness::ExpectedHarnessLaunchProvenance,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        if operation_id.trim().is_empty()
            || terminal_id.trim().is_empty()
            || identity_id.trim().is_empty()
            || generation == 0
            || !crate::harness::valid_expected_harness_launch_provenance(&expected)
        {
            return Err("cannot record invalid expected Cortana Harness launch provenance".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let launch = current
            .cortana
            .managed_launch
            .as_ref()
            .ok_or("expected Cortana Harness provenance has no durable managed launch")?;
        if launch.operation_id != operation_id
            || launch.terminal_id != terminal_id
            || launch.identity_id != identity_id
            || launch.generation != generation
            || launch.harness != expected.provider
        {
            return Err("expected Cortana Harness provenance does not match its launch".into());
        }
        let same_bound_entry = |legacy: &crate::harness::ExpectedHarnessLaunchProvenance| {
            legacy.provider == expected.provider
                && legacy.kind == expected.kind
                && legacy.executable == expected.executable
                && legacy.entry_script == expected.entry_script
                && legacy.trusted_child_executable == expected.trusted_child_executable
                && legacy.argv_layout_sha256 == expected.argv_layout_sha256
        };
        if launch.version == 4 {
            if launch.expected_harness_launch_provenance.as_ref() == Some(&expected) {
                return Ok(current.cortana.clone());
            }
            if launch
                .expected_harness_launch_provenance
                .as_ref()
                .is_none_or(|legacy| {
                    legacy.version != 2
                        || legacy.launch_policy_sha256.is_some()
                        || legacy.semantic_argv_sha256.is_some()
                        || !same_bound_entry(legacy)
                })
            {
                return Err(
                    "different expected Cortana Harness provenance is already durable".into(),
                );
            }
        }
        if launch.version == 3
            && launch
                .expected_harness_launch_provenance
                .as_ref()
                .is_none_or(|legacy| {
                    legacy.provider != expected.provider
                        || legacy.kind != expected.kind
                        || legacy.executable != expected.executable
                        || legacy.entry_script != expected.entry_script
                        || legacy.argv_layout_sha256 != expected.argv_layout_sha256
                })
        {
            return Err(
                "expected Cortana Harness provenance changed while enriching its trusted child"
                    .into(),
            );
        }
        if !matches!(launch.version, 1..=4)
            || (launch.version != 3 && launch.expected_harness_launch_provenance.is_some())
                && launch.version != 4
        {
            return Err("expected Cortana Harness provenance cannot enrich this launch".into());
        }
        let previous = current.clone();
        let launch = current
            .cortana
            .managed_launch
            .as_mut()
            .expect("checked above");
        launch.version = 4;
        if launch.phase == crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
            && launch.harness_process.is_none()
        {
            launch.phase = crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved;
        }
        launch.expected_harness_launch_provenance = Some(expected);
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn record_cortana_harness_process(
        &self,
        operation_id: &str,
        terminal_id: &str,
        process: crate::harness::HarnessProcessIdentity,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        if operation_id.trim().is_empty()
            || terminal_id.trim().is_empty()
            || !crate::harness::valid_harness_process_identity(&process)
        {
            return Err("cannot record invalid Cortana Harness process evidence".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let launch = current
            .cortana
            .managed_launch
            .as_ref()
            .ok_or("Cortana Harness process has no durable managed launch")?;
        if launch.operation_id != operation_id
            || launch.terminal_id != terminal_id
            || !matches!(
                launch.phase,
                crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                    | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
            )
        {
            return Err("Cortana Harness process does not match its managed launch".into());
        }
        let owner = current
            .cortana
            .owner
            .as_ref()
            .ok_or("Cortana Harness process has no durable managed owner")?;
        if process.provider != launch.harness
            || process.tmux_session_id != owner.tmux.tmux_session_id
            || process.tmux_session_created != owner.tmux.tmux_session_created
            || process.tmux_window_id != owner.tmux.tmux_window_id
            || process.tmux_pane_id != owner.tmux.tmux_pane_id
            || process.pane_pid != owner.tmux.pane_pid
            || process.pane_start_ticks != owner.tmux.pane_start_ticks
            || process.cgroup_path != owner.cgroup_path
        {
            return Err("Cortana Harness process does not match its managed owner".into());
        }
        if launch.version != 4 {
            return Err("Cortana Harness process has an unsupported launch version".into());
        }
        if matches!(
            launch.phase,
            crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
        ) && launch.harness_process.is_some()
        {
            return if launch.harness_process.as_ref() == Some(&process) {
                Ok(current.cortana.clone())
            } else {
                Err("a different Cortana Harness process is already durable".into())
            };
        }
        if !matches!(
            launch.phase,
            crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed
        ) {
            return Err("Cortana Harness process cannot attest this launch phase".into());
        }
        let previous = current.clone();
        let launch = current
            .cortana
            .managed_launch
            .as_mut()
            .expect("checked above");
        launch.phase = crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed;
        launch.harness_process = Some(process);
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn record_cortana_claimed_launch(
        &self,
        operation_id: &str,
        terminal_id: &str,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let exact_claims = current
            .captains
            .iter()
            .filter(|claim| claim.role == FleetRole::Cortana && claim.state == ClaimState::Active)
            .collect::<Vec<_>>();
        let launch = current
            .cortana
            .managed_launch
            .as_ref()
            .ok_or("claimed Cortana has no managed launch")?;
        if launch.operation_id != operation_id
            || launch.terminal_id != terminal_id
            || launch.phase != crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
            || launch.harness_process.is_none()
            || !matches!(
                exact_claims.as_slice(),
                [claim]
                    if claim.terminal_id.as_deref() == Some(terminal_id)
                        && claim.provider.as_deref().or(claim.harness.as_deref())
                            == Some(launch.harness.as_str())
            )
        {
            return Err("Cortana Fleet claim changed before durable claim phase".into());
        }
        let previous = current.clone();
        current
            .cortana
            .managed_launch
            .as_mut()
            .expect("checked above")
            .phase = crate::cortana_reconcile::CortanaManagedLaunchPhase::Claimed;
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn prepare_cortana_managed_launch(
        &self,
        operation_id: &str,
        terminal_id: &str,
        identity_id: &str,
        generation: u64,
        harness: &str,
        launch: &tmux::ManagedRuntimeLaunchSpec,
        expected_harness_launch_provenance: crate::harness::ExpectedHarnessLaunchProvenance,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let tools = durable_cortana_tools(&launch.tools);
        let intent = crate::cortana_reconcile::CortanaManagedLaunchIntent {
            version: 4,
            operation_id: operation_id.to_string(),
            terminal_id: terminal_id.to_string(),
            tmux_target: tmux_target(terminal_id),
            identity_id: identity_id.to_string(),
            generation,
            harness: harness.to_string(),
            unit_name: launch.unit_name.clone(),
            launch_nonce: launch.launch_nonce.clone(),
            tools,
            phase: crate::cortana_reconcile::CortanaManagedLaunchPhase::Prepared,
            expected_harness_launch_provenance: Some(expected_harness_launch_provenance),
            harness_process: None,
        };
        if !valid_cortana_managed_launch(&intent) {
            return Err("cannot prepare an invalid Cortana managed launch".into());
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let active_operation = match &current.cortana.recovery {
            crate::cortana_reconcile::CortanaRecoveryState::Recovering { operation_id, .. }
            | crate::cortana_reconcile::CortanaRecoveryState::ReplacingOrphan {
                operation_id,
                ..
            }
            | crate::cortana_reconcile::CortanaRecoveryState::LegacyUnownedQuarantined {
                operation_id,
                ..
            } => operation_id,
            _ => return Err("Cortana launch cannot be prepared outside recovery".into()),
        };
        if active_operation != operation_id {
            return Err("Cortana launch operation changed before prepare".into());
        }
        if let Some(existing) = current.cortana.managed_launch.as_ref() {
            return if existing == &intent {
                Ok(current.cortana.clone())
            } else {
                Err("a different Cortana managed launch is already prepared".into())
            };
        }
        let previous = current.clone();
        current.cortana.managed_launch = Some(intent);
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn clear_prepared_cortana_managed_launch(
        &self,
        expected: &crate::cortana_reconcile::CortanaManagedLaunchIntent,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if current.cortana.managed_launch.as_ref() != Some(expected) {
            return Err("Cortana managed launch changed before cleanup commit".into());
        }
        let previous = current.clone();
        current.cortana.managed_launch = None;
        if matches!(
            expected.phase,
            crate::cortana_reconcile::CortanaManagedLaunchPhase::OwnerObserved
                | crate::cortana_reconcile::CortanaManagedLaunchPhase::Observed
        ) {
            current.cortana.owner = None;
            current.cortana.active_harness_attestation = None;
            current.cortana.active_harness_attestation_recovery = None;
            current.cortana.terminal_id = None;
        }
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn clear_gone_cortana_runtime_owner(
        &self,
        operation_id: &str,
        expected: &crate::cortana_reconcile::CortanaManagedOwnerToken,
    ) -> Result<crate::cortana_reconcile::CortanaDurableIdentity, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if !matches!(
            &current.cortana.recovery,
            crate::cortana_reconcile::CortanaRecoveryState::Recovering {
                operation_id: active,
                ..
            } if active == operation_id
        ) || current.cortana.owner.as_ref() != Some(expected)
        {
            return Err("Cortana owner changed before gone-owner recovery".into());
        }
        let terminal_id = current
            .cortana
            .terminal_id
            .clone()
            .ok_or("gone Cortana owner has no durable terminal")?;
        if current.captains.iter().any(|claim| {
            claim.role == FleetRole::Cortana
                && claim.state == ClaimState::Active
                && claim.terminal_id.as_deref() != Some(terminal_id.as_str())
        }) {
            return Err("Cortana Fleet authority changed before gone-owner recovery".into());
        }
        let previous = current.clone();
        let now = now_ms();
        for claim in current.captains.iter_mut().filter(|claim| {
            claim.role == FleetRole::Cortana
                && claim.state == ClaimState::Active
                && claim.terminal_id.as_deref() == Some(terminal_id.as_str())
        }) {
            claim.state = ClaimState::Orphaned { since: now };
            claim.terminal_id = None;
        }
        current.cortana.owner = None;
        current.cortana.active_harness_attestation = None;
        current.cortana.active_harness_attestation_recovery = None;
        current.cortana.terminal_id = None;
        current.seq = current.seq.saturating_add(1);
        let result = current.cortana.clone();
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub fn append_agent_checkpoint(
        &self,
        agent_session_id: &str,
        author_session_id: &str,
        summary: &str,
        stage: Option<crate::agent_session::WorkStage>,
    ) -> Result<AgentCheckpoint, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let agent_index = current
            .agent_sessions
            .iter()
            .position(|agent| agent.agent_session_id == agent_session_id)
            .ok_or_else(|| format!("agent_checkpoint: agent '{agent_session_id}' was not found"))?;
        let cursor = current.seq.saturating_add(1);
        let checkpoint = AgentCheckpoint {
            cursor,
            agent_session_id: agent_session_id.to_string(),
            author_session_id: author_session_id.to_string(),
            summary: summary.to_string(),
            created_at: now_ms(),
        };
        checkpoint.validate()?;
        if current.agent_sessions[agent_index].work_stage
            == crate::agent_session::WorkStage::Stopped
            && stage.is_some_and(|stage| stage != crate::agent_session::WorkStage::Stopped)
        {
            return Err(
                "agent_checkpoint: stopped is a terminal work stage and cannot be resumed".into(),
            );
        }
        current.agent_sessions[agent_index].updated_at = checkpoint.created_at;
        if let Some(stage) = stage {
            current.agent_sessions[agent_index].work_stage = stage;
        }
        let runtime_state = current.agent_sessions[agent_index].runtime_state;
        let work_stage = current.agent_sessions[agent_index].work_stage;
        let delivery_states = current.agent_sessions[agent_index].delivery_states();
        current.agent_checkpoints.push(checkpoint.clone());
        current.agent_events.push(AgentEvent {
            cursor,
            agent_session_id: agent_session_id.to_string(),
            kind: "checkpoint".into(),
            created_at: checkpoint.created_at,
            runtime_state: Some(runtime_state),
            work_stage: Some(work_stage),
            checkpoint: Some(checkpoint.clone()),
            delivery_states,
        });
        if current.agent_checkpoints.len() > crate::agent_session::MAX_CHECKPOINT_HISTORY {
            let overflow =
                current.agent_checkpoints.len() - crate::agent_session::MAX_CHECKPOINT_HISTORY;
            current.agent_checkpoints.drain(0..overflow);
        }
        if current.agent_events.len() > crate::agent_session::MAX_CHECKPOINT_HISTORY {
            let overflow =
                current.agent_events.len() - crate::agent_session::MAX_CHECKPOINT_HISTORY;
            current.agent_events.drain(0..overflow);
        }
        current.seq = cursor;
        self.commit_mutation(current, previous)?;
        Ok(checkpoint)
    }

    pub(super) fn record_agent_delivery(
        &self,
        agent_session_id: &str,
        update: AgentDeliveryUpdate,
    ) -> Result<AgentSessionRecord, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let agent = current
            .agent_sessions
            .iter_mut()
            .find(|agent| agent.agent_session_id == agent_session_id)
            .ok_or_else(|| {
                format!("record_agent_delivery: agent '{agent_session_id}' was not found")
            })?;
        if agent.work_stage == crate::agent_session::WorkStage::Stopped {
            return Err(
                "record_agent_delivery: a stopped lane is discarded and cannot accept new delivery evidence"
                    .into(),
            );
        }
        let delivery = agent
            .delivery
            .as_mut()
            .ok_or("record_agent_delivery: legacy agent has no exact dispatch baseline")?;
        update.apply(delivery)?;
        let states = delivery.states();
        if states.complete && agent.work_stage != crate::agent_session::WorkStage::Stopped {
            agent.work_stage = crate::agent_session::WorkStage::Complete;
        }
        agent.updated_at = now_ms();
        agent.validate()?;
        let result = agent.clone();
        let cursor = current.seq.saturating_add(1);
        current.agent_events.push(AgentEvent {
            cursor,
            agent_session_id: agent_session_id.to_string(),
            kind: "delivery_evidence".into(),
            created_at: result.updated_at,
            runtime_state: Some(result.runtime_state),
            work_stage: Some(result.work_stage),
            checkpoint: None,
            delivery_states: Some(states),
        });
        if current.agent_events.len() > crate::agent_session::MAX_CHECKPOINT_HISTORY {
            let overflow =
                current.agent_events.len() - crate::agent_session::MAX_CHECKPOINT_HISTORY;
            current.agent_events.drain(0..overflow);
        }
        current.seq = cursor;
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub fn insert_agent_session(&self, record: AgentSessionRecord) -> Result<(), String> {
        record.validate_for_dispatch()?;
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        if current
            .agent_sessions
            .iter()
            .any(|agent| agent.agent_session_id == record.agent_session_id)
        {
            return Err(format!(
                "agent session '{}' already exists",
                record.agent_session_id
            ));
        }
        if !current
            .projects
            .iter()
            .any(|project| project.project_id == record.project_id)
        {
            return Err(format!(
                "agent session '{}' references unknown projectId '{}'",
                record.agent_session_id, record.project_id
            ));
        }
        if !current.captains.iter().any(|captain| {
            captain.terminal_id.as_deref() == Some(record.captain_session_id.as_str())
        }) {
            return Err(format!(
                "agent session '{}' references unknown captainSessionId '{}'",
                record.agent_session_id, record.captain_session_id
            ));
        }
        current.agent_sessions.push(record);
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    pub fn mark_agent_started(
        &self,
        agent_session_id: &str,
        workspace_tab_id: Option<String>,
    ) -> Result<AgentSessionRecord, String> {
        self.update_agent_session_with_event(agent_session_id, "started", |agent| {
            agent.workspace_tab_id = workspace_tab_id;
            agent.runtime_state = RuntimeState::Running;
            agent.updated_at = now_ms();
        })
    }

    pub fn mark_agent_unavailable(&self, agent_session_id: &str) -> Result<(), String> {
        self.update_agent_session_with_event(agent_session_id, "unavailable", |agent| {
            agent.runtime_state = RuntimeState::Unavailable;
            agent.work_stage = crate::agent_session::WorkStage::Stopped;
            agent.updated_at = now_ms();
        })
        .map(|_| ())
    }

    /// Reconcile runtime evidence without changing the explicit work stage.
    /// Provider identity is write-once from trusted runtime evidence, while an
    /// absent identity remains unknown until the provider reports one.
    pub fn reconcile_agent_runtime(
        &self,
        agent_session_id: &str,
        runtime_state: RuntimeState,
        provider_conversation_id: Option<String>,
    ) -> Result<AgentSessionRecord, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let agent = current
            .agent_sessions
            .iter_mut()
            .find(|agent| agent.agent_session_id == agent_session_id)
            .ok_or_else(|| format!("agent session '{agent_session_id}' was not found"))?;
        let changed = agent.runtime_state != runtime_state
            || provider_conversation_id
                .as_deref()
                .is_some_and(|id| agent.provider_conversation_id.as_deref() != Some(id));
        if !changed {
            return Ok(agent.clone());
        }
        agent.runtime_state = runtime_state;
        if provider_conversation_id.is_some() {
            agent.provider_conversation_id = provider_conversation_id;
        }
        agent.updated_at = now_ms();
        agent.validate()?;
        let result = agent.clone();
        let cursor = current.seq.saturating_add(1);
        current.agent_events.push(AgentEvent {
            cursor,
            agent_session_id: agent_session_id.to_string(),
            kind: "runtime_reconciled".into(),
            created_at: result.updated_at,
            runtime_state: Some(result.runtime_state),
            work_stage: Some(result.work_stage),
            checkpoint: None,
            delivery_states: result.delivery_states(),
        });
        if current.agent_events.len() > crate::agent_session::MAX_CHECKPOINT_HISTORY {
            let overflow =
                current.agent_events.len() - crate::agent_session::MAX_CHECKPOINT_HISTORY;
            current.agent_events.drain(0..overflow);
        }
        current.seq = cursor;
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub fn update_agent_stage(
        &self,
        agent_session_id: &str,
        stage: crate::agent_session::WorkStage,
    ) -> Result<AgentSessionRecord, String> {
        self.update_agent_session_with_event(agent_session_id, "stage_changed", |agent| {
            agent.work_stage = stage;
            agent.updated_at = now_ms();
        })
    }

    pub fn replace_agent_assignment(
        &self,
        agent_session_id: &str,
        assignment: &str,
    ) -> Result<AgentSessionRecord, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let agent = current
            .agent_sessions
            .iter_mut()
            .find(|agent| agent.agent_session_id == agent_session_id)
            .ok_or_else(|| format!("agent session '{agent_session_id}' was not found"))?;
        if agent.assignment == assignment {
            return Ok(agent.clone());
        }
        agent.assignment = assignment.to_string();
        agent.updated_at = now_ms();
        agent.validate()?;
        let result = agent.clone();
        let cursor = current.seq.saturating_add(1);
        current.agent_events.push(AgentEvent {
            cursor,
            agent_session_id: agent_session_id.to_string(),
            kind: "assignment_replaced".into(),
            created_at: result.updated_at,
            runtime_state: Some(result.runtime_state),
            work_stage: Some(result.work_stage),
            checkpoint: None,
            delivery_states: result.delivery_states(),
        });
        if current.agent_events.len() > crate::agent_session::MAX_CHECKPOINT_HISTORY {
            let overflow =
                current.agent_events.len() - crate::agent_session::MAX_CHECKPOINT_HISTORY;
            current.agent_events.drain(0..overflow);
        }
        current.seq = cursor;
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    pub(super) fn update_agent_session_with_event(
        &self,
        agent_session_id: &str,
        kind: &str,
        update: impl FnOnce(&mut AgentSessionRecord),
    ) -> Result<AgentSessionRecord, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let agent = current
            .agent_sessions
            .iter_mut()
            .find(|agent| agent.agent_session_id == agent_session_id)
            .ok_or_else(|| format!("agent session '{agent_session_id}' was not found"))?;
        update(agent);
        agent.validate()?;
        let result = agent.clone();
        let cursor = current.seq.saturating_add(1);
        current.agent_events.push(AgentEvent {
            cursor,
            agent_session_id: agent_session_id.to_string(),
            kind: kind.to_string(),
            created_at: result.updated_at,
            runtime_state: Some(result.runtime_state),
            work_stage: Some(result.work_stage),
            checkpoint: None,
            delivery_states: result.delivery_states(),
        });
        if current.agent_events.len() > crate::agent_session::MAX_CHECKPOINT_HISTORY {
            let overflow =
                current.agent_events.len() - crate::agent_session::MAX_CHECKPOINT_HISTORY;
            current.agent_events.drain(0..overflow);
        }
        current.seq = cursor;
        self.commit_mutation(current, previous)?;
        Ok(result)
    }

    /// Capture durable registry values and their internal authority versions under
    /// one lock. The returned copies are safe to carry across remote I/O without
    /// holding either the registry lock or the mutation serializer.
    pub(super) fn snapshot_with_authority_generations(
        &self,
    ) -> (CaptainsSnapshot, AuthorityGenerations, u64) {
        let g = self.lock();
        (
            CaptainsSnapshot {
                schema_version: CAPTAINS_SCHEMA_VERSION,
                seq: g.seq,
                captains: g.captains.clone(),
                cortana: g.cortana.clone(),
                agent_sessions: g.agent_sessions.clone(),
                agent_checkpoints: g.agent_checkpoints.clone(),
                agent_events: g.agent_events.clone(),
                projects: g.projects.clone(),
                workspaces: g.workspaces.clone(),
                pending_fleet_operations: g.pending_fleet_operations.clone(),
                retired_fleet_tile_ids: g.retired_fleet_tile_ids.clone(),
                pending_dispatch_claims: g.pending_dispatch_claims.clone(),
                pending_dispatch_releases: g.pending_dispatch_releases.clone(),
                pending_git_initializations: g.pending_git_initializations.clone(),
            },
            g.authority_generations.clone(),
            self.authority_epoch,
        )
    }

    /// Return the durable project registry without exposing the registry lock.
    pub fn projects(&self) -> Vec<ProjectRecord> {
        self.lock().projects.clone()
    }

    pub fn pending_git_initializations(&self) -> Vec<GitInitIntent> {
        self.lock().pending_git_initializations.clone()
    }

    pub(super) fn recover_pending_git_initializations(&self) {
        let intents = self.pending_git_initializations();
        for intent in intents {
            if intent.phase == "foreign_git" {
                if git_init_fault("foreign_cleanup").is_err() {
                    continue;
                }
                let _ = self.clear_git_initialization(&intent.operation_id);
                continue;
            }
            if let Err(error) = recover_git_initialization(self, &intent) {
                let _ = self.update_git_initialization(
                    &intent.operation_id,
                    "recovery_blocked",
                    Some(error),
                );
            }
        }
    }

    pub(super) fn git_initialization_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.git_initialization
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn begin_git_initialization(
        &self,
        intent: GitInitIntent,
    ) -> Result<GitInitIntent, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        if let Some(existing) = current
            .pending_git_initializations
            .iter()
            .find(|candidate| candidate.root_path == intent.root_path)
        {
            if existing.name != intent.name || existing.owner_identity != intent.owner_identity {
                return Err(
                    "initialize_git has a conflicting durable transaction for this root".into(),
                );
            }
            return Ok(existing.clone());
        }
        let previous = current.clone();
        current.pending_git_initializations.push(intent.clone());
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)?;
        Ok(intent)
    }

    pub(super) fn update_git_initialization(
        &self,
        operation_id: &str,
        phase: &str,
        recovery_error: Option<String>,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        let intent = current
            .pending_git_initializations
            .iter_mut()
            .find(|candidate| candidate.operation_id == operation_id)
            .ok_or_else(|| format!("unknown Git initialization operation '{operation_id}'"))?;
        intent.phase = phase.to_string();
        intent.recovery_error = recovery_error;
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    pub(super) fn clear_git_initialization(&self, operation_id: &str) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut current = self.lock();
        let previous = current.clone();
        current
            .pending_git_initializations
            .retain(|intent| intent.operation_id != operation_id);
        if current.pending_git_initializations.len() == previous.pending_git_initializations.len() {
            return Ok(());
        }
        current.seq = current.seq.saturating_add(1);
        self.commit_mutation(current, previous)
    }

    /// Register or update one canonical repository. Repository roots and project
    /// ids are both unique so a rename cannot silently create two identities for
    /// the same checkout and an id cannot be repointed to another repository.
    pub fn upsert_project(&self, mut project: ProjectRecord) -> Result<ProjectRecord, String> {
        project.project_id = project.project_id.trim().to_string();
        project.name = project.name.trim().to_string();
        let identity_source = project.root_path.as_deref().unwrap_or(&project.repo_root);
        let identity = canonical_project_identity(identity_source)?;
        project.repo_root = identity.clone();
        project.root_path = Some(identity.clone());
        if project.vcs_capability.is_none() {
            record_project_probe(2);
            let detected = git::git_info_cached(&identity);
            project.vcs_capability = Some(if project.git_main_root.is_some() || detected.is_repo {
                "git".into()
            } else {
                "none".into()
            });
            if project.vcs_capability.as_deref() == Some("git") && project.git_main_root.is_none() {
                project.git_main_root = detected
                    .worktree_root
                    .map(|root| files::posix_form(&root))
                    .or_else(|| Some(identity.clone()));
            }
        }
        if let Some(main_root) = project.git_main_root.as_deref() {
            project.git_main_root = Some(canonical_project_identity(main_root)?);
        } else if project.vcs_capability.as_deref() == Some("git") {
            project.git_main_root = Some(identity.clone());
        }
        if project.project_id.is_empty() {
            return Err("projectId must not be empty".into());
        }
        if project.name.is_empty() {
            return Err("project name must not be empty".into());
        }
        if !matches!(
            project.vcs_capability.as_deref(),
            Some("git") | Some("none")
        ) {
            return Err("vcsCapability must be 'git' or 'none'".into());
        }
        if let Some(powder) = project.powder.as_mut() {
            powder.connection_profile = powder.connection_profile.trim().to_string();
            powder.repository = powder.repository.trim().to_string();
            if powder.connection_profile.is_empty() || powder.repository.is_empty() {
                return Err("Powder connectionProfile and repository must not be empty".into());
            }
        }

        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let by_id = g
            .projects
            .iter()
            .position(|p| p.project_id == project.project_id);
        let by_root = g.projects.iter().position(|p| {
            p.root_path
                .as_deref()
                .or(Some(p.repo_root.as_str()))
                .is_some_and(|root| files::posix_form(root) == identity)
        });
        if let (Some(id_index), Some(root_index)) = (by_id, by_root) {
            if id_index != root_index {
                return Err(
                    "projectId and repoRoot belong to different registered projects".into(),
                );
            }
        }
        if let Some(index) = by_id {
            if files::posix_form(&g.projects[index].repo_root) != identity {
                return Err(format!(
                    "projectId '{}' is already bound to '{}'",
                    project.project_id, g.projects[index].repo_root
                ));
            }
        }

        let index = by_id.or(by_root);
        if let Some(index) = index {
            project.project_id = g.projects[index].project_id.clone();
            project.created_at = g.projects[index].created_at;
            project.updated_at = g.projects[index].updated_at;
            if g.projects[index] == project {
                return Ok(project);
            }
            project.updated_at = now_ms();
            g.projects[index] = project.clone();
        } else {
            let now = now_ms();
            if project.created_at == 0 {
                project.created_at = now;
            }
            project.updated_at = now;
            g.projects.push(project.clone());
        }
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(project)
    }

    /// Advance a project's Powder cursor monotonically without allowing an event
    /// poll that raced a rebind to write into the replacement stream.
    pub fn advance_project_powder_cursor(
        &self,
        project_id: &str,
        connection_profile: &str,
        repository: &str,
        cursor: i64,
    ) -> Result<ProjectRecord, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let project = g
            .projects
            .iter_mut()
            .find(|project| project.project_id == project_id)
            .ok_or_else(|| format!("unknown projectId '{project_id}'"))?;
        let powder = project
            .powder
            .as_mut()
            .ok_or_else(|| format!("project '{project_id}' is not Powder-bound"))?;
        if powder.connection_profile != connection_profile || powder.repository != repository {
            return Err(format!(
                "project '{project_id}' Powder binding changed while events were being read"
            ));
        }
        if cursor <= powder.event_cursor {
            return Ok(project.clone());
        }
        powder.event_cursor = cursor;
        project.updated_at = now_ms();
        let updated = project.clone();
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(updated)
    }

    /// Bind an existing ship to its durable project and reset-safe assignment.
    /// The project and ship must already exist; commissioning creates both before
    /// this binding step and can therefore retry without inventing partial state.
    pub fn bind_ship_context(
        &self,
        ship_slug: &str,
        project_id: &str,
        assignment: &str,
        harness: &str,
    ) -> Result<CaptainRecord, String> {
        let assignment = assignment.trim();
        let harness = harness.trim();
        if assignment.is_empty() {
            return Err("assignment must not be empty".into());
        }
        if harness.is_empty() {
            return Err("harness must not be empty".into());
        }
        validate_harness_name(harness, "Captain harness")?;
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        if !g.projects.iter().any(|p| p.project_id == project_id) {
            return Err(format!("unknown projectId '{project_id}'"));
        }
        let captain = g
            .captains
            .iter_mut()
            .find(|c| c.ship_slug == ship_slug)
            .ok_or_else(|| format!("unknown shipSlug '{ship_slug}'"))?;
        captain.project_id = Some(project_id.to_string());
        captain.assignment_id = assignment_id_for(Some(project_id), ship_slug);
        captain.assignment = Some(assignment.to_string());
        captain.harness = Some(harness.to_string());
        let provider_changed = captain.provider.as_deref() != Some(harness);
        captain.provider = Some(harness.to_string());
        if harness != "claude" {
            captain.claude_uuid = None;
        }
        if provider_changed {
            captain.provider_session_id = None;
            captain.conversation_id = None;
        }
        let result = captain.clone();
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(result)
    }

    /// Rename one durable Captain identity without changing its Assignment,
    /// terminal binding, Harness, or Workspace ownership.
    pub fn rename_captain(
        &self,
        captain_session_id: Option<&str>,
        ship_slug: Option<&str>,
        display_name: &str,
    ) -> Result<CaptainRecord, String> {
        if captain_session_id.is_none() && ship_slug.is_none() {
            return Err("rename_captain requires 'captainSessionId' or 'shipSlug'".into());
        }
        let display_name = normalize_captain_display_name(display_name)?;
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let captain = g
            .captains
            .iter_mut()
            .find(|captain| {
                captain_session_id.is_some_and(|id| captain.terminal_id.as_deref() == Some(id))
                    || ship_slug.is_some_and(|slug| captain.ship_slug == slugify_ship(slug))
            })
            .ok_or("rename_captain: no matching Captain is registered")?;
        if captain.display_name == display_name {
            return Ok(captain.clone());
        }
        captain.display_name = display_name;
        let result = captain.clone();
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(result)
    }

    /// Reconcile legacy and stale Crew placement against one authoritative
    /// Workspace report. Exact owned placement is retained, one unambiguous owned
    /// destination is rehomed, and every other case fails closed as
    /// `needsAssignment` without selecting a foreign Workspace.
    pub fn reconcile_crew_workspaces(&self, tabs: &mut [TabRecord]) -> Result<bool, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let changed = reconcile_crew_workspace_candidates(&mut g.captains, tabs)?;
        if changed {
            g.seq = g.seq.saturating_add(1);
            self.commit_mutation(g, previous)?;
        }
        Ok(changed)
    }

    /// Enrich a spawned Crew reference with its durable task, harness, checkout,
    /// and authoritative Powder card/run binding.
    pub fn bind_crew_context(
        &self,
        captain_session_id: &str,
        crew_session_id: &str,
        task: &str,
        harness: &str,
        worktree_path: Option<&str>,
        branch: Option<&str>,
        powder_work: PowderWorkBinding,
    ) -> Result<CrewRef, String> {
        self.bind_crew_context_exact(
            captain_session_id,
            crew_session_id,
            task,
            harness,
            worktree_path,
            branch,
            None,
            powder_work,
            None,
            None,
        )
    }

    pub(super) fn bind_crew_context_exact(
        &self,
        captain_session_id: &str,
        crew_session_id: &str,
        task: &str,
        harness: &str,
        worktree_path: Option<&str>,
        branch: Option<&str>,
        workspace_tab_id: Option<&str>,
        powder_work: PowderWorkBinding,
        expected_dispatch: Option<&DispatchAuthority>,
        expected_bind: Option<DispatchBindAuthority>,
    ) -> Result<CrewRef, String> {
        if task.trim().is_empty() {
            return Err("crew task must not be empty".into());
        }
        validate_harness_name(harness.trim(), "Crew harness")?;
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        if let (Some(expected_dispatch), Some(expected_bind)) = (expected_dispatch, expected_bind) {
            if captain_session_id
                != expected_dispatch
                    .captain
                    .terminal_id
                    .as_deref()
                    .unwrap_or("")
            {
                return Err("acl: dispatch authority changed; refusing Crew bind".into());
            }
            let captain = g
                .captains
                .iter()
                .find(|captain| captain.terminal_id.as_deref() == Some(captain_session_id))
                .ok_or_else(|| format!("unknown Captain session '{captain_session_id}'"))?;
            let current = g.authority_generations.scoped(
                self.authority_epoch,
                &expected_dispatch.captain.ship_slug,
                crew_session_id,
                &expected_dispatch.project.project_id,
            );
            if current.captain != expected_dispatch.generation.captain
                || current.project != expected_dispatch.generation.project
                || current.registry_epoch != expected_dispatch.generation.registry_epoch
                || current.crew != expected_bind.crew_generation
                || CaptainAuthorityProjection::from(captain)
                    != CaptainAuthorityProjection::from(&expected_dispatch.captain)
                || captain.project_id.as_deref()
                    != Some(expected_dispatch.project.project_id.as_str())
            {
                return Err("acl: dispatch authority changed; refusing Crew bind".into());
            }
        }
        let captain = g
            .captains
            .iter_mut()
            .find(|captain| captain.terminal_id.as_deref() == Some(captain_session_id))
            .ok_or_else(|| format!("unknown Captain session '{captain_session_id}'"))?;
        let crew = captain
            .crew
            .iter_mut()
            .find(|crew| crew.terminal_id == crew_session_id)
            .ok_or_else(|| format!("unknown Crew session '{crew_session_id}'"))?;
        let provider_changed = crew.provider.as_deref() != Some(harness);
        crew.task = Some(task.trim().to_string());
        crew.harness = Some(harness.to_string());
        crew.provider = Some(harness.to_string());
        crew.harness_permission = None;
        crew.t_hub_capability = None;
        if harness != "claude" {
            crew.claude_uuid = None;
        }
        if provider_changed {
            crew.provider_session_id = None;
            crew.conversation_id = None;
        }
        crew.worktree_path = worktree_path.map(str::to_string);
        crew.branch = branch.map(str::to_string);
        crew.workspace_tab_id = workspace_tab_id.map(str::to_string);
        crew.powder_work = Some(powder_work);
        let result = crew.clone();
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(result)
    }

    /// Persist the two independent permission axes only after provider-native
    /// process evidence has verified the effective Harness mode.
    pub fn record_crew_launch_attestation(
        &self,
        crew_session_id: &str,
        attestation: HarnessPermissionAttestation,
        t_hub_capability: &str,
    ) -> Result<CrewRef, String> {
        if !matches!(t_hub_capability, "read" | "control") {
            return Err("Crew T-Hub capability is invalid".into());
        }
        if attestation.permission != CREW_DEFAULT_PERMISSION {
            return Err(format!(
                "Crew Harness permission '{}' conflicts with the fleet default '{}'",
                attestation.permission, CREW_DEFAULT_PERMISSION
            ));
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let crew = g
            .captains
            .iter_mut()
            .flat_map(|captain| captain.crew.iter_mut())
            .find(|crew| crew.terminal_id == crew_session_id)
            .ok_or_else(|| format!("unknown Crew session '{crew_session_id}'"))?;
        if !matches!(crew.state, CrewState::Active) {
            return Err(format!(
                "Crew session '{crew_session_id}' is not active; refusing launch attestation"
            ));
        }
        let expected_provider = attestation.provider.as_provider();
        if crew.provider.as_deref() != Some(expected_provider)
            || crew.harness.as_deref() != Some(expected_provider)
        {
            return Err(format!(
                "Crew session '{crew_session_id}' provider binding conflicts with launch attestation"
            ));
        }
        if !crew
            .powder_work
            .as_ref()
            .is_some_and(|work| matches!(work.state, PowderWorkState::Active))
        {
            return Err(format!(
                "Crew session '{crew_session_id}' has no active authoritative Powder binding"
            ));
        }
        if crew.harness_permission.is_some() || crew.t_hub_capability.is_some() {
            return Err(format!(
                "Crew session '{crew_session_id}' already has launch permission evidence"
            ));
        }
        crew.harness_permission = Some(attestation.permission);
        crew.t_hub_capability = Some(t_hub_capability.to_string());
        let result = crew.clone();
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(result)
    }

    pub fn checkpoint(
        &self,
        captain_session_id: Option<&str>,
        ship_slug: Option<&str>,
        crew_session_id: Option<&str>,
        conversation_id: Option<&str>,
        resume_point: Option<&str>,
    ) -> Result<CaptainRecord, String> {
        if captain_session_id.is_none() && ship_slug.is_none() {
            return Err("captain_checkpoint requires 'captainSessionId' or 'shipSlug'".into());
        }
        if conversation_id.is_none() && resume_point.is_none() {
            return Err("captain_checkpoint requires 'conversationId' or 'resumePoint'".into());
        }
        let conversation_id = conversation_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let resume_point = resume_point
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if conversation_id.is_none() && resume_point.is_none() {
            return Err("captain_checkpoint values must not be empty".into());
        }

        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let captain = g
            .captains
            .iter_mut()
            .find(|captain| {
                captain_session_id.is_some_and(|id| captain.terminal_id.as_deref() == Some(id))
                    || ship_slug.is_some_and(|slug| captain.ship_slug == slug)
            })
            .ok_or("captain_checkpoint: no matching Captain is registered")?;
        if let Some(crew_session_id) = crew_session_id {
            let crew = captain
                .crew
                .iter_mut()
                .find(|crew| crew.terminal_id == crew_session_id)
                .ok_or_else(|| {
                    format!(
                        "captain_checkpoint: Crew session '{crew_session_id}' is not on ship '{}'",
                        captain.ship_slug
                    )
                })?;
            if let Some(value) = conversation_id {
                let provider = crew
                    .provider
                    .as_deref()
                    .or(crew.harness.as_deref())
                    .unwrap_or("claude")
                    .to_string();
                crew.provider = Some(provider.clone());
                crew.harness.get_or_insert_with(|| provider.clone());
                crew.conversation_id = Some(value.to_string());
                crew.provider_session_id = Some(value.to_string());
                crew.claude_uuid = (provider == "claude").then(|| value.to_string());
            }
            if let Some(value) = resume_point {
                crew.resume_point = Some(value.to_string());
            }
        } else {
            if let Some(value) = conversation_id {
                let provider = captain
                    .provider
                    .as_deref()
                    .or(captain.harness.as_deref())
                    .unwrap_or("claude")
                    .to_string();
                captain.provider = Some(provider.clone());
                captain.harness.get_or_insert_with(|| provider.clone());
                captain.conversation_id = Some(value.to_string());
                captain.provider_session_id = Some(value.to_string());
                captain.claude_uuid = (provider == "claude").then(|| value.to_string());
            }
            if let Some(value) = resume_point {
                captain.resume_point = Some(value.to_string());
            }
        }
        let result = captain.clone();
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(result)
    }

    /// The fleet identity a terminal id currently POINTS (item-2 §2.1: keyed on the
    /// mutable `terminal_id`, was `captain_session_id`). Used by the fleet notifier
    /// to label a transition as belonging to a captain (and name its ship). A record
    /// whose terminal is orphaned (`terminal_id: None`) is intentionally NOT returned
    /// here - it has no live pointer to attribute a status edge to.
    pub fn captain_for_session(&self, session_id: &str) -> Option<CaptainRecord> {
        self.lock()
            .captains
            .iter()
            .find(|c| c.terminal_id.as_deref() == Some(session_id))
            .cloned()
    }

    /// Does record `c` hold the durable key for `(role, slug)`? For a Captain the key
    /// is `ship_slug`; for the Cortana singleton it is the ROLE (D1 - uniqueness on
    /// the role, not a reserved slug).
    pub(super) fn key_matches(c: &FleetIdentity, role: FleetRole, slug: &str) -> bool {
        match role {
            FleetRole::Cortana => c.role == FleetRole::Cortana,
            FleetRole::Captain => c.role == FleetRole::Captain && c.ship_slug == slug,
        }
    }

    pub(super) fn set_provider_identity(
        captain: &mut FleetIdentity,
        provider: Option<&str>,
        provider_session_id: Option<&str>,
    ) {
        let Some(provider) = provider else { return };
        let provider_changed = captain.provider.as_deref() != Some(provider);
        captain.provider = Some(provider.to_string());
        captain.harness = Some(provider.to_string());
        if let Some(provider_session_id) = provider_session_id {
            captain.provider_session_id = Some(provider_session_id.to_string());
            captain.claude_uuid = (provider == "claude").then(|| provider_session_id.to_string());
            captain.conversation_id = Some(provider_session_id.to_string());
        } else if provider_changed {
            captain.provider_session_id = None;
            captain.claude_uuid = None;
            captain.conversation_id = None;
        } else if provider != "claude" {
            captain.claude_uuid = None;
        }
    }

    pub(super) fn apply_claim_binding(
        captain: &mut FleetIdentity,
        binding: Option<(&str, &str, &str)>,
    ) -> bool {
        let Some((project_id, assignment, harness)) = binding else {
            return false;
        };
        let assignment_id = assignment_id_for(Some(project_id), &captain.ship_slug);
        let changed = captain.project_id.as_deref() != Some(project_id)
            || captain.assignment_id != assignment_id
            || captain.assignment.as_deref() != Some(assignment)
            || captain.harness.as_deref() != Some(harness);
        captain.project_id = Some(project_id.to_string());
        captain.assignment_id = assignment_id;
        captain.assignment = Some(assignment.to_string());
        captain.harness = Some(harness.to_string());
        changed
    }

    /// Bump seq + persist iff `changed`, then package the [`ClaimOutcome`]. The guard
    /// is consumed here so the (potentially slow) disk write runs AFTER `inner` is
    /// dropped (Incident-D discipline).
    pub(super) fn commit_claim(
        &self,
        mut g: std::sync::MutexGuard<'_, CaptainsInner>,
        previous: CaptainsInner,
        record: FleetIdentity,
        disposition: ClaimDisposition,
        mut changed: bool,
    ) -> Result<ClaimOutcome, String> {
        if let Some(position) = g.pending_fleet_operations.iter().position(|operation| {
            matches!(
                &operation.payload,
                PendingFleetOperationPayload::CommissionCaptain { terminal_id, .. }
                    if record.terminal_id.as_deref() == Some(terminal_id.as_str())
            )
        }) {
            let PendingFleetOperationPayload::CommissionCaptain {
                terminal_id,
                project_id,
                assignment,
                ship_slug,
                harness,
                identity_id,
            } = g.pending_fleet_operations[position].payload.clone()
            else {
                unreachable!("position matched a commission operation")
            };
            if identity_id.is_none()
                || record.terminal_id.as_deref() != Some(terminal_id.as_str())
                || record.project_id.as_deref() != Some(project_id.as_str())
                || record.assignment.as_deref() != Some(assignment.as_str())
                || record.ship_slug != ship_slug
                || record.harness.as_deref() != Some(harness.as_str())
            {
                return Err(
                    "commission_captain: claimed identity does not match its durable intent".into(),
                );
            }
            for workspace in &mut g.workspaces {
                workspace.tile_ids.retain(|tile| tile != &terminal_id);
            }
            g.workspaces
                .iter_mut()
                .find(|workspace| workspace.id == CAPTAIN_WORKSPACE_ID)
                .expect("durable registry always has Captain Workspace")
                .tile_ids
                .push(terminal_id.clone());
            g.retired_fleet_tile_ids.retain(|tile| tile != &terminal_id);
            g.pending_fleet_operations.remove(position);
            changed = true;
        }
        if changed {
            g.seq += 1;
            let terminal_id = record.terminal_id.as_deref();
            let excluded_since = terminal_id.and_then(|terminal_id| {
                self.workspace_projection_exclusions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(terminal_id)
            });
            if let Err(error) = self.commit_mutation(g, previous) {
                if let Some(excluded_since) = excluded_since {
                    self.workspace_projection_exclusions
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(
                            terminal_id
                                .expect("excluded terminal is present")
                                .to_string(),
                            excluded_since,
                        );
                }
                return Err(error);
            }
        } else if let Some(terminal_id) = record.terminal_id.as_deref() {
            self.clear_workspace_projection_exclusion(terminal_id);
        }
        Ok(ClaimOutcome {
            record,
            disposition,
        })
    }

    /// Claim (or re-key / rebind) an identity on the DURABLE ship/role key (item-2
    /// §2.1/§2.2). This replaces the terminal-id-primary upsert. The collision matrix
    /// (§2.2, "defined once"):
    ///   - key FREE                          -> `Created` (or a same-terminal
    ///     re-designation moves this session's record to the new key);
    ///   - key held by the SAME terminal     -> `Refreshed` (idempotent);
    ///   - key held by an `Orphaned`/`Vacant`
    ///     record (or an un-pointed one)      -> `ReadoptedOrphan` (D4: the ship-slug
    ///     re-claim IS the always-available auto-rebind trigger; crew re-adopted);
    ///   - key held by a DIFFERENT terminal
    ///     that is UNAMBIGUOUSLY dead         -> `AutoReleasedDead` (the R-H2 deadlock
    ///     clearer - transfer ONLY on `tmux::has_session == false`, R1);
    ///   - key held by a DIFFERENT terminal
    ///     that is ALIVE                      -> rejected ("already captained by a
    ///     LIVE session - release first"). No soft signal ever seizes a live ship.
    ///
    /// LOCK DISCIPLINE (MED-3 / Incident-D): the incumbent liveness probe
    /// (`is_terminal_dead`, a tmux subprocess) is a COMPARE-AND-SWAP - snapshot the
    /// colliding record under `inner`, RELEASE `inner`, probe with NO lock held,
    /// re-acquire `inner` and RE-VALIDATE the incumbent is unchanged before
    /// releasing/rebinding; if the window changed, recompute. tmux is NEVER called
    /// while `inner` is held.
    ///
    /// `is_terminal_dead(tile)` is `|t| tmux::is_definitively_gone(session_liveness(target(t)))`
    /// in production (the SOLE transfer-grade signal, R1): true ONLY for a completed
    /// probe reporting the session absent, so a timed-out/ambiguous probe never
    /// seizes a live ship. Tests inject a deterministic predicate.
    pub fn claim_provider(
        &self,
        terminal_id: &str,
        ship_slug: Option<&str>,
        role: FleetRole,
        provider: Option<&str>,
        provider_session_id: Option<&str>,
        workspace_tab_ids: Vec<String>,
        is_terminal_dead: &dyn Fn(&str) -> bool,
        crew_liveness: &dyn Fn(&str) -> tmux::SessionLiveness,
    ) -> Result<ClaimOutcome, String> {
        self.claim_provider_with_binding(
            terminal_id,
            ship_slug,
            role,
            provider,
            provider_session_id,
            workspace_tab_ids,
            None,
            is_terminal_dead,
            crew_liveness,
        )
    }

    pub(super) fn claim_provider_with_binding(
        &self,
        terminal_id: &str,
        ship_slug: Option<&str>,
        role: FleetRole,
        provider: Option<&str>,
        provider_session_id: Option<&str>,
        workspace_tab_ids: Vec<String>,
        binding: Option<(&str, &str, &str)>,
        is_terminal_dead: &dyn Fn(&str) -> bool,
        crew_liveness: &dyn Fn(&str) -> tmux::SessionLiveness,
    ) -> Result<ClaimOutcome, String> {
        if terminal_id.trim().is_empty() {
            return Err("claim_captain requires a non-empty 'captainSessionId'".into());
        }
        validate_runtime_identity(
            "Captain claim",
            None,
            provider,
            provider_session_id,
            (provider == Some("claude"))
                .then_some(provider_session_id)
                .flatten(),
            true,
        )?;
        // The Cortana singleton always occupies the reserved slug; a Captain slugifies
        // its ship name, falling back to `ship-<terminal>` so a UI pin always claims
        // something addressable.
        let slug = match role {
            FleetRole::Cortana => CORTANA_SLUG.to_string(),
            FleetRole::Captain => ship_slug
                .map(slugify_ship)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| slugify_ship(&format!("ship-{terminal_id}"))),
        };
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let previous = self.lock().clone();
        if let Some((project_id, assignment, harness)) = binding {
            if assignment.trim().is_empty() || harness.trim().is_empty() {
                return Err("Captain binding requires a non-empty assignment and harness".into());
            }
            validate_harness_name(harness, "Captain harness")?;
            if !previous
                .projects
                .iter()
                .any(|project| project.project_id == project_id)
            {
                return Err(format!("unknown projectId '{project_id}'"));
            }
        }
        if let Some(existing) = previous
            .captains
            .iter()
            .find(|captain| captain.terminal_id.as_deref() == Some(terminal_id))
        {
            if existing.ship_slug != slug && existing.project_id.is_some() {
                return Err(format!(
                    "claim_captain: project-bound Captain '{}' cannot be redesignated as ship '{slug}'; release the existing Captain explicitly before reusing its terminal or slug",
                    existing.ship_slug
                ));
            }
        }

        for _attempt in 0..CLAIM_CAS_ATTEMPTS {
            // Phase 1 (under `inner`): decide whether an incumbent liveness probe is
            // required, and snapshot WHICH terminal to probe. Release the lock before
            // any tmux I/O.
            let probe: Option<String> = {
                let g = self.lock();
                match g
                    .captains
                    .iter()
                    .find(|c| Self::key_matches(c, role, &slug))
                {
                    Some(h) => {
                        let same_terminal = h.terminal_id.as_deref() == Some(terminal_id);
                        if same_terminal {
                            None
                        } else {
                            match (&h.terminal_id, &h.state) {
                                // An ACTIVE incumbent on a different terminal is the
                                // only case that needs a liveness probe to decide
                                // transfer-vs-reject.
                                (Some(other), ClaimState::Active) => Some(other.clone()),
                                // Un-pointed or non-Active (Orphaned/Vacant) => a
                                // ship-slug re-adoption; no probe.
                                _ => None,
                            }
                        }
                    }
                    None => None,
                }
            };

            // Phase 2 (NO lock held): probe incumbent liveness (Incident-D / MED-3).
            let incumbent_dead = probe.as_deref().map(is_terminal_dead);

            // Phase 3 (re-acquire `inner`): re-validate then mutate.
            let mut g = self.lock();
            for workspace_id in &workspace_tab_ids {
                if let Some(owner) = g.captains.iter().find(|captain| {
                    captain.terminal_id.as_deref() != Some(terminal_id)
                        && !Self::key_matches(captain, role, &slug)
                        && captain
                            .workspace_tab_ids
                            .iter()
                            .any(|owned| owned == workspace_id)
                }) {
                    return Err(format!(
                        "claim_captain: Work Workspace '{workspace_id}' is already owned by Captain '{}'",
                        owner.ship_slug
                    ));
                }
            }
            let holder_pos = g
                .captains
                .iter()
                .position(|c| Self::key_matches(c, role, &slug));

            // Re-validate the probe assumption still holds; if the incumbent moved
            // under the window, recompute from scratch.
            if let Some(probed) = &probe {
                let still = holder_pos.is_some_and(|i| {
                    let h = &g.captains[i];
                    h.terminal_id.as_deref() == Some(probed.as_str())
                        && h.state == ClaimState::Active
                });
                if !still {
                    drop(g);
                    continue;
                }
            }

            match holder_pos {
                None => {
                    // Durable key FREE. If THIS terminal already captains a different
                    // ship, this is a re-designation: move its record to the new key
                    // (preserving crew). Otherwise a fresh claim.
                    if let Some(mi) = g
                        .captains
                        .iter()
                        .position(|c| c.terminal_id.as_deref() == Some(terminal_id))
                    {
                        let c = &mut g.captains[mi];
                        c.ship_slug = slug.clone();
                        c.role = role;
                        Self::set_provider_identity(c, provider, provider_session_id);
                        if !workspace_tab_ids.is_empty() {
                            c.workspace_tab_ids = workspace_tab_ids;
                        }
                        c.state = ClaimState::Active;
                        Self::apply_claim_binding(c, binding);
                        let rec = c.clone();
                        return self.commit_claim(
                            g,
                            previous.clone(),
                            rec,
                            ClaimDisposition::Refreshed,
                            true,
                        );
                    }
                    let mut rec = FleetIdentity {
                        ship_slug: slug.clone(),
                        assignment_id: assignment_id_for(None, &slug),
                        display_name: slug.clone(),
                        role,
                        claude_uuid: (provider == Some("claude"))
                            .then(|| provider_session_id.map(str::to_string))
                            .flatten(),
                        provider: provider.map(str::to_string),
                        provider_session_id: provider_session_id.map(str::to_string),
                        terminal_id: Some(terminal_id.to_string()),
                        project_id: None,
                        assignment: None,
                        harness: provider.map(str::to_string),
                        conversation_id: provider_session_id.map(str::to_string),
                        resume_point: None,
                        workspace_tab_ids,
                        crew: Vec::new(),
                        state: ClaimState::Active,
                    };
                    Self::apply_claim_binding(&mut rec, binding);
                    g.captains.push(rec.clone());
                    return self.commit_claim(
                        g,
                        previous.clone(),
                        rec,
                        ClaimDisposition::Created,
                        true,
                    );
                }
                Some(i) => {
                    // Idempotent refresh by the SAME terminal.
                    if g.captains[i].terminal_id.as_deref() == Some(terminal_id) {
                        let c = &mut g.captains[i];
                        let tabs_change = !workspace_tab_ids.is_empty()
                            && c.workspace_tab_ids != workspace_tab_ids;
                        let provider_change = provider.is_some()
                            && (c.provider.as_deref() != provider
                                || provider_session_id.is_some()
                                    && c.provider_session_id.as_deref() != provider_session_id);
                        let reactivate = c.state != ClaimState::Active;
                        if tabs_change {
                            c.workspace_tab_ids = workspace_tab_ids;
                        }
                        if provider_change {
                            Self::set_provider_identity(c, provider, provider_session_id);
                        }
                        if reactivate {
                            c.state = ClaimState::Active;
                            Self::readopt_orphaned_crew(c, crew_liveness);
                        }
                        let binding_change = Self::apply_claim_binding(c, binding);
                        let changed =
                            tabs_change || provider_change || reactivate || binding_change;
                        let rec = c.clone();
                        return self.commit_claim(
                            g,
                            previous.clone(),
                            rec,
                            ClaimDisposition::Refreshed,
                            changed,
                        );
                    }

                    // A DIFFERENT terminal holds the key. Classify per the matrix.
                    let orphan_or_vacant = g.captains[i].terminal_id.is_none()
                        || matches!(
                            g.captains[i].state,
                            ClaimState::Orphaned { .. } | ClaimState::Vacant
                        );
                    let disposition = if orphan_or_vacant {
                        ClaimDisposition::ReadoptedOrphan
                    } else {
                        // Active incumbent on a different terminal, no UUID match:
                        // transfer ONLY on the unambiguous-death signal (R1).
                        match incumbent_dead {
                            Some(true) => ClaimDisposition::AutoReleasedDead,
                            Some(false) => {
                                let other = g.captains[i].terminal_id.clone().unwrap_or_default();
                                return Err(format!(
                                    "claim_captain: ship '{slug}' is already captained by a \
                                     LIVE session '{other}' (release_captain it first - one \
                                     captain per ship)"
                                ));
                            }
                            // Probe missing/stale for this now-Active incumbent (a
                            // race made it Active under the window): recompute.
                            None => {
                                drop(g);
                                continue;
                            }
                        }
                    };

                    // Rebind the pointer, re-activate, re-adopt orphaned crew.
                    let c = &mut g.captains[i];
                    c.terminal_id = Some(terminal_id.to_string());
                    Self::set_provider_identity(c, provider, provider_session_id);
                    if !workspace_tab_ids.is_empty() {
                        c.workspace_tab_ids = workspace_tab_ids;
                    }
                    c.state = ClaimState::Active;
                    Self::readopt_orphaned_crew(c, crew_liveness);
                    Self::apply_claim_binding(c, binding);
                    let rec = c.clone();
                    return self.commit_claim(g, previous.clone(), rec, disposition, true);
                }
            }
        }
        Err(format!(
            "claim_captain: ship '{slug}' claim was contended across {CLAIM_CAS_ATTEMPTS} \
             attempts - retry"
        ))
    }

    /// Compatibility entry point for legacy Claude callers and persisted tests.
    /// New multi-harness paths must use `claim_provider` explicitly.
    pub fn claim(
        &self,
        terminal_id: &str,
        ship_slug: Option<&str>,
        role: FleetRole,
        claude_uuid: Option<&str>,
        workspace_tab_ids: Vec<String>,
        is_terminal_dead: &dyn Fn(&str) -> bool,
        crew_liveness: &dyn Fn(&str) -> tmux::SessionLiveness,
    ) -> Result<ClaimOutcome, String> {
        self.claim_provider(
            terminal_id,
            ship_slug,
            role,
            Some("claude"),
            claude_uuid,
            workspace_tab_ids,
            is_terminal_dead,
            crew_liveness,
        )
    }

    /// Re-adopt a resuming supervisor's Orphaned crew, GATED on a liveness probe
    /// (audit BUG-1: the old form blind-flipped EVERY Orphaned crew to `Active`
    /// with no probe, so a captain resuming after its crew had died resurrected
    /// dead tiles). Per-crew, keyed on the same de-conflated two-tier liveness the
    /// incumbent transfer uses (PR#58):
    ///   - `Alive`   -> `Active`  (the worker is really there, re-adopt it);
    ///   - `Gone`    -> `Removed` (a DEFINITIVE absent probe: the worker died while
    ///     orphaned; mark it, don't resurrect it);
    ///   - `Unknown` -> left `Orphaned` (an ambiguous/timed-out probe is NEVER
    ///     seized - it stays re-adoptable on the next resume once liveness is
    ///     definite; item-2 two-tier invariant: ambiguous is never acted on).
    /// A crew whose OWN tile already went `Removed` is untouched (the worker is
    /// gone for good).
    ///
    /// `crew_liveness` MUST be a PURE lookup (this runs while the registry `inner`
    /// lock is held; MED-3/Incident-D forbids tmux I/O under the lock). The real
    /// caller precomputes a liveness map lock-free and passes a map-reading
    /// closure; tests inject a deterministic one.
    pub(super) fn readopt_orphaned_crew(
        c: &mut FleetIdentity,
        crew_liveness: &dyn Fn(&str) -> tmux::SessionLiveness,
    ) {
        let now = now_ms();
        for cr in c.crew.iter_mut() {
            if matches!(cr.state, CrewState::Orphaned { .. }) {
                match crew_liveness(&cr.terminal_id) {
                    tmux::SessionLiveness::Alive => cr.state = CrewState::Active,
                    tmux::SessionLiveness::Gone => cr.state = CrewState::Removed { since: now },
                    tmux::SessionLiveness::Unknown => {}
                }
            }
        }
    }

    /// Release a captaincy, addressed by terminal id OR ship slug (or the Cortana
    /// reserved slug). Unknown target is an error (strict - a silent no-op is how
    /// state drifts). If live Crew or unresolved Powder work remain, the claim
    /// transitions to `Vacant` (re-claimable by a new captain of the same ship,
    /// complete Crew history preserved) rather than hard-removing. A claim whose
    /// Crew are all Removed and carry no unresolved Powder obligation is removed
    /// outright (§3.1 release row). Returns the record as it stands after release.
    pub fn release(&self, target: &str) -> Result<CaptainRecord, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let Some(idx) = g
            .captains
            .iter()
            .position(|c| c.terminal_id.as_deref() == Some(target) || c.ship_slug == target)
        else {
            return Err(format!(
                "release_captain: no claim matches '{target}' (list_captains shows \
                 terminalId + shipSlug of every claim)"
            ));
        };
        if g.captains[idx].role == FleetRole::Cortana {
            return Err(
                "release_captain: Cortana is a durable backend-owned singleton; use the dedicated reconciliation or replacement operation"
                    .into(),
            );
        }
        let requires_retention = g.captains[idx].crew.iter().any(|crew| {
            !matches!(crew.state, CrewState::Removed { .. })
                || crew.powder_work.as_ref().is_some_and(|work| {
                    matches!(
                        work.state,
                        PowderWorkState::Active | PowderWorkState::CompletionPending { .. }
                    )
                })
        });
        let released = if requires_retention {
            let c = &mut g.captains[idx];
            c.state = ClaimState::Vacant;
            c.terminal_id = None;
            c.provider_session_id = None;
            c.conversation_id = None;
            c.claude_uuid = None;
            c.clone()
        } else {
            g.captains.remove(idx)
        };
        g.seq += 1;
        self.commit_mutation(g, previous)?;
        Ok(released)
    }

    /// Record a spawned crew session under its spawner's SHIP (item-2 §2.3: crew
    /// membership is a property of the ship, keyed via the spawner's terminal
    /// pointer). Returns true (revision bumped) when the spawner holds a claim and
    /// the crew was newly added or REACTIVATED (a reused tile id whose prior ref was
    /// Removed/Orphaned); false when the spawner has no claim (the spawn still
    /// proceeds) or the crew is already an Active member. The `CrewRef`'s
    /// `claude_uuid` is `None` here (the crew's own SessionStart has not fired yet,
    /// MED-7) and is backfilled later via [`backfill_uuid`](Self::backfill_uuid).
    pub fn record_crew(&self, spawned_by: &str, crew_session_id: &str) -> Result<bool, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let Some(c) = g
            .captains
            .iter_mut()
            .find(|c| c.terminal_id.as_deref() == Some(spawned_by))
        else {
            return Ok(false);
        };
        if let Some(existing) = c
            .crew
            .iter_mut()
            .find(|cr| cr.terminal_id == crew_session_id)
        {
            if matches!(existing.state, CrewState::Active) {
                drop(g);
                self.clear_workspace_projection_exclusion(crew_session_id);
                return Ok(false);
            }
            existing.state = CrewState::Active;
        } else {
            c.crew.push(CrewRef::new(crew_session_id));
        }
        g.seq += 1;
        let excluded_since = self
            .workspace_projection_exclusions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(crew_session_id);
        if let Err(error) = self.commit_mutation(g, previous) {
            if let Some(excluded_since) = excluded_since {
                self.workspace_projection_exclusions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(crew_session_id.to_string(), excluded_since);
            }
            return Err(error);
        }
        Ok(true)
    }

    /// Remove a Crew reference created by a dispatch transaction that failed
    /// before work started. Normal terminal death remains a retained Removed
    /// record; this method is only rollback for an uncommitted dispatch.
    pub fn rollback_crew(&self, crew_session_id: &str) -> Result<bool, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let mut changed = false;
        for captain in &mut g.captains {
            let before = captain.crew.len();
            captain
                .crew
                .retain(|crew| crew.terminal_id != crew_session_id);
            changed |= captain.crew.len() != before;
        }
        if changed {
            g.seq = g.seq.saturating_add(1);
            self.commit_mutation(g, previous)?;
        }
        Ok(changed)
    }

    pub fn mark_crew_cleanup_pending(&self, crew_session_id: &str) -> Result<bool, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let Some(crew) = g
            .captains
            .iter_mut()
            .flat_map(|captain| captain.crew.iter_mut())
            .find(|crew| crew.terminal_id == crew_session_id)
        else {
            return Ok(false);
        };
        if matches!(crew.state, CrewState::CleanupPending { .. }) {
            return Ok(false);
        }
        crew.state = CrewState::CleanupPending { since: now_ms() };
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)?;
        Ok(true)
    }

    /// Backfill the Claude continuity anchor for a tile once the StatusBridge
    /// resolves it (item-2 §2.3/MED-7 + §2.1: the async-resolved anchor). Sets
    /// `claude_uuid` on a captain record whose `terminal_id` matches, or on a
    /// `CrewRef` whose `terminal_id` matches, but ONLY when currently `None` (never
    /// overwrites a resolved anchor). Returns true (revision bumped) if it filled
    /// one. A pure enrichment - it changes no ownership.
    pub fn backfill_uuid(&self, tile: &str, uuid: &str) -> Result<bool, String> {
        if tile.is_empty() || uuid.is_empty() {
            return Ok(false);
        }
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let mut changed = false;
        for c in g.captains.iter_mut() {
            if c.terminal_id.as_deref() == Some(tile)
                && c.provider
                    .as_deref()
                    .is_none_or(|provider| provider == "claude")
                && c.claude_uuid.is_none()
            {
                c.claude_uuid = Some(uuid.to_string());
                c.provider = Some("claude".to_string());
                c.provider_session_id = Some(uuid.to_string());
                c.conversation_id.get_or_insert_with(|| uuid.to_string());
                changed = true;
            }
            for cr in c.crew.iter_mut() {
                if cr.terminal_id == tile
                    && cr
                        .provider
                        .as_deref()
                        .is_none_or(|provider| provider == "claude")
                    && cr.claude_uuid.is_none()
                {
                    cr.claude_uuid = Some(uuid.to_string());
                    cr.provider = Some("claude".to_string());
                    cr.provider_session_id = Some(uuid.to_string());
                    cr.conversation_id.get_or_insert_with(|| uuid.to_string());
                    changed = true;
                }
            }
        }
        if changed {
            g.seq += 1;
            self.commit_mutation(g, previous)?;
        }
        Ok(changed)
    }

    /// Resolve which SHIP (and, for a supervisor, which role) a terminal id belongs
    /// to (item-2 §2.5/§2.6: the `ship_of` resolver the cross-ship ownership ACL and
    /// per-session attribution key on). A supervisor terminal resolves to its own
    /// ship+role; a crew tile resolves to its ship (skipping a `Removed` ref - that
    /// worker is gone). `None` if the tile belongs to no ship.
    pub fn ship_of(&self, tile: &str) -> Option<ShipMembership> {
        let g = self.lock();
        if let Some(c) = g
            .captains
            .iter()
            .find(|c| c.terminal_id.as_deref() == Some(tile))
        {
            return Some(ShipMembership::Supervisor {
                ship_slug: c.ship_slug.clone(),
                role: c.role,
            });
        }
        for c in g.captains.iter() {
            if c.crew
                .iter()
                .any(|cr| cr.terminal_id == tile && !matches!(cr.state, CrewState::Removed { .. }))
            {
                return Some(ShipMembership::Crew {
                    ship_slug: c.ship_slug.clone(),
                });
            }
        }
        None
    }

    /// Resolve historical ownership only for cleanup of a stopped Crew terminal
    /// that still has durable Powder work. Removed Crew intentionally remain
    /// excluded from [`Self::ship_of`] so this cannot restore ordinary membership
    /// or authorize any lifecycle operation other than exact Powder cleanup.
    pub(super) fn removed_crew_powder_ship(&self, tile: &str) -> Result<Option<String>, String> {
        let g = self.lock();
        let matches = g
            .captains
            .iter()
            .flat_map(|captain| {
                captain
                    .crew
                    .iter()
                    .filter(|crew| {
                        crew.terminal_id == tile
                            && matches!(crew.state, CrewState::Removed { .. })
                            && crew.powder_work.is_some()
                    })
                    .map(|_| captain.ship_slug.clone())
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Ok(None),
            [ship_slug] => Ok(Some(ship_slug.clone())),
            _ => Err(format!(
                "Crew session '{tile}' has ambiguous historical Powder ownership"
            )),
        }
    }

    /// Lifecycle transition for a closed/killed session (item-2 §2.4: death MARKS,
    /// it does not scrub - retiring the old `remove_session` C4 silent-leak). Two
    /// cases, both idempotent:
    ///
    /// - the id is a SUPERVISOR terminal: its record goes `Orphaned{since}`, its
    ///   `terminal_id` clears to `None`, and its Active crew go `Orphaned` under the
    ///   STILL-PRESENT ship record (dead captain -> orphaned crew; dead Cortana ->
    ///   orphaned captains-as-crew). Re-adoptable by a resumed same-key supervisor.
    /// - the id is a CREW tile: that `CrewRef` flips to `Removed{since}` (its own
    ///   worker died; not re-adoptable), retained not scrubbed.
    ///
    /// Records are retained INDEFINITELY (D6); reap timing stays reap-ship's. Returns
    /// true (revision bumped) if anything changed.
    pub fn remove_session(&self, session_id: &str) -> Result<bool, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let now = now_ms();
        let mut changed = false;
        // Case 1: a supervisor's terminal died -> Orphaned, un-pointed, crew orphaned.
        for c in g.captains.iter_mut() {
            if c.terminal_id.as_deref() == Some(session_id) {
                c.state = ClaimState::Orphaned { since: now };
                c.terminal_id = None;
                for cr in c.crew.iter_mut() {
                    if matches!(cr.state, CrewState::Active) {
                        cr.state = CrewState::Orphaned { since: now };
                    }
                }
                changed = true;
            }
        }
        // Case 2: a crew tile's OWN session died -> mark that ref Removed (not scrubbed).
        for c in g.captains.iter_mut() {
            for cr in c.crew.iter_mut() {
                if cr.terminal_id == session_id && !matches!(cr.state, CrewState::Removed { .. }) {
                    cr.state = CrewState::Removed { since: now };
                    changed = true;
                }
            }
        }
        if changed {
            g.seq += 1;
            self.commit_mutation(g, previous)?;
        }
        Ok(changed)
    }

    /// Drop a closed workspace tab from every captain's `workspaceTabIds` (the
    /// registry must never advertise ownership of a tab that no longer exists).
    /// The claim itself survives - a captain can control zero tabs. Returns true
    /// (revision bumped) if anything changed.
    pub fn prune_tab(&self, tab_id: &str) -> Result<bool, String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let mut changed = false;
        for c in g.captains.iter_mut() {
            let before = c.workspace_tab_ids.len();
            c.workspace_tab_ids.retain(|id| id != tab_id);
            changed |= c.workspace_tab_ids.len() != before;
        }
        if changed {
            g.seq += 1;
            self.commit_mutation(g, previous)?;
        }
        Ok(changed)
    }
}

// --- #[cfg(test)] helper impl + Default (folded from control.rs) ---
#[cfg(test)]
impl CaptainsRegistry {
    /// Test convenience preserving the legacy 3-arg `claim` ergonomics: a `Captain`
    /// claim, no UUID hint, and a "nothing is dead" liveness predicate (so a live
    /// incumbent is never auto-released). Tests that exercise the dead-claim /
    /// rebind / Cortana paths call the full 6-arg [`claim`](Self::claim) directly.
    pub(crate) fn claim_test(
        &self,
        terminal_id: &str,
        ship_slug: Option<&str>,
        workspace_tab_ids: Vec<String>,
    ) -> Result<ClaimOutcome, String> {
        self.claim(
            terminal_id,
            ship_slug,
            FleetRole::Captain,
            None,
            workspace_tab_ids,
            &|_| false,
            // Legacy resurrect-all: existing readopt tests predate the liveness
            // gate and assert orphaned crew come back Active. Tests that exercise
            // the Gone/Unknown legs pass an explicit `crew_liveness` to `claim`.
            &|_| tmux::SessionLiveness::Alive,
        )
    }

    #[allow(dead_code)]
    pub(super) fn set_historical_scope_capture_hook(
        &self,
        authenticated: std::sync::mpsc::SyncSender<String>,
        resume: std::sync::mpsc::Receiver<()>,
    ) {
        *self
            .historical_scope_capture_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(HistoricalScopeCaptureHook {
            authenticated,
            resume,
        });
    }

    pub(super) fn test_scoped_authority_generation(
        &self,
        ship_slug: &str,
        crew_session_id: &str,
        project_id: &str,
    ) -> ScopedAuthorityGeneration {
        self.lock().authority_generations.scoped(
            self.authority_epoch,
            ship_slug,
            crew_session_id,
            project_id,
        )
    }

    #[allow(dead_code)]
    pub(super) fn test_remove_captain_and_project(
        &self,
        ship_slug: &str,
        project_id: &str,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        let captain_index = g
            .captains
            .iter()
            .position(|captain| captain.ship_slug == ship_slug)
            .ok_or_else(|| format!("unknown shipSlug '{ship_slug}'"))?;
        let project_index = g
            .projects
            .iter()
            .position(|project| project.project_id == project_id)
            .ok_or_else(|| format!("unknown projectId '{project_id}'"))?;
        g.captains.remove(captain_index);
        g.projects.remove(project_index);
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)
    }

    #[allow(dead_code)]
    pub(super) fn test_restore_captain_and_project(
        &self,
        captain: CaptainRecord,
        project: ProjectRecord,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = self.lock();
        let previous = g.clone();
        if g.captains
            .iter()
            .any(|candidate| candidate.ship_slug == captain.ship_slug)
            || g.projects
                .iter()
                .any(|candidate| candidate.project_id == project.project_id)
        {
            return Err("test authority scope already exists".into());
        }
        g.projects.push(project);
        g.captains.push(captain);
        g.seq = g.seq.saturating_add(1);
        self.commit_mutation(g, previous)
    }

    #[allow(dead_code)]
    pub(super) fn pause_before_historical_scope_capture(&self, crew_session_id: &str) {
        let mut hook = self
            .historical_scope_capture_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(hook) = hook.take() else {
            return;
        };
        hook.authenticated
            .send(crew_session_id.to_string())
            .expect("historical scope observer must still be available");
        hook.resume
            .recv_timeout(Duration::from_secs(4))
            .expect("historical scope capture must be resumed before its deadline");
    }
}

impl Default for CaptainsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// --- Captains data model: roles + claim/crew state + CrewRef (from control.rs) ---
/// The durable org ROLE a fleet identity holds (item-2 §2.1, D1). Cortana is the
/// apex SINGLETON - at most one `Active` across the whole registry - and a Captain
/// maps to exactly one ship. This is the first-class role that RETIRES the
/// `ship: cortana` slug-collision hack: uniqueness is enforced on the role, not on a
/// reserved slug. It is a strict subset of the coarse [`crate::identity::Role`]
/// (which also carries mint-time General/Crew/Unknown) because only a supervisor
/// ever holds a registry claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FleetRole {
    Cortana,
    Captain,
}

impl Default for FleetRole {
    /// Legacy records (no `role` field) default to `Captain`; the load-time
    /// reconciliation then re-seeds the single `ship_slug == "cortana"` incumbent to
    /// `Cortana` (D2/MED-6), so the singleton is seeded from the live incumbent, not
    /// defaulted empty.
    fn default() -> Self {
        FleetRole::Captain
    }
}

impl FleetRole {
    pub fn label(self) -> &'static str {
        match self {
            FleetRole::Cortana => "cortana",
            FleetRole::Captain => "captain",
        }
    }
}

/// The lifecycle state of a claim (item-2 §2.4). Death MARKS, it does not scrub - a
/// dead supervisor's record and crew are RETAINED for re-adoption instead of the
/// silent `retain`-away leak (the old `remove_session` C4 single-point-of-failure).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClaimState {
    /// Live and pointed at a terminal.
    #[default]
    Active,
    /// The supervisor's terminal is UNAMBIGUOUSLY gone (`tmux::has_session` false)
    /// but the durable identity + its crew are retained for re-adoption by a resumed
    /// same-key supervisor. `since` is epoch-ms. Retained INDEFINITELY (D6); reap
    /// timing + the landed-gate stay reap-ship's, not item-2's.
    Orphaned { since: u64 },
    /// Explicitly released while crew remained: re-claimable by a new captain of the
    /// same ship, crew preserved. (A release with NO crew hard-removes instead.)
    Vacant,
}

/// A crew member's lifecycle under its ship (item-2 §2.4). Like [`ClaimState`],
/// crew are marked rather than scrubbed so an orphaned worker is re-adoptable and
/// a dead one is visible to telemetry/reap-ship instead of vanishing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CrewState {
    /// Live under a live captain.
    #[default]
    Active,
    /// The CAPTAIN died: the crew is orphaned-but-retained, re-adopted (→ `Active`)
    /// when a same-ship captain resumes. `since` epoch-ms.
    Orphaned { since: u64 },
    /// The Crew terminal is stopped, but its Powder claim could not be released.
    /// Keep the binding addressable until a later cleanup confirms the release.
    CleanupPending { since: u64 },
    /// A live legacy Crew terminal whose owning Work Workspace cannot be resolved
    /// without guessing. It remains visible but cannot be treated as assigned.
    NeedsAssignment { since: u64 },
    /// The crew's OWN tile died: a terminal marker (NOT re-adoptable - the worker is
    /// gone), retained (not scrubbed) so telemetry/reap-ship still see it. `since`
    /// epoch-ms.
    Removed { since: u64 },
}

/// One crew member of a ship (item-2 §2.3). Crew membership is a property of the
/// SHIP (this ref lives inside the ship's [`FleetIdentity`]), so it follows the ship
/// across a captain migration by construction - no pointer-chasing migration routine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrewRef {
    /// The crew's tile id (a MUTABLE pointer). Membership is keyed on the ship, not
    /// on this pointer.
    pub terminal_id: String,
    /// The crew's Claude continuity anchor. `None` at record time (the crew's own
    /// `SessionStart` has not fired yet - `control.rs` async-backfill window, MED-7)
    /// and BACKFILLED on the first StatusBridge resolution. Never load-bearing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_uuid: Option<String>,
    /// Harness that owns `provider_session_id`. Missing on legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Provider-native conversation id, such as a Codex thread id or Claude UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// Harness-neutral conversation identifier used to resume or reconcile a
    /// replaced Crew conversation. Provider continuity is useful, but never the
    /// durable crew identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// Latest durable handoff boundary for this Crew conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_point: Option<String>,
    /// Human-readable task boundary delegated to this Crew member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Effective provider-native permission mode, persisted only after
    /// authoritative post-launch process evidence verifies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_permission: Option<PermMode>,
    /// T-Hub control-plane capability is a separate authority axis from local
    /// Harness execution permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_hub_capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Exact durable Work Workspace selected by the owning Captain before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_tab_id: Option<String>,
    /// Powder work claimed by this Crew member. T-Hub owns the terminal binding;
    /// Powder remains authoritative for the claim and run lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powder_work: Option<PowderWorkBinding>,
    #[serde(default)]
    pub state: CrewState,
}

impl CrewRef {
    pub(super) fn new(terminal_id: &str) -> Self {
        CrewRef {
            terminal_id: terminal_id.to_string(),
            claude_uuid: None,
            provider: None,
            provider_session_id: None,
            conversation_id: None,
            resume_point: None,
            task: None,
            harness: None,
            harness_permission: None,
            t_hub_capability: None,
            worktree_path: None,
            branch: None,
            workspace_tab_id: None,
            powder_work: None,
            state: CrewState::Active,
        }
    }
}

// --- Captains registry internals: authority machinery + inner state + struct ---
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CaptainAuthorityProjection {
    ship_slug: String,
    assignment_id: String,
    display_name: String,
    role: FleetRole,
    claude_uuid: Option<String>,
    provider: Option<String>,
    provider_session_id: Option<String>,
    terminal_id: Option<String>,
    project_id: Option<String>,
    assignment: Option<String>,
    harness: Option<String>,
    conversation_id: Option<String>,
    resume_point: Option<String>,
    workspace_tab_ids: Vec<String>,
    state: ClaimState,
}

impl From<&CaptainRecord> for CaptainAuthorityProjection {
    fn from(captain: &CaptainRecord) -> Self {
        Self {
            ship_slug: captain.ship_slug.clone(),
            assignment_id: captain.assignment_id.clone(),
            display_name: captain.display_name.clone(),
            role: captain.role,
            claude_uuid: captain.claude_uuid.clone(),
            provider: captain.provider.clone(),
            provider_session_id: captain.provider_session_id.clone(),
            terminal_id: captain.terminal_id.clone(),
            project_id: captain.project_id.clone(),
            assignment: captain.assignment.clone(),
            harness: captain.harness.clone(),
            conversation_id: captain.conversation_id.clone(),
            resume_point: captain.resume_point.clone(),
            workspace_tab_ids: captain.workspace_tab_ids.clone(),
            state: captain.state.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CrewAuthorityProjection {
    ship_slug: String,
    crew: CrewRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectAuthorityProjection {
    project_id: String,
    repo_root: String,
    powder: Option<(String, String)>,
}

impl From<&ProjectRecord> for ProjectAuthorityProjection {
    fn from(project: &ProjectRecord) -> Self {
        Self {
            project_id: project.project_id.clone(),
            repo_root: project.repo_root.clone(),
            powder: project.powder.as_ref().map(|binding| {
                (
                    binding.connection_profile.clone(),
                    binding.repository.clone(),
                )
            }),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct AuthorityGenerationChanges {
    captains: Vec<String>,
    crew: Vec<String>,
    projects: Vec<String>,
}

impl AuthorityGenerationChanges {
    pub(super) fn between(previous: &CaptainsInner, candidate: &CaptainsInner) -> Self {
        let captain_keys = previous
            .captains
            .iter()
            .chain(candidate.captains.iter())
            .map(|captain| captain.ship_slug.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let crew_keys = previous
            .captains
            .iter()
            .chain(candidate.captains.iter())
            .flat_map(|captain| captain.crew.iter())
            .map(|crew| crew.terminal_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let project_keys = previous
            .projects
            .iter()
            .chain(candidate.projects.iter())
            .map(|project| project.project_id.clone())
            .collect::<std::collections::BTreeSet<_>>();

        let captain_projection = |inner: &CaptainsInner, key: &str| {
            inner
                .captains
                .iter()
                .find(|captain| captain.ship_slug == key)
                .map(CaptainAuthorityProjection::from)
        };
        let crew_projection = |inner: &CaptainsInner, key: &str| {
            inner
                .captains
                .iter()
                .flat_map(|captain| {
                    captain
                        .crew
                        .iter()
                        .filter(move |crew| crew.terminal_id == key)
                        .map(move |crew| CrewAuthorityProjection {
                            ship_slug: captain.ship_slug.clone(),
                            crew: crew.clone(),
                        })
                })
                .collect::<Vec<_>>()
        };
        let project_projection = |inner: &CaptainsInner, key: &str| {
            inner
                .projects
                .iter()
                .find(|project| project.project_id == key)
                .map(ProjectAuthorityProjection::from)
        };

        Self {
            captains: captain_keys
                .into_iter()
                .filter(|key| {
                    captain_projection(previous, key) != captain_projection(candidate, key)
                })
                .collect(),
            crew: crew_keys
                .into_iter()
                .filter(|key| crew_projection(previous, key) != crew_projection(candidate, key))
                .collect(),
            projects: project_keys
                .into_iter()
                .filter(|key| {
                    project_projection(previous, key) != project_projection(candidate, key)
                })
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ScopedAuthorityGeneration {
    registry_epoch: u64,
    captain: u64,
    crew: u64,
    project: u64,
}

/// The authority tuple captured before dispatch performs remote I/O.
///
/// This deliberately carries the original Captain and Project values instead of
/// a later snapshot.  A Crew bind may add its own Crew generation, but it must
/// never make a replacement Captain or Project become the expected authority.
#[derive(Clone, Debug)]
pub(super) struct DispatchAuthority {
    captain: CaptainRecord,
    project: ProjectRecord,
    generation: ScopedAuthorityGeneration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DispatchBindAuthority {
    crew_generation: u64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct AuthorityGenerations {
    clock: u64,
    captains: std::collections::HashMap<String, u64>,
    crew: std::collections::HashMap<String, u64>,
    projects: std::collections::HashMap<String, u64>,
}

impl AuthorityGenerations {
    pub(super) fn advance(&mut self, changes: AuthorityGenerationChanges) -> Result<(), String> {
        for key in changes.captains {
            self.clock = self
                .clock
                .checked_add(1)
                .ok_or("Captain authority generation exhausted")?;
            self.captains.insert(key, self.clock);
        }
        for key in changes.crew {
            self.clock = self
                .clock
                .checked_add(1)
                .ok_or("Crew authority generation exhausted")?;
            self.crew.insert(key, self.clock);
        }
        for key in changes.projects {
            self.clock = self
                .clock
                .checked_add(1)
                .ok_or("Project authority generation exhausted")?;
            self.projects.insert(key, self.clock);
        }
        Ok(())
    }

    pub(super) fn scoped(
        &self,
        registry_epoch: u64,
        ship_slug: &str,
        crew_session_id: &str,
        project_id: &str,
    ) -> ScopedAuthorityGeneration {
        ScopedAuthorityGeneration {
            registry_epoch,
            captain: self.captains.get(ship_slug).copied().unwrap_or(0),
            crew: self.crew.get(crew_session_id).copied().unwrap_or(0),
            project: self.projects.get(project_id).copied().unwrap_or(0),
        }
    }
}

#[derive(Clone)]
pub(super) struct CaptainsInner {
    pub(super) captains: Vec<CaptainRecord>,
    pub(super) cortana: crate::cortana_reconcile::CortanaDurableIdentity,
    pub(super) agent_sessions: Vec<AgentSessionRecord>,
    pub(super) agent_checkpoints: Vec<AgentCheckpoint>,
    pub(super) agent_events: Vec<AgentEvent>,
    pub(super) projects: Vec<ProjectRecord>,
    pub(super) workspaces: Vec<FleetWorkspaceRecord>,
    pub(super) pending_fleet_operations: Vec<PendingFleetOperation>,
    pub(super) retired_fleet_tile_ids: Vec<String>,
    pub(super) pending_dispatch_claims: Vec<PendingDispatchClaim>,
    pub(super) pending_dispatch_releases: Vec<PendingDispatchRelease>,
    pub(super) pending_git_initializations: Vec<GitInitIntent>,
    /// Monotonic revision, bumped on every accepted mutation - the same
    /// convergence contract as [`RegistryInner::seq`]. Persisted, so it stays
    /// monotonic across app restarts.
    pub(super) seq: u64,
    /// Internal non-ABA versions for exact historical Powder authority scopes.
    /// These are intentionally not serialized: a process restart destroys every
    /// in-flight request, and the registry epoch makes tokens from another loaded
    /// instance invalid while new requests start from the loaded durable state.
    pub(super) authority_generations: AuthorityGenerations,
}

impl Default for CaptainsInner {
    fn default() -> Self {
        Self {
            captains: Vec::new(),
            cortana: crate::cortana_reconcile::CortanaDurableIdentity::default(),
            agent_sessions: Vec::new(),
            agent_checkpoints: Vec::new(),
            agent_events: Vec::new(),
            projects: Vec::new(),
            workspaces: vec![FleetWorkspaceRecord::captain_workspace()],
            pending_fleet_operations: Vec::new(),
            retired_fleet_tile_ids: Vec::new(),
            pending_dispatch_claims: Vec::new(),
            pending_dispatch_releases: Vec::new(),
            pending_git_initializations: Vec::new(),
            seq: 0,
            authority_generations: AuthorityGenerations::default(),
        }
    }
}

pub(super) static NEXT_AUTHORITY_REGISTRY_EPOCH: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
pub(super) struct DispatchBarrier {
    pub(super) boundary: &'static str,
    pub(super) reached: std::sync::mpsc::SyncSender<&'static str>,
    pub(super) resume: std::sync::mpsc::Receiver<()>,
}

pub(super) fn next_authority_registry_epoch() -> u64 {
    NEXT_AUTHORITY_REGISTRY_EPOCH
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .expect("authority registry epoch exhausted")
}

/// The CORE's authoritative captains registry (captain-chat phase 2).
///
/// Captain identity previously lived in two disconnected places: the UI's
/// localStorage designation and the captain's own ship files. This registry is
/// the source of truth for commissioned Captain authority. Overlay pins remain
/// independent UI view state. Captains claim through the control plane, and every
/// mutation forwards a seq'd [`CaptainsSnapshot`] to the UI like the tab registry.
///
/// Unlike [`TabRegistry`] this IS persistent (the phases doc: "survives restarts
/// server-side; localStorage keeps only view state"): every mutation is written
/// through to `captains.json`, and `load` seeds from it. The write-through happens
/// AFTER the registry lock is dropped (see [`persist`](Self::persist)) so a slow
/// state-file write never wedges a reader on the registry lock.
pub(super) enum AgentDeliveryUpdate {
    Implemented(String),
    Reviewed(crate::agent_session::ReviewEvidence),
    Tested(crate::agent_session::AcceptanceTestEvidence),
    Integrated(crate::agent_session::IntegrationEvidence),
    Packaged(crate::agent_session::ArtifactEvidence),
    Installed(crate::agent_session::InstallationEvidence),
    LiveVerified(crate::agent_session::LiveVerificationEvidence),
}

impl AgentDeliveryUpdate {
    fn apply(self, delivery: &mut crate::agent_session::DeliveryProvenance) -> Result<(), String> {
        match self {
            Self::Implemented(commit) => delivery.record_implementation(commit),
            Self::Reviewed(evidence) => delivery.record_review(evidence),
            Self::Tested(evidence) => delivery.record_acceptance_test(evidence),
            Self::Integrated(evidence) => delivery.record_integration(evidence),
            Self::Packaged(evidence) => delivery.record_artifact(evidence),
            Self::Installed(evidence) => delivery.record_installation(evidence),
            Self::LiveVerified(evidence) => delivery.record_live_verification(evidence),
        }
    }
}

pub struct CaptainsRegistry {
    inner: Mutex<CaptainsInner>,
    /// Unique in-process identity for this loaded registry instance. No request
    /// can carry an authority token through replacement or restart of the store.
    authority_epoch: u64,
    /// Serializes mutations so a candidate is published in memory only after its
    /// durable write succeeds, without racing another accepted mutation.
    pub(super) mutation: Mutex<()>,
    /// Serializes multi-step Captain provisioning so project uniqueness checks,
    /// ship claims, and project binding cannot interleave across requests.
    provision: Mutex<()>,
    /// Serializes the complete explicit Git initialization transaction, from
    /// durable intent through marker cleanup, across equivalent callers.
    git_initialization: Mutex<()>,
    /// Serializes every remote Powder operation and its final registry mutation
    /// for each Crew binding. This prevents cleanup or renewal from acting on a
    /// stale Active snapshot while completion is becoming durable. The guard is
    /// intentionally in-memory: after a crash, the durable pending digest drives
    /// a fresh evidence read before a same-proof retry.
    powder_operations_inflight: Mutex<std::collections::HashMap<String, CrewPowderOperationKind>>,
    powder_operation_ready: Condvar,
    /// Terminal IDs proven gone by the definitive startup tmux snapshot but not
    /// yet removed durably. This authoritative-read overlay survives persistence
    /// failures and is folded into the next successful registry mutation. When
    /// both locks are needed, `inner` is always acquired before this mutex.
    workspace_projection_exclusions: Mutex<std::collections::HashMap<String, u64>>,
    #[cfg(test)]
    #[allow(dead_code)]
    historical_scope_capture_hook: Mutex<Option<HistoricalScopeCaptureHook>>,
    #[cfg(test)]
    dispatch_barrier: Mutex<Option<DispatchBarrier>>,
    /// Persistence target; `None` = in-memory only (unit tests / headless proofs).
    path: Option<PathBuf>,
    /// Set when a newer on-disk schema is encountered. The old binary may expose
    /// no state, but it must never overwrite or quarantine the newer registry.
    pub(super) write_blocked: Option<String>,
    /// Serializes disk write-throughs WITHOUT holding `inner`, guarding the last
    /// revision that reached disk so an out-of-order write (a slower older
    /// snapshot racing a newer one after both dropped `inner`) can never regress
    /// the file. Held ONLY across the file write, and NEVER while `inner` is
    /// locked - so a stalled Windows/OneDrive-backed state write can't wedge a
    /// registry reader (`list_captains`, `get_status`) or the spawn hot path on
    /// the `inner` lock. That coupling - disk I/O under the registry lock - was
    /// the Incident-D flapping wedge (one slow persist parked every
    /// captains-touching command, and its handler thread, until it drained).
    persist: Mutex<u64>,
    /// Test-only injection point: a callback run INSIDE [`persist`](Self::persist)
    /// while it holds the `persist` mutex (never `inner`), so a test can SIMULATE a
    /// stalled disk write and assert a concurrent reader/mutator on `inner` is not
    /// blocked by it. `None` in every non-test path.
    #[cfg(test)]
    persist_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Test-only deterministic failure after a complete previous snapshot is on
    /// disk. This proves recovery keeps the last durable release state instead
    /// of relying on an in-memory transition.
    #[cfg(test)]
    fail_next_persist: Mutex<Option<String>>,
}

pub(super) struct CrewPowderOperationGuard<'a> {
    registry: &'a CaptainsRegistry,
    crew_session_id: String,
}

#[cfg(test)]
#[allow(dead_code)]
pub(super) struct HistoricalScopeCaptureHook {
    authenticated: std::sync::mpsc::SyncSender<String>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CrewPowderOperationKind {
    Cleanup,
}

impl Drop for CrewPowderOperationGuard<'_> {
    fn drop(&mut self) {
        let mut inflight = self
            .registry
            .powder_operations_inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inflight.remove(&self.crew_session_id);
        self.registry.powder_operation_ready.notify_all();
    }
}

/// Normalize a caller-supplied ship name into a slug: lowercase, runs of
/// non-alphanumerics collapse to single dashes, trimmed. Empty in = empty out
/// (the caller falls back to a derived slug).
pub(super) fn slugify_ship(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// --- Captains data model: workspace + pending-operation + snapshot records ---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FleetWorkspaceOwner {
    pub project_id: String,
    pub assignment_id: String,
    pub ship_slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FleetWorkspaceRecord {
    pub id: String,
    pub name: String,
    pub kind: WorkspaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<FleetWorkspaceOwner>,
    #[serde(default)]
    pub tile_ids: Vec<String>,
}

impl FleetWorkspaceRecord {
    pub(super) fn captain_workspace() -> Self {
        Self {
            id: CAPTAIN_WORKSPACE_ID.to_string(),
            name: CAPTAIN_WORKSPACE_NAME.to_string(),
            kind: WorkspaceKind::Captain,
            owner: None,
            tile_ids: Vec::new(),
        }
    }

    pub(super) fn as_tab_record(&self) -> TabRecord {
        TabRecord {
            id: self.id.clone(),
            name: self.name.clone(),
            tile_ids: self.tile_ids.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingFleetOperationPhase {
    Prepared,
    EffectApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PendingFleetOperationPayload {
    CloseTerminal {
        terminal_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        powder_release: Option<PendingDispatchRelease>,
    },
    CommissionCaptain {
        terminal_id: String,
        project_id: String,
        assignment: String,
        ship_slug: String,
        harness: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingFleetOperation {
    pub operation_id: String,
    pub expected_seq: u64,
    pub phase: PendingFleetOperationPhase,
    pub created_at: u64,
    pub payload: PendingFleetOperationPayload,
}

/// Durable transaction intent for explicit Git initialization.
/// The intent remains on disk until the Project and ownership marker are both
/// finalized, so a restart can finish or fail closed without guessing whether
/// T-Hub created the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitInitIntent {
    pub version: u32,
    pub operation_id: String,
    pub root_path: String,
    pub name: String,
    pub project_id: String,
    pub owner_identity: String,
    pub phase: String,
    pub marker_nonce: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_error: Option<String>,
}

#[derive(Debug)]
pub(super) struct CloseWorkspaceResult {
    pub(super) removed_tile_ids: Vec<String>,
    pub(super) captains_changed: bool,
}

#[derive(Debug)]
pub(super) struct CloseTerminalCommitResult {
    pub(super) captain_state_changed: bool,
    pub(super) workspace_changed: bool,
}

/// A full, versioned copy of the captains registry: what `list_captains` returns,
/// what every `sync_captains` forward carries down to the UI (the UI renders FROM
/// this, exactly like the tab [`RegistrySnapshot`]), and the on-disk persistence
/// shape (so a restart resumes at the same revision).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptainsSnapshot {
    /// On-disk schema version (item-2 §3.2/D2). Absent/0 = legacy; every write
    /// stamps [`CAPTAINS_SCHEMA_VERSION`].
    ///
    /// Upgrades are seamless because the reader accepts every prior shape.
    /// Downgrading from v2 is not safe for project metadata: a v1 binary ignores
    /// the new fields and drops them on its next write even though captain claims
    /// remain readable.
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub captains: Vec<CaptainRecord>,
    /// Durable identity, generation, continuity, and recovery state for the
    /// singleton Cortana supervisor.
    #[serde(default)]
    pub cortana: crate::cortana_reconcile::CortanaDurableIdentity,
    /// Powder-independent durable agent sessions.
    /// Legacy Crew records remain in `captains` until compatibility migration
    /// is complete.
    #[serde(default)]
    pub agent_sessions: Vec<AgentSessionRecord>,
    #[serde(default)]
    pub agent_checkpoints: Vec<AgentCheckpoint>,
    #[serde(default)]
    pub agent_events: Vec<AgentEvent>,
    /// Durable registered repositories. Added in schema v2; older snapshots
    /// deserialize to an empty registry.
    #[serde(default)]
    pub projects: Vec<ProjectRecord>,
    /// Durable Fleet Workspace authority. TabRegistry is only a projection cache
    /// of these records and is seeded from them before any listener or UI report.
    #[serde(default)]
    pub workspaces: Vec<FleetWorkspaceRecord>,
    /// Bounded prepare/effect/commit recovery records for operations that cross
    /// tmux, IdentityStore, or Powder boundaries.
    #[serde(default)]
    pub pending_fleet_operations: Vec<PendingFleetOperation>,
    /// Bounded durable tombstones for terminal identities retired by cleanup.
    /// These prevent a stale projection or racing move from resurrecting a tile.
    #[serde(default)]
    pub retired_fleet_tile_ids: Vec<String>,
    /// Initial claim attempts whose remote outcome remains unresolved.
    #[serde(default)]
    pub pending_dispatch_claims: Vec<PendingDispatchClaim>,
    /// Trusted post-bind release attempts whose remote outcome is ambiguous.
    #[serde(default)]
    pub pending_dispatch_releases: Vec<PendingDispatchRelease>,
    /// Explicit Git initialization transactions awaiting safe finalization or
    /// fail-closed recovery.
    #[serde(default)]
    pub pending_git_initializations: Vec<GitInitIntent>,
}

// --- Captains data model: FleetIdentity/CaptainRecord + claim/ship disposition ---
/// A fleet identity as the control channel sees it (item-2 §2.1: the ship/role
/// re-key). The record is keyed on the DURABLE `ship_slug` (was a mere label); the
/// terminal id is demoted to a rebindable `Option` pointer, `role` is first-class,
/// and the Claude UUID is a continuity anchor (a fast-path hint, resolved async, NOT
/// the load-bearing key). Crew carry their own anchor + state.
///
/// Serialized camelCase in BOTH directions: the persistence file, `list_captains`,
/// and every `sync_captains` forward all carry this exact shape. On READ it also
/// accepts the legacy v0 shape (`captainSessionId`, `crew: [string]`, no
/// role/state) via the field aliases + [`deserialize_crew`] (D2 migration).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetIdentity {
    /// The DURABLE primary key (item-2 §2.1): every registry lookup for a captain
    /// keys on this. For a Cortana claim it is the reserved [`CORTANA_SLUG`].
    pub ship_slug: String,
    /// Durable Assignment key. This is independent of the current terminal,
    /// Harness conversation, cwd, and Workspace placement.
    #[serde(default)]
    pub assignment_id: String,
    /// Durable user-facing Captain name. Legacy records are deterministically
    /// seeded from `ship_slug` during load and persisted on the next mutation.
    #[serde(default)]
    pub display_name: String,
    /// The first-class role (D1). Cortana is the registry-wide singleton.
    #[serde(default)]
    pub role: FleetRole,
    /// The Claude continuity anchor (`provider_session_id`). A fast-path idempotency
    /// hint that fires WHEN resolved and is otherwise absent (backfilled async,
    /// HIGH-1); correctness never rests on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_uuid: Option<String>,
    /// Harness that owns `provider_session_id`. Missing means a legacy Claude claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Provider-native continuity anchor. This replaces Claude-only identity for
    /// new claims while `claude_uuid` remains a read-compatible migration field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// The MUTABLE terminal pointer (was `captain_session_id`, the old primary key).
    /// `None` while orphaned/vacant - un-pointed but not lost (the exact window that
    /// deadlocked R-H2). Accepts the legacy `captainSessionId` field on load.
    #[serde(default, alias = "captainSessionId")]
    pub terminal_id: Option<String>,
    /// The registered project this ship supervises. A missing value identifies a
    /// legacy or deliberately unscoped Captain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// The Captain's durable assignment, restored independently of model memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Harness-neutral conversation identifier for reset and provider migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// Deterministic one-screen recovery state, refreshed by the Captain protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_point: Option<String>,
    #[serde(default)]
    pub workspace_tab_ids: Vec<String>,
    /// The ship's crew (item-2 §2.3). Deserializes from BOTH the legacy `Vec<String>`
    /// of tile ids and the modern `Vec<CrewRef>` (D2 migration).
    #[serde(default, deserialize_with = "deserialize_crew")]
    pub crew: Vec<CrewRef>,
    #[serde(default)]
    pub state: ClaimState,
}

/// Back-compat alias: item-2 renamed `CaptainRecord` → [`FleetIdentity`] (a captain
/// is a ship/role, not a terminal). The old name stays as an alias so existing
/// references and call sites read unchanged.
pub type CaptainRecord = FleetIdentity;

#[derive(Debug)]
pub(super) enum SnapshotReadError {
    Invalid(String),
    UnsupportedSchema { path: PathBuf, version: u32 },
    IncompatibleRecovery { path: PathBuf },
}

impl std::fmt::Display for SnapshotReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::UnsupportedSchema { path, version } => write!(
                f,
                "'{}' uses unsupported schemaVersion {version}",
                path.display()
            ),
            Self::IncompatibleRecovery { path } => write!(
                f,
                "'{}' contains dispatch release recovery state incompatible with this T-Hub version",
                path.display()
            ),
        }
    }
}

/// What a [`CaptainsRegistry::claim`] resolved to - for the audit/telemetry trail
/// (D6: orphan/rebind lifecycle is surfaced, never silent). Distinguishes a fresh
/// claim from an idempotent refresh, an orphan/vacant re-adoption, and a
/// dead-incumbent auto-release (the R-H2 deadlock clearer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimDisposition {
    /// A brand-new claim (durable key was free).
    Created,
    /// A re-claim by the SAME terminal (idempotent designation refresh).
    Refreshed,
    /// An `Orphaned`/`Vacant` record re-claimed by its own durable key (D4: the
    /// ship-slug re-claim IS the always-available auto-rebind trigger). Crew re-adopted.
    ReadoptedOrphan,
    /// The durable key was held by a DIFFERENT terminal that is UNAMBIGUOUSLY dead
    /// (`tmux::has_session` false - the SOLE transfer-grade signal, R1): the corpse's
    /// claim is auto-released and the new claim takes the slug. This is the R-H2
    /// deadlock clearer (§2.2 fix 1).
    AutoReleasedDead,
}

impl ClaimDisposition {
    pub fn label(self) -> &'static str {
        match self {
            ClaimDisposition::Created => "created",
            ClaimDisposition::Refreshed => "refreshed",
            ClaimDisposition::ReadoptedOrphan => "readopted_orphan",
            ClaimDisposition::AutoReleasedDead => "auto_released_dead",
        }
    }
}

/// The result of a [`CaptainsRegistry::claim`]: the resulting record + how it was
/// resolved (for the audit/telemetry stamp). Whether the registry `seq` advanced
/// (⇒ a `sync_captains` forward) is still derived by the caller from the seq delta,
/// exactly as before.
#[derive(Debug, Clone)]
pub struct ClaimOutcome {
    pub record: FleetIdentity,
    pub disposition: ClaimDisposition,
}

/// Which ship (and role) a terminal belongs to (item-2 §2.5/§2.6: the `ship_of`
/// resolution the cross-ship ownership ACL and per-session attribution key on). The
/// item-2 KEY; the ACL WIRING that consumes it stays item-1 Phase 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShipMembership {
    /// The tile is a supervisor's OWN terminal (a captain of its ship, or Cortana).
    Supervisor { ship_slug: String, role: FleetRole },
    /// The tile is a crew member of a ship.
    Crew { ship_slug: String },
}

impl ShipMembership {
    /// The durable ship slug, whichever membership kind (the H3 ACL comparison key).
    pub fn ship_slug(&self) -> &str {
        match self {
            ShipMembership::Supervisor { ship_slug, .. } => ship_slug,
            ShipMembership::Crew { ship_slug } => ship_slug,
        }
    }
}

// --- Captains data model: ProjectRecord ---
/// A repository registered with T-Hub. Projects outlive terminals, Captain
/// conversations, and individual ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub repo_root: String,
    /// Canonical POSIX Project identity. `repoRoot` remains a read-compatible
    /// wire alias while this field becomes the persisted source of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    /// Git capability is explicit: `git` or `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_capability: Option<String>,
    /// Canonical Git main-worktree root, present only for Git Projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_main_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub powder: Option<PowderProjectBinding>,
    pub created_at: u64,
    pub updated_at: u64,
}
