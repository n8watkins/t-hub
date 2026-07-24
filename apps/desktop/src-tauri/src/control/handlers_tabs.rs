//! Tab / organization control handlers, split out of `control.rs` to shrink that
//! module. The organization-apply forwarding helpers (`broadcast_apply`,
//! `forward_apply`, `organization_apply`, `with_sync`, `organization_sync_apply`)
//! and the tab mutations `new_tab` / `close_tab` / `rename_tab` / `focus_tab` /
//! `move_tile` plus `list_tabs`. The parent dispatch match routes here.

use super::*;

/// Organization-tier actions whose effect is a pure UI mutation
/// (`focus_session`, `move_tile`, `rename_tab`). We **accept and audit** them
/// (PRD §11.2: "allowed with visible audit event") AND apply them: the accepted
/// `{command, args}` is forwarded to the frontend through the [`ApplySink`]
/// (a Tauri `control://apply` event), where `controlBridge.ts` dispatches it into
/// the workspace store. `applied` reflects whether the forward happened — `true`
/// once the app has wired its sink (the normal app path), `false` in the headless
/// proof/tests that run the listener without an `AppHandle` (still audited).
/// Broadcast one accepted forward to event subscribers on
/// [`APPLY_EVENT_CHANNEL`] (T12: the native client's delivery path). Returns how
/// many subscribers received it. Zero subscribers is a cheap no-op, so this runs
/// unconditionally next to every [`ApplySink`] forward.
pub(super) fn broadcast_apply(ctx: &ControlContext, command: &str, args: &Value) -> usize {
    ctx.fanout.emit_event(
        APPLY_EVENT_CHANNEL,
        &json!({ "command": command, "args": args }),
    )
}

/// Forward one command + args to the frontend through the [`ApplySink`], returning
/// whether the forward was delivered. A forward failure is non-fatal (logged), and
/// no sink (headless proof/tests) is simply `false`. Shared by every
/// Organization-tier handler that mutates the UI.
///
/// T12: the forward is ALSO broadcast to event subscribers (the native client's
/// path). With a sink wired (the Tauri app), `applied` keeps meaning exactly what
/// it always did — the sink delivered — so the webview path is unchanged; with no
/// sink (a headless server fronting the native cockpit), reaching at least one
/// event subscriber counts as delivery.
pub(super) fn forward_apply(ctx: &ControlContext, command: &str, args: &Value) -> bool {
    let sink_applied = match &ctx.apply_sink {
        Some(sink) => match sink.apply(command, args) {
            Ok(()) => Some(true),
            Err(e) => {
                eprintln!("t-hub-control: failed to forward '{command}' to the UI: {e}");
                Some(false)
            }
        },
        None => None,
    };
    let subscribers = broadcast_apply(ctx, command, args);
    sink_applied.unwrap_or(subscribers > 0)
}

pub(super) fn organization_apply(
    ctx: &ControlContext,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    let applied = forward_apply(ctx, command, args);
    Ok(json!({
        "accepted": command,
        "args": args,
        "audited": true,
        "applied": applied,
        "note": if applied {
            "organization action accepted, audited, and forwarded to the UI \
             (control://apply) for application (PRD §11.2 organization tier)."
        } else {
            "organization action accepted + audited; UI application is delivered \
             by the frontend command (PRD §11.2 organization tier)."
        },
    }))
}

/// Merge the authoritative registry snapshot into a forward's args (under `sync`)
/// so the UI renders FROM the registry instead of re-deriving the mutation -
/// the headless-org apply contract. Applied AFTER the registry mutation, so the
/// snapshot already reflects it.
pub(super) fn with_sync(ctx: &ControlContext, mut args: Value) -> Value {
    let snap = ctx.tabs.snapshot_full();
    args["sync"] = serde_json::to_value(&snap).unwrap_or(Value::Null);
    args
}

/// Registry-first organization mutation: the registry was already updated; forward
/// the args + authoritative snapshot to the UI and answer with the new revision.
pub(super) fn organization_sync_apply(
    ctx: &ControlContext,
    command: &str,
    args: Value,
) -> Result<Value, String> {
    let forward = with_sync(ctx, args);
    let applied = forward_apply(ctx, command, &forward);
    let snap = ctx.tabs.snapshot_full();
    Ok(json!({
        "accepted": command,
        "audited": true,
        "applied": applied,
        "seq": snap.seq,
        "tabs": snap.tabs,
        "note": "applied to the server tab registry (authoritative) and forwarded \
                 to the UI with the registry snapshot; a hidden tab or unfocused \
                 window cannot lose this update.",
    }))
}

/// `new_tab` (Organization, audited): create a new workspace tab. TASK C / #22 —
/// the CORE mints the tab id so it can RETURN it (`tabId`), making the tab
/// immediately addressable for `move_tile` / `focus_tab`, and forwards that id to
/// the frontend to adopt (rather than letting the frontend mint its own id the
/// caller never learns). The id is recorded in the registry optimistically so
/// `list_tabs` sees it before the frontend reports back. Args: `name` (optional;
/// auto-named "Workspace N" when omitted).
pub(super) fn new_tab(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let authority = workspace_mutation_authority(ctx, caller, trusted_internal, "new_tab")?;
    let name = arg_str(args, "name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ctx.tabs.auto_name());
    if name == CAPTAIN_WORKSPACE_NAME || name == "Captains" {
        return Err("new_tab: Captain Workspace is reserved and already exists".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let owner = match &authority {
        WorkspaceMutationAuthority::Apex => None,
        WorkspaceMutationAuthority::Assignment(owner) => {
            if arg_str(args, "projectId")
                .or_else(|| arg_str(args, "project_id"))
                .is_some_and(|project_id| project_id != owner.project_id)
                || arg_str(args, "shipSlug")
                    .or_else(|| arg_str(args, "ship_slug"))
                    .is_some_and(|ship_slug| ship_slug != owner.ship_slug)
            {
                return Err("acl: new_tab requested a foreign Project or Captain ship".into());
            }
            Some(owner)
        }
    };
    ctx.captains.create_workspace(&id, &name, owner)?;
    ctx.tabs.insert_tab(&id, &name);
    let mut res = organization_sync_apply(ctx, "new_tab", json!({ "id": id, "name": name }))?;
    res["tabId"] = json!(id);
    res["name"] = json!(name);
    Ok(res)
}

/// `close_tab` (Organization, audited; headless-org): close a workspace tab over
/// the socket - the missing half of the headless tab lifecycle (an auto-created
/// tab emptied by `close_terminal` was previously only closeable by hand in the
/// UI). Policy (see [`TabRegistry::remove_tab`]): unknown tab and the last tab
/// are errors; a non-empty tab is refused unless `force: true` (its still-live
/// sessions are then re-adopted into the UI's active tab, never orphaned).
/// Auto-created empty tabs are NOT reaped implicitly - an agent staging a
/// workspace may empty and refill a tab, so closing is always an explicit call.
/// Args: `tabId` (or `tabName` to resolve by exact name); `force` (optional).
pub(super) fn close_tab(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let tab_id = arg_str(args, "tabId")
        .or_else(|| arg_str(args, "id"))
        .or_else(|| {
            arg_str(args, "tabName")
                .or_else(|| arg_str(args, "tab_name"))
                .and_then(|n| ctx.tabs.id_for_name(&n))
        })
        .ok_or("close_tab requires a 'tabId' (or a 'tabName' that resolves to one)")?;
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let authority = workspace_mutation_authority(ctx, caller, trusted_internal, "close_tab")?;
    if matches!(authority, WorkspaceMutationAuthority::Apex) {
        ctx.captains
            .adopt_unowned_workspace_projection(&ctx.tabs.snapshot())?;
    }
    enforce_workspace_owner(ctx, &authority, &tab_id, "close_tab")?;
    let expected_owner = match &authority {
        WorkspaceMutationAuthority::Apex => None,
        WorkspaceMutationAuthority::Assignment(owner) => Some(owner),
    };
    let closed = ctx
        .captains
        .close_workspace(&tab_id, force, expected_owner)?;
    #[cfg(test)]
    if args
        .get("testCrashAfterFleetCommit")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err("injected crash after durable Fleet Workspace close commit".into());
    }
    ctx.tabs.replace(ctx.captains.workspace_projection());
    let _ = captains_sync_apply(ctx);
    let mut res =
        organization_sync_apply(ctx, "close_tab", json!({ "tabId": tab_id, "force": force }))?;
    res["tabId"] = json!(tab_id);
    res["orphanedTileIds"] = json!(closed.removed_tile_ids);
    res["captainsChanged"] = json!(closed.captains_changed);
    Ok(res)
}

/// `rename_tab` (Organization, audited; headless-org): rename a tab. Registry-
/// first + strict (unknown tab is an error), then forwards the snapshot so the
/// rename applies even when the tab is hidden or the window is unfocused.
/// Args: `tabId` (or `id`), `name`.
pub(super) fn rename_tab(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let tab_id = arg_str(args, "tabId")
        .or_else(|| arg_str(args, "id"))
        .ok_or("rename_tab requires a 'tabId' argument")?;
    let name = arg_str(args, "name")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or("rename_tab requires a non-empty 'name' argument")?;
    let authority = workspace_mutation_authority(ctx, caller, trusted_internal, "rename_tab")?;
    if matches!(authority, WorkspaceMutationAuthority::Apex) {
        ctx.captains
            .adopt_unowned_workspace_projection(&ctx.tabs.snapshot())?;
    }
    enforce_workspace_owner(ctx, &authority, &tab_id, "rename_tab")?;
    ctx.captains.rename_workspace(&tab_id, &name)?;
    ctx.tabs.rename_tab(&tab_id, &name)?;
    organization_sync_apply(ctx, "rename_tab", json!({ "tabId": tab_id, "name": name }))
}

/// `focus_tab` (Organization, audited): activate a tab - the ONE organization
/// command that intentionally moves the user's view. Validates the tab against
/// the registry (strict), mirrors the new active tab there (so `list_tabs` and
/// default spawn placement track it), and forwards to the UI.
pub(super) fn focus_tab(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let tab_id = arg_str(args, "tabId")
        .or_else(|| arg_str(args, "id"))
        .ok_or("focus_tab requires a 'tabId' argument")?;
    let authority = workspace_mutation_authority(ctx, caller, trusted_internal, "focus_tab")?;
    enforce_workspace_owner(ctx, &authority, &tab_id, "focus_tab")?;
    // Validate-and-set atomically (a focus racing a close must fail cleanly, not
    // leave the registry's active pointer on a deleted tab).
    if !ctx.tabs.set_active_tab(&tab_id) {
        return Err(format!("focus_tab: unknown tabId '{tab_id}'"));
    }
    organization_apply(ctx, "focus_tab", &json!({ "tabId": tab_id }))
}

/// `move_tile` (Organization, audited; headless-org): move a tile into another
/// tab. Registry-FIRST and STRICT: the server registry is updated (or the command
/// errors - an unknown `tabId` is a hard error now, not the silent accept-then-
/// lose of the mirror model), then the authoritative snapshot is forwarded so the
/// UI applies it even when the target tab is hidden. A `targetId`-only call is
/// the legacy within-tab reorder: forwarded for the UI to apply and report back
/// (visual order is a UI concern).
pub(super) fn move_tile(
    ctx: &ControlContext,
    args: &Value,
    caller: Option<&ResolvedIdentity>,
    trusted_internal: bool,
) -> Result<Value, String> {
    let tile = arg_str(args, "terminalId").or_else(|| arg_str(args, "id"));
    let tab = arg_str(args, "tabId");
    match (tile, tab) {
        (Some(tile), Some(tab)) => {
            let authority =
                workspace_mutation_authority(ctx, caller, trusted_internal, "move_tile")?;
            if matches!(authority, WorkspaceMutationAuthority::Apex) {
                ctx.captains
                    .adopt_unowned_workspace_projection(&ctx.tabs.snapshot())?;
            }
            enforce_workspace_owner(ctx, &authority, &tab, "move_tile")?;
            if let Some(source) = ctx
                .captains
                .snapshot()
                .workspaces
                .iter()
                .find(|workspace| workspace.tile_ids.contains(&tile))
                .map(|workspace| workspace.id.clone())
            {
                enforce_workspace_owner(ctx, &authority, &source, "move_tile")?;
            }
            let kind = ctx
                .tabs
                .kind_for_id(&tab)
                .ok_or_else(|| format!("move_tile: unknown tabId '{tab}'"))?;
            validate_workspace_occupant(&ctx.captains, &tile, &tab, kind)?;
            ctx.captains.move_workspace_tile(&tile, &tab)?;
            ctx.tabs.replace(ctx.captains.workspace_projection());
            organization_sync_apply(
                ctx,
                "move_tile",
                json!({ "terminalId": tile, "tabId": tab }),
            )
        }
        _ => organization_apply(ctx, "move_tile", args),
    }
}

/// `list_tabs`: the live workspace tabs from the CORE tab registry (TASK C / #22),
/// each `{id, name, tileIds}`. The frontend reports its full tab layout up (the
/// `report_workspace_tabs` Tauri command) so this reflects UI-created tabs and real
/// tile membership; MCP-driven `new_tab` / `move_tile` / named placement update it
/// optimistically so a just-created tab is addressable immediately. This is the
/// minimal in-memory registry that makes headless tab ops (discover an id, then
/// `move_tile` / `focus_tab` into it) work — NOT the PRD §8 persistence layer.
pub(super) fn list_tabs(ctx: &ControlContext) -> Result<Value, String> {
    let snap = ctx.tabs.snapshot_full();
    Ok(json!({
        "tabs": snap.tabs,
        "count": snap.tabs.len(),
        "seq": snap.seq,
        "activeTabId": snap.active_tab_id,
    }))
}
