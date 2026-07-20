use serde_json::{json, Value};

use super::{control, emit_json_ok, endpoint, CliError, Flags};

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
        "start" => start(&args[1..]),
        "list" => list(&args[1..]),
        "show" => show(&args[1..]),
        "checkpoint" => checkpoint(&args[1..]),
        "events" => events(&args[1..]),
        "" => Err(CliError::usage(
            "usage: th agents <start|list|show|checkpoint|events> ...",
        )),
        other => Err(CliError::usage(format!(
            "unknown agents subcommand '{other}' (expected start|list|show|checkpoint|events)"
        ))),
    }
}

fn print_help() {
    println!(
        "usage: th agents <start|list|show|checkpoint|events> [flags]\n\n\
start      --request-id ID --captain ID --directory PATH --assignment TEXT [--harness codex|claude] [--name NAME] [--tab ID]\n\
list       --captain ID or --project ID [--cursor N] [--limit N] [--json]\n\
show       <agentSessionId> [--json]\n\
checkpoint <agentSessionId> <summary> --author ID [--stage STAGE] [--json]\n\
events     <agentSessionId> [--cursor N] [--limit N] [--json]"
    );
}

fn start(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(
        args,
        &[
            "--request-id",
            "--captain",
            "--directory",
            "--assignment",
            "--harness",
            "--name",
            "--tab",
        ],
    )?;
    let request_id = required(&flags, "--request-id", "agents start")?;
    let captain = required(&flags, "--captain", "agents start")?;
    let directory = required(&flags, "--directory", "agents start")?;
    let assignment = required(&flags, "--assignment", "agents start")?;
    let mut args = json!({
        "requestId": request_id,
        "captainSessionId": captain,
        "directory": directory,
        "assignment": assignment,
    });
    for (flag, key) in [
        ("--harness", "harness"),
        ("--name", "name"),
        ("--tab", "workspaceTabId"),
    ] {
        if let Some(value) = flags.opts.get(flag) {
            args[key] = json!(value);
        }
    }
    call_and_render("agents start", "start_agent", args, &flags)
}

fn list(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(args, &["--captain", "--project", "--cursor", "--limit"])?;
    let mut input = json!({});
    if let Some(value) = flags.opts.get("--captain") {
        input["captainSessionId"] = json!(value);
    }
    if let Some(value) = flags.opts.get("--project") {
        input["projectId"] = json!(value);
    }
    if let Some(value) = flags.opts.get("--cursor") {
        input["cursor"] = json!(value);
    }
    if let Some(value) = flags.opts.get("--limit") {
        input["limit"] = json!(value
            .parse::<u64>()
            .map_err(|_| { CliError::usage("th agents list: --limit must be an integer") })?);
    }
    call_and_render("agents list", "list_agents", input, &flags)
}

fn show(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(args, &[])?;
    let agent = flags.positional(0, "agents show", "<agentSessionId>")?;
    call_and_render(
        "agents show",
        "get_agent",
        json!({"agentSessionId": agent}),
        &flags,
    )
}

fn checkpoint(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(args, &["--author", "--stage"])?;
    let agent = flags.positional(0, "agents checkpoint", "<agentSessionId>")?;
    let summary = flags.positional(1, "agents checkpoint", "<summary>")?;
    let author = required(&flags, "--author", "agents checkpoint")?;
    let mut input = json!({
        "agentSessionId": agent,
        "authorSessionId": author,
        "summary": summary,
    });
    if let Some(stage) = flags.opts.get("--stage") {
        input["stage"] = json!(stage);
    }
    call_and_render(
        "agents checkpoint",
        "agent_checkpoint",
        input,
        &flags,
    )
}

fn events(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(args, &["--cursor", "--limit"])?;
    let agent = flags.positional(0, "agents events", "<agentSessionId>")?;
    let mut input = json!({"agentSessionId": agent});
    if let Some(value) = flags.opts.get("--cursor") {
        input["cursor"] = json!(value);
    }
    if let Some(value) = flags.opts.get("--limit") {
        input["limit"] = json!(value
            .parse::<u64>()
            .map_err(|_| { CliError::usage("th agents events: --limit must be an integer") })?);
    }
    call_and_render("agents events", "agent_events", input, &flags)
}

fn required(flags: &Flags, name: &str, command: &str) -> Result<String, CliError> {
    flags
        .opts
        .get(name)
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CliError::usage(format!("th {command}: missing required flag {name}")))
}

fn call_and_render(
    label: &str,
    operation: &str,
    args: Value,
    flags: &Flags,
) -> Result<(), CliError> {
    let result = control::call(&endpoint()?, operation, args)?;
    if flags.json {
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
