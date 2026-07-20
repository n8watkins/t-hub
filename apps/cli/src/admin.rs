use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::args::StrictFlags;
use super::{control, emit_json_ok, endpoint, CliError};

const OPERATIONS: &[&str] = &[
    "inspectStatus",
    "maintainSession",
    "cleanupSession",
    "recoverResource",
    "maintainWorktree",
    "cleanupWorktree",
    "prepareRetirement",
    "buildCrossCaptainReport",
    "maintainFleetResource",
];

pub fn command_label(args: &[String]) -> String {
    match args.first().map(String::as_str) {
        Some(sub) if !sub.starts_with('-') => format!("admin {sub}"),
        _ => "admin".into(),
    }
}

pub fn run(args: &[String]) -> Result<(), CliError> {
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        print_help();
        return Ok(());
    }
    match args.first().map(String::as_str).unwrap_or("") {
        "list" => list(&args[1..]),
        "appoint" => appoint(&args[1..]),
        "revoke" => revoke(&args[1..]),
        "approve-session" => approve_session(&args[1..]),
        "approve-worktree" => approve_worktree(&args[1..]),
        "cleanup-session" => cleanup_session(&args[1..]),
        "" => Err(CliError::usage(
            "usage: th admin <list|appoint|revoke|approve-session|approve-worktree|cleanup-session> ...",
        )),
        other => Err(CliError::usage(format!(
            "unknown admin subcommand '{other}' (expected list|appoint|revoke|approve-session|approve-worktree|cleanup-session)"
        ))),
    }
}

fn print_help() {
    println!(
        "usage: th admin <command> [flags]\n\n\
list                                                  list grants visible to this identity\n\
appoint <crewSessionId> --role ROLE --operations CSV  appoint a durable Ship or Fleet Admin\n\
revoke <grantId> [--reason TEXT]                      revoke a grant and its active approvals\n\
approve-session <grantId> <sessionId> --ship SLUG     approve one exact session cleanup\n\
approve-worktree <grantId> <path> --ship SLUG         approve one exact worktree cleanup\n\
cleanup-session <sessionId> --approval ID --confirm   consume approval and close the session\n\n\
ROLE is shipAdmin or fleetAdmin.\n\
CSV operations: inspectStatus, maintainSession, cleanupSession, recoverResource,\n\
maintainWorktree, cleanupWorktree, prepareRetirement, buildCrossCaptainReport,\n\
maintainFleetResource. Worktree removal remains unavailable until the authoritative\n\
worktree safety service proves the exact target removable."
    );
}

fn list(args: &[String]) -> Result<(), CliError> {
    let flags = StrictFlags::parse(args, &[], &["--json"])?;
    flags.require_positionals(0, "th admin list [--json]")?;
    call_and_render("admin list", "list_admin_grants", json!({}), &flags)
}

fn appoint(args: &[String]) -> Result<(), CliError> {
    let flags = StrictFlags::parse(args, &["--role", "--operations"], &["--json"])?;
    flags.require_positionals(
        1,
        "th admin appoint <crewSessionId> --role <shipAdmin|fleetAdmin> --operations <csv> [--json]",
    )?;
    let actor = positional(&flags, 0, "admin appoint", "<crewSessionId>")?;
    let role = required(&flags, "--role", "admin appoint")?;
    if !matches!(role.as_str(), "shipAdmin" | "fleetAdmin") {
        return Err(CliError::usage(
            "th admin appoint: --role must be shipAdmin or fleetAdmin",
        ));
    }
    let operations = csv_values(
        &required(&flags, "--operations", "admin appoint")?,
        "th admin appoint: --operations",
    )?;
    for operation in &operations {
        if !OPERATIONS.contains(&operation.as_str()) {
            return Err(CliError::usage(format!(
                "th admin appoint: unknown operation '{operation}'"
            )));
        }
    }
    call_and_render(
        "admin appoint",
        "appoint_admin",
        json!({
            "actorSessionId": actor,
            "role": role,
            "permittedOperations": operations,
        }),
        &flags,
    )
}

fn revoke(args: &[String]) -> Result<(), CliError> {
    let flags = StrictFlags::parse(args, &["--reason"], &["--json"])?;
    flags.require_positionals(1, "th admin revoke <grantId> [--reason TEXT] [--json]")?;
    let grant_id = positional(&flags, 0, "admin revoke", "<grantId>")?;
    let mut input = json!({"grantId": grant_id});
    if let Some(reason) = flags.options.get("--reason") {
        input["reason"] = json!(reason);
    }
    call_and_render("admin revoke", "revoke_admin", input, &flags)
}

fn approve_session(args: &[String]) -> Result<(), CliError> {
    let flags = StrictFlags::parse(args, &["--ship"], &["--json"])?;
    flags.require_positionals(
        2,
        "th admin approve-session <grantId> <sessionId> --ship <slug> [--json]",
    )?;
    let grant_id = positional(&flags, 0, "admin approve-session", "<grantId>")?;
    let session_id = positional(&flags, 1, "admin approve-session", "<sessionId>")?;
    let ship_slug = required(&flags, "--ship", "admin approve-session")?;
    call_and_render(
        "admin approve-session",
        "approve_admin_action",
        json!({
            "grantId": grant_id,
            "operation": "cleanupSession",
            "target": {
                "kind": "crewSession",
                "shipSlug": ship_slug,
                "sessionId": session_id,
            }
        }),
        &flags,
    )
}

fn approve_worktree(args: &[String]) -> Result<(), CliError> {
    let flags = StrictFlags::parse(args, &["--ship"], &["--json"])?;
    flags.require_positionals(
        2,
        "th admin approve-worktree <grantId> <path> --ship <slug> [--json]",
    )?;
    let grant_id = positional(&flags, 0, "admin approve-worktree", "<grantId>")?;
    let worktree_id = positional(&flags, 1, "admin approve-worktree", "<path>")?;
    let ship_slug = required(&flags, "--ship", "admin approve-worktree")?;
    call_and_render(
        "admin approve-worktree",
        "approve_admin_action",
        json!({
            "grantId": grant_id,
            "operation": "cleanupWorktree",
            "target": {
                "kind": "worktree",
                "shipSlug": ship_slug,
                "worktreeId": worktree_id,
            }
        }),
        &flags,
    )
}

fn cleanup_session(args: &[String]) -> Result<(), CliError> {
    let flags = StrictFlags::parse(args, &["--approval"], &["--confirm", "--force", "--json"])?;
    flags.require_positionals(
        1,
        "th admin cleanup-session <sessionId> --approval <id> --confirm [--force] [--json]",
    )?;
    if !flags.booleans.contains("--confirm") {
        return Err(CliError::gated(
            "th admin cleanup-session requires --confirm before endpoint discovery or mutation",
        ));
    }
    let session_id = positional(&flags, 0, "admin cleanup-session", "<sessionId>")?;
    let approval_id = required(&flags, "--approval", "admin cleanup-session")?;
    call_and_render(
        "admin cleanup-session",
        "close_terminal",
        json!({
            "sessionId": session_id,
            "approvalId": approval_id,
            "force": flags.booleans.contains("--force"),
        }),
        &flags,
    )
}

fn required(flags: &StrictFlags, name: &str, command: &str) -> Result<String, CliError> {
    flags
        .options
        .get(name)
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CliError::usage(format!("th {command}: missing required flag {name}")))
}

fn positional(
    flags: &StrictFlags,
    index: usize,
    command: &str,
    name: &str,
) -> Result<String, CliError> {
    let value = flags.positionals.get(index).cloned().ok_or_else(|| {
        CliError::usage(format!("th {command}: missing required argument {name}"))
    })?;
    if value.trim().is_empty() {
        return Err(CliError::usage(format!(
            "th {command}: {name} must not be empty"
        )));
    }
    Ok(value)
}

fn csv_values(value: &str, label: &str) -> Result<Vec<String>, CliError> {
    let mut values = BTreeSet::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(CliError::usage(format!(
                "{label} must be a comma-separated list without empty entries"
            )));
        }
        values.insert(item.to_string());
    }
    Ok(values.into_iter().collect())
}

fn call_and_render(
    label: &str,
    operation: &str,
    args: Value,
    flags: &StrictFlags,
) -> Result<(), CliError> {
    let result = control::call(&endpoint()?, operation, args)?;
    if flags.json() {
        emit_json_ok(label, result);
    } else {
        println!(
            "{}: {}",
            label,
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    }
    Ok(())
}
