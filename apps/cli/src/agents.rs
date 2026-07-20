use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::args::StrictFlags;
use super::{control, emit_json_ok, endpoint, CliError};

type AgentFlags = StrictFlags;

pub fn command_label(args: &[String]) -> String {
    match args.first().map(String::as_str) {
        Some(sub) if !sub.starts_with('-') => format!("agents {sub}"),
        _ => "agents".into(),
    }
}

pub fn run(args: &[String]) -> Result<(), CliError> {
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        print_help();
        return Ok(());
    }
    match args.first().map(String::as_str).unwrap_or("") {
        "preflight" => preflight(&args[1..]),
        "start" => start(&args[1..]),
        "list" => list(&args[1..]),
        "show" => show(&args[1..]),
        "checkpoint" => checkpoint(&args[1..]),
        "delivery" => delivery(&args[1..]),
        "events" => events(&args[1..]),
        "" => Err(CliError::usage(
            "usage: th agents <preflight|start|list|show|checkpoint|delivery|events> ...",
        )),
        other => Err(CliError::usage(format!(
            "unknown agents subcommand '{other}' (expected preflight|start|list|show|checkpoint|delivery|events)"
        ))),
    }
}

fn print_help() {
    println!(
        "usage: th agents <preflight|start|list|show|checkpoint|delivery|events> [flags]\n\n\
preflight  --project ID --lanes-json JSON [--integration-contracts-json JSON] [--json]\n\
start      --request-id ID --captain ID --directory PATH --assignment TEXT --source-commit COMMIT --lane-id ID [ownership flags] [--admission-purpose PURPOSE]\n\
list       --captain ID or --project ID [--cursor N] [--limit N] [--state active|removed] [--json]\n\
show       <agentSessionId> [--json]\n\
checkpoint <agentSessionId> <summary> --author ID [--stage STAGE] [--json]\n\
delivery   <agentSessionId> <state> --evidence-json JSON [--json]\n\
events     <agentSessionId> [--cursor N] [--limit N] [--json]"
    );
}

fn preflight(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(
        args,
        &["--project", "--lanes-json", "--integration-contracts-json"],
        &["--json"],
    )?;
    flags.require_positionals(0, "th agents preflight [flags]")?;
    let project = required(&flags, "--project", "agents preflight")?;
    let lanes = json_array(
        &required(&flags, "--lanes-json", "agents preflight")?,
        "th agents preflight: --lanes-json",
        false,
    )?;
    let integration_contracts = optional_json_array(
        &flags,
        "--integration-contracts-json",
        "th agents preflight: --integration-contracts-json",
    )?;
    call_and_render(
        "agents preflight",
        "dispatch_preflight",
        json!({
            "projectId": project,
            "requestedLanes": lanes,
            "integrationContracts": integration_contracts,
        }),
        &flags,
    )
}

fn start(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(
        args,
        &[
            "--request-id",
            "--captain",
            "--directory",
            "--assignment",
            "--harness",
            "--name",
            "--tab",
            "--source-commit",
            "--lane-id",
            "--dependencies",
            "--mutable-files",
            "--mutable-schemas",
            "--mutable-interfaces",
            "--integration-contracts-json",
            "--admission-purpose",
        ],
        &["--visible-product-bug", "--json"],
    )?;
    flags.require_positionals(0, "th agents start [flags]")?;
    let request_id = required(&flags, "--request-id", "agents start")?;
    let captain = required(&flags, "--captain", "agents start")?;
    let directory = required(&flags, "--directory", "agents start")?;
    let assignment = required(&flags, "--assignment", "agents start")?;
    let source_commit = required(&flags, "--source-commit", "agents start")?;
    validate_commit(&source_commit, "th agents start: --source-commit")?;
    let lane_id = required(&flags, "--lane-id", "agents start")?;
    let mut args = json!({
        "requestId": request_id,
        "captainSessionId": captain,
        "directory": directory,
        "assignment": assignment,
        "sourceCommit": source_commit,
        "visibleProductBug": flags.booleans.contains("--visible-product-bug"),
        "laneId": lane_id,
        "dependencies": optional_csv(&flags, "--dependencies")?,
        "mutableFiles": optional_csv(&flags, "--mutable-files")?,
        "mutableSchemas": optional_csv(&flags, "--mutable-schemas")?,
        "mutableInterfaces": optional_csv(&flags, "--mutable-interfaces")?,
        "integrationContracts": optional_json_array(
            &flags,
            "--integration-contracts-json",
            "th agents start: --integration-contracts-json",
        )?,
    });
    for (flag, key) in [
        ("--harness", "harness"),
        ("--name", "name"),
        ("--tab", "workspaceTabId"),
    ] {
        if let Some(value) = flags.options.get(flag) {
            args[key] = json!(value);
        }
    }
    if let Some(harness) = flags.options.get("--harness") {
        if !matches!(harness.as_str(), "codex" | "claude") {
            return Err(CliError::usage(
                "th agents start: --harness must be codex or claude",
            ));
        }
    }
    if let Some(purpose) = flags.options.get("--admission-purpose") {
        if !matches!(
            purpose.as_str(),
            "ordinary" | "fleet-admin" | "ship-admin" | "recovery"
        ) {
            return Err(CliError::usage(
                "th agents start: --admission-purpose must be ordinary, fleet-admin, ship-admin, or recovery",
            ));
        }
        args["admissionPurpose"] = json!(purpose);
    }
    call_and_render("agents start", "start_agent", args, &flags)
}

fn list(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(
        args,
        &["--captain", "--project", "--cursor", "--limit", "--state"],
        &["--json"],
    )?;
    flags.require_positionals(0, "th agents list [flags]")?;
    if !flags.options.contains_key("--captain") && !flags.options.contains_key("--project") {
        return Err(CliError::usage(
            "th agents list: requires --captain <id> or --project <id>",
        ));
    }
    let mut input = json!({});
    if let Some(value) = flags.options.get("--captain") {
        input["captainSessionId"] = json!(value);
    }
    if let Some(value) = flags.options.get("--project") {
        input["projectId"] = json!(value);
    }
    if let Some(value) = flags.options.get("--cursor") {
        parse_cursor(value, "th agents list")?;
        input["cursor"] = json!(value);
    }
    if let Some(value) = flags.options.get("--limit") {
        input["limit"] = json!(parse_limit(value, "th agents list")?);
    }
    if let Some(value) = flags.options.get("--state") {
        if !matches!(value.as_str(), "active" | "removed") {
            return Err(CliError::usage(
                "th agents list: --state must be active or removed",
            ));
        }
        input["state"] = json!(value);
    }
    call_and_render("agents list", "list_agents", input, &flags)
}

fn show(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(args, &[], &["--json"])?;
    flags.require_positionals(1, "th agents show <agentSessionId> [--json]")?;
    let agent = positional(&flags, 0, "agents show", "<agentSessionId>")?;
    call_and_render(
        "agents show",
        "get_agent",
        json!({"agentSessionId": agent}),
        &flags,
    )
}

fn checkpoint(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(args, &["--author", "--stage"], &["--json"])?;
    flags.require_positionals(
        2,
        "th agents checkpoint <agentSessionId> <summary> --author <id> [--stage <stage>] [--json]",
    )?;
    let agent = positional(&flags, 0, "agents checkpoint", "<agentSessionId>")?;
    let summary = positional(&flags, 1, "agents checkpoint", "<summary>")?;
    let author = required(&flags, "--author", "agents checkpoint")?;
    let mut input = json!({
        "agentSessionId": agent,
        "authorSessionId": author,
        "summary": summary,
    });
    if let Some(stage) = flags.options.get("--stage") {
        validate_stage(stage)?;
        input["stage"] = json!(stage);
    }
    call_and_render("agents checkpoint", "agent_checkpoint", input, &flags)
}

fn delivery(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(args, &["--evidence-json"], &["--json"])?;
    flags.require_positionals(
        2,
        "th agents delivery <agentSessionId> <state> --evidence-json <json> [--json]",
    )?;
    let agent = positional(&flags, 0, "agents delivery", "<agentSessionId>")?;
    let state = positional(&flags, 1, "agents delivery", "<state>")?;
    if !matches!(
        state.as_str(),
        "implemented"
            | "reviewed"
            | "tested"
            | "integrated"
            | "packaged"
            | "installed"
            | "liveVerified"
    ) {
        return Err(CliError::usage(
            "th agents delivery: state must be implemented, reviewed, tested, integrated, packaged, installed, or liveVerified",
        ));
    }
    let evidence_text = required(&flags, "--evidence-json", "agents delivery")?;
    let evidence = serde_json::from_str::<Value>(&evidence_text).map_err(|error| {
        CliError::usage(format!(
            "th agents delivery: --evidence-json must be valid JSON: {error}"
        ))
    })?;
    if !evidence.is_object() {
        return Err(CliError::usage(
            "th agents delivery: --evidence-json must be a JSON object",
        ));
    }
    call_and_render(
        "agents delivery",
        "record_agent_delivery",
        json!({
            "agentSessionId": agent,
            "state": state,
            "evidence": evidence,
        }),
        &flags,
    )
}

fn events(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(args, &["--cursor", "--limit"], &["--json"])?;
    flags.require_positionals(1, "th agents events <agentSessionId> [flags]")?;
    let agent = positional(&flags, 0, "agents events", "<agentSessionId>")?;
    let mut input = json!({"agentSessionId": agent});
    if let Some(value) = flags.options.get("--cursor") {
        parse_cursor(value, "th agents events")?;
        input["cursor"] = json!(value);
    }
    if let Some(value) = flags.options.get("--limit") {
        input["limit"] = json!(parse_limit(value, "th agents events")?);
    }
    call_and_render("agents events", "agent_events", input, &flags)
}

fn required(flags: &AgentFlags, name: &str, command: &str) -> Result<String, CliError> {
    flags
        .options
        .get(name)
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CliError::usage(format!("th {command}: missing required flag {name}")))
}

fn positional(
    flags: &AgentFlags,
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

fn parse_cursor(value: &str, command: &str) -> Result<u64, CliError> {
    value.parse::<u64>().map_err(|_| {
        CliError::usage(format!(
            "{command}: --cursor must be a non-negative integer"
        ))
    })
}

fn parse_limit(value: &str, command: &str) -> Result<u64, CliError> {
    let limit = value.parse::<u64>().map_err(|_| {
        CliError::usage(format!(
            "{command}: --limit must be an integer from 1 to 100"
        ))
    })?;
    if !(1..=100).contains(&limit) {
        return Err(CliError::usage(format!(
            "{command}: --limit must be an integer from 1 to 100"
        )));
    }
    Ok(limit)
}

fn validate_stage(stage: &str) -> Result<(), CliError> {
    if matches!(
        stage,
        "assigned"
            | "working"
            | "needsInput"
            | "readyForReview"
            | "awaitingIntegration"
            | "complete"
            | "stopped"
    ) {
        return Ok(());
    }
    Err(CliError::usage(
        "th agents checkpoint: --stage must be assigned, working, needsInput, readyForReview, awaitingIntegration, complete, or stopped",
    ))
}

fn validate_commit(value: &str, label: &str) -> Result<(), CliError> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(CliError::usage(format!(
        "{label} must be an exact 40- or 64-character hexadecimal commit"
    )))
}

fn optional_csv(flags: &AgentFlags, name: &str) -> Result<Vec<String>, CliError> {
    let Some(value) = flags.options.get(name) else {
        return Ok(Vec::new());
    };
    let mut values = BTreeSet::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(CliError::usage(format!(
                "th agents start: {name} must be a comma-separated list without empty entries"
            )));
        }
        values.insert(item.to_string());
    }
    Ok(values.into_iter().collect())
}

fn optional_json_array(
    flags: &AgentFlags,
    name: &str,
    label: &str,
) -> Result<Vec<Value>, CliError> {
    match flags.options.get(name) {
        Some(value) => json_array(value, label, true),
        None => Ok(Vec::new()),
    }
}

fn json_array(value: &str, label: &str, allow_empty: bool) -> Result<Vec<Value>, CliError> {
    let values = serde_json::from_str::<Vec<Value>>(value)
        .map_err(|error| CliError::usage(format!("{label} must be a JSON array: {error}")))?;
    if !allow_empty && values.is_empty() {
        return Err(CliError::usage(format!(
            "{label} must contain at least one lane"
        )));
    }
    Ok(values)
}

fn call_and_render(
    label: &str,
    operation: &str,
    args: Value,
    flags: &AgentFlags,
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
