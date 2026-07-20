use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::{control, emit_json_ok, endpoint, CliError};

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
        ],
        &["--json"],
    )?;
    flags.require_positionals(0, "th agents start [flags]")?;
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
    call_and_render("agents start", "start_agent", args, &flags)
}

fn list(args: &[String]) -> Result<(), CliError> {
    let flags = AgentFlags::parse(
        args,
        &["--captain", "--project", "--cursor", "--limit"],
        &["--json"],
    )?;
    flags.require_positionals(0, "th agents list [flags]")?;
    if flags.options.get("--captain").is_none() && flags.options.get("--project").is_none() {
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

fn call_and_render(
    label: &str,
    operation: &str,
    args: Value,
    flags: &AgentFlags,
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

struct AgentFlags {
    positionals: Vec<String>,
    options: HashMap<String, String>,
    json: bool,
}

impl AgentFlags {
    fn parse(
        args: &[String],
        value_flags: &[&str],
        boolean_flags: &[&str],
    ) -> Result<Self, CliError> {
        let value_flags: HashSet<&str> = value_flags.iter().copied().collect();
        let boolean_flags: HashSet<&str> = boolean_flags.iter().copied().collect();
        let mut positionals = Vec::new();
        let mut options = HashMap::new();
        let mut booleans = HashSet::new();
        let mut index = 0;
        while index < args.len() {
            let argument = &args[index];
            if let Some((name, value)) = argument.split_once('=') {
                if !name.starts_with("--") || !value_flags.contains(name) {
                    return Err(CliError::usage(format!("unknown flag '{name}'")));
                }
                if value.is_empty() {
                    return Err(CliError::usage(format!("{name} expects a value")));
                }
                insert_once(&mut options, name, value)?;
            } else if value_flags.contains(argument.as_str()) {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| CliError::usage(format!("{argument} expects a value")))?;
                insert_once(&mut options, argument, value)?;
                index += 1;
            } else if boolean_flags.contains(argument.as_str()) {
                if !booleans.insert(argument.clone()) {
                    return Err(CliError::usage(format!(
                        "{argument} may be provided only once"
                    )));
                }
            } else if argument.starts_with('-') {
                return Err(CliError::usage(format!("unknown flag '{argument}'")));
            } else {
                positionals.push(argument.clone());
            }
            index += 1;
        }
        Ok(Self {
            positionals,
            options,
            json: booleans.contains("--json"),
        })
    }

    fn require_positionals(&self, expected: usize, usage: &str) -> Result<(), CliError> {
        if self.positionals.len() == expected {
            return Ok(());
        }
        Err(CliError::usage(format!(
            "usage: {usage} (expected {expected} positional argument{}, got {})",
            if expected == 1 { "" } else { "s" },
            self.positionals.len()
        )))
    }
}

fn insert_once(
    options: &mut HashMap<String, String>,
    name: &str,
    value: &str,
) -> Result<(), CliError> {
    if options
        .insert(name.to_string(), value.to_string())
        .is_some()
    {
        return Err(CliError::usage(format!("{name} may be provided only once")));
    }
    Ok(())
}
