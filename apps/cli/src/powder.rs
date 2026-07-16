use std::collections::{HashMap, HashSet};

use serde_json::json;

use super::{control, emit_json_ok, endpoint, CliError};

const OP_APPEND_CREW_POWDER_WORK_LOG: &str = "append_crew_powder_work_log";
const OP_READ_CREW_POWDER_EVIDENCE: &str = "read_crew_powder_evidence";
const OP_COMPLETE_CREW_POWDER: &str = "complete_crew_powder";
const EVIDENCE_DEFAULT_LIMIT: u64 = 20;
const EVIDENCE_MAX_LIMIT: u64 = 20;
const WORK_LOG_MAX_BYTES: usize = 16 * 1024;
const COMPLETION_PROOF_MAX_BYTES: usize = 4096;

pub fn command_label(args: &[String]) -> String {
    match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("work-log"), Some("append")) => "powder work-log append".to_string(),
        (Some("evidence"), _) => "powder evidence".to_string(),
        (Some("complete"), _) => "powder complete".to_string(),
        (Some(sub), _) if !sub.starts_with('-') => format!("powder {sub}"),
        _ => "powder".to_string(),
    }
}

pub fn run(args: &[String]) -> Result<(), CliError> {
    if wants_help(args) {
        print_help();
        return Ok(());
    }
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = remaining(args);
    match sub {
        "work-log" => work_log(rest),
        "evidence" => evidence(rest),
        "complete" => complete(rest),
        "" => Err(CliError::usage(
            "usage: th powder <work-log|evidence|complete> ...",
        )),
        other => Err(CliError::usage(format!(
            "unknown powder subcommand '{other}' (expected work-log|evidence|complete)"
        ))),
    }
}

fn work_log(args: &[String]) -> Result<(), CliError> {
    if wants_help(args) {
        print_work_log_help();
        return Ok(());
    }
    let action = args.first().map(String::as_str).unwrap_or("");
    match action {
        "append" => append_work_log(remaining(args)),
        "" => Err(CliError::usage(
            "usage: th powder work-log append <message> [--json]",
        )),
        other => Err(CliError::usage(format!(
            "unknown powder work-log action '{other}' (expected append)"
        ))),
    }
}

fn append_work_log(args: &[String]) -> Result<(), CliError> {
    if wants_help(args) {
        print_work_log_help();
        return Ok(());
    }
    let flags = StrictFlags::parse(args, &[], &["--json"])?;
    flags.require_positionals(1, "th powder work-log append <message> [--json]")?;
    let message = bounded_nonempty_text(&flags.positionals[0], "message", WORK_LOG_MAX_BYTES)?;
    let result = control::call(
        &endpoint()?,
        OP_APPEND_CREW_POWDER_WORK_LOG,
        json!({ "message": message }),
    )?;
    if flags.json {
        emit_json_ok("powder work-log append", result);
    } else {
        println!("Powder work log appended.");
    }
    Ok(())
}

fn evidence(args: &[String]) -> Result<(), CliError> {
    if wants_help(args) {
        print_evidence_help();
        return Ok(());
    }
    let flags = StrictFlags::parse(args, &["--crew", "--limit"], &["--json"])?;
    flags.require_positionals(
        0,
        "th powder evidence [--crew <session>] [--limit <1-20>] [--json]",
    )?;
    let limit = parse_limit(flags.options.get("--limit"))?;
    let mut args = json!({ "limit": limit });
    if let Some(crew) = flags.options.get("--crew") {
        args["crewSessionId"] = json!(bounded_nonempty_text(crew, "--crew", 256)?);
    }
    let result = control::call(&endpoint()?, OP_READ_CREW_POWDER_EVIDENCE, args)?;
    if flags.json {
        emit_json_ok("powder evidence", result);
    } else {
        println!("{}", serde_json::to_string(&result).unwrap_or_default());
    }
    Ok(())
}

fn complete(args: &[String]) -> Result<(), CliError> {
    if wants_help(args) {
        print_complete_help();
        return Ok(());
    }
    let flags = StrictFlags::parse(args, &["--proof"], &["--json"])?;
    flags.require_positionals(
        1,
        "th powder complete <crew-session> --proof <text> [--json]",
    )?;
    let crew = bounded_nonempty_text(&flags.positionals[0], "crew-session", 256)?;
    let proof = flags.options.get("--proof").ok_or_else(|| {
        CliError::usage("th powder complete: missing required flag --proof <text>")
    })?;
    let proof = bounded_nonempty_text(proof, "--proof", COMPLETION_PROOF_MAX_BYTES)?;
    let result = control::call(
        &endpoint()?,
        OP_COMPLETE_CREW_POWDER,
        json!({ "crewSessionId": crew, "proof": proof }),
    )?;
    if flags.json {
        emit_json_ok("powder complete", result);
    } else {
        println!("Powder card completed with proof for Crew {crew}.");
    }
    Ok(())
}

fn wants_help(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    )
}

fn remaining(args: &[String]) -> &[String] {
    if args.is_empty() {
        &[]
    } else {
        &args[1..]
    }
}

fn bounded_nonempty_text(
    value: &str,
    field: &str,
    maximum_bytes: usize,
) -> Result<String, CliError> {
    if value.trim().is_empty() {
        return Err(CliError::usage(format!("{field} must not be empty")));
    }
    if value.len() > maximum_bytes {
        return Err(CliError::usage(format!(
            "{field} exceeds the {maximum_bytes}-byte UTF-8 limit"
        )));
    }
    Ok(value.to_string())
}

fn parse_limit(value: Option<&String>) -> Result<u64, CliError> {
    let Some(value) = value else {
        return Ok(EVIDENCE_DEFAULT_LIMIT);
    };
    let limit = value.parse::<u64>().map_err(|_| invalid_limit(value))?;
    if !(1..=EVIDENCE_MAX_LIMIT).contains(&limit) {
        return Err(invalid_limit(value));
    }
    Ok(limit)
}

fn invalid_limit(value: &str) -> CliError {
    CliError::usage(format!(
        "--limit expects an integer from 1 to {EVIDENCE_MAX_LIMIT}, got '{value}'"
    ))
}

struct StrictFlags {
    positionals: Vec<String>,
    options: HashMap<String, String>,
    json: bool,
}

impl StrictFlags {
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
                    .filter(|value| !value.starts_with("--"))
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

fn print_help() {
    println!(
        "usage: th powder <command> [args] [flags]\n\
\n\
commands:\n\
  work-log append <message>              append to this Crew run's work log\n\
  evidence [--crew S] [--limit 1-20]    read bounded bound-card/run evidence\n\
  complete <crew-session> --proof TEXT  complete a Crew card as its Captain\n\
\n\
All authority comes from the caller's T-Hub session binding.\n\
Card ids, run ids, Powder profiles, endpoints, and credentials are not accepted."
    );
}

fn print_work_log_help() {
    println!(
        "usage: th powder work-log append <message> [--json]\n\
\n\
Append one non-empty message up to 16 KiB UTF-8 to the calling Crew session's Powder work log."
    );
}

fn print_evidence_help() {
    println!(
        "usage: th powder evidence [--crew <session>] [--limit <1-20>] [--json]\n\
\n\
Read deterministic bounded card and run evidence for the calling Crew binding.\n\
A Captain may select one Crew it owns with --crew. The default and maximum limit is 20."
    );
}

fn print_complete_help() {
    println!(
        "usage: th powder complete <crew-session> --proof <text> [--json]\n\
\n\
Complete the Crew-bound Powder card with non-empty proof up to 4096 UTF-8 bytes.\n\
The backend requires the caller to be the Crew session's owning Captain."
    );
}
