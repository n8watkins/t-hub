use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::{control, emit_json_ok, endpoint, CliError};

const OP_APPEND_CREW_POWDER_WORK_LOG: &str = "append_crew_powder_work_log";
const OP_READ_CREW_POWDER_EVIDENCE: &str = "read_crew_powder_evidence";
const OP_REVIEW_CREW_POWDER_CRITERION: &str = "review_crew_powder_criterion";
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
        (Some("criterion"), Some("review")) => "powder criterion review".to_string(),
        (Some("complete"), _) => "powder complete".to_string(),
        (Some(sub), _) if !sub.starts_with('-') => format!("powder {sub}"),
        _ => "powder".to_string(),
    }
}

pub fn run(args: &[String]) -> Result<(), CliError> {
    if !args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Err(CliError::powder_retired(
            "Powder commands are retired; use `th agents` instead.",
        ));
    }
    if wants_help(args) {
        print_help();
        return Ok(());
    }
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = remaining(args);
    match sub {
        "work-log" => work_log(rest),
        "evidence" => evidence(rest),
        "criterion" => criterion(rest),
        "complete" => complete(rest),
        "" => Err(CliError::usage(
            "usage: th powder <work-log|evidence|criterion|complete> ...",
        )),
        other => Err(CliError::usage(format!(
            "unknown powder subcommand '{other}' (expected work-log|evidence|criterion|complete)"
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
            "usage: th powder work-log append <message> --operation-id <id> [--json]",
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
    let flags = StrictFlags::parse(args, &["--operation-id"], &["--json"])?;
    flags.require_positionals(
        1,
        "th powder work-log append <message> --operation-id <id> [--json]",
    )?;
    let message = bounded_nonempty_text(&flags.positionals[0], "message", WORK_LOG_MAX_BYTES)?;
    let operation_id = required_operation_id(&flags, "th powder work-log append")?;
    let result = control::call(
        &endpoint()?,
        OP_APPEND_CREW_POWDER_WORK_LOG,
        json!({ "message": message, "operationId": operation_id }),
    )?;
    if flags.json {
        emit_json_ok("powder work-log append", result);
    } else {
        println!("Powder work log appended.");
    }
    Ok(())
}

fn criterion(args: &[String]) -> Result<(), CliError> {
    if wants_help(args) {
        print_criterion_help();
        return Ok(());
    }
    match args.first().map(String::as_str).unwrap_or("") {
        "review" => review_criterion(remaining(args)),
        "" => Err(CliError::usage(
            "usage: th powder criterion review <crew-session> [flags]",
        )),
        other => Err(CliError::usage(format!(
            "unknown powder criterion action '{other}' (expected review)"
        ))),
    }
}

fn review_criterion(args: &[String]) -> Result<(), CliError> {
    if wants_help(args) {
        print_criterion_help();
        return Ok(());
    }
    let flags = StrictFlags::parse(
        args,
        &[
            "--operation-id",
            "--criterion",
            "--criterion-id",
            "--decision",
            "--proof",
            "--expected-reviewer-identity",
        ],
        &["--json"],
    )?;
    flags.require_positionals(1, "th powder criterion review <crew-session> [flags]")?;
    let crew = bounded_nonempty_text(&flags.positionals[0], "crew-session", 256)?;
    let operation_id = required_operation_id(&flags, "th powder criterion review")?;
    let criterion = flags
        .options
        .get("--criterion")
        .ok_or_else(|| CliError::usage("th powder criterion review: missing --criterion <index>"))?
        .parse::<usize>()
        .map_err(|_| CliError::usage("--criterion expects a non-negative integer"))?;
    let criterion_id =
        required_bounded_option(&flags, "--criterion-id", "th powder criterion review", 256)?;
    let decision = required_bounded_option(&flags, "--decision", "th powder criterion review", 16)?;
    if !matches!(decision.as_str(), "approved" | "rejected" | "cleared") {
        return Err(CliError::usage(
            "--decision must be approved, rejected, or cleared",
        ));
    }
    let proof = flags
        .options
        .get("--proof")
        .map(|proof| bounded_nonempty_text(proof, "--proof", COMPLETION_PROOF_MAX_BYTES))
        .transpose()?;
    if decision == "cleared" && proof.is_some() {
        return Err(CliError::usage(
            "--proof is not allowed for a cleared review",
        ));
    }
    if decision != "cleared" && proof.is_none() {
        return Err(CliError::usage(
            "--proof is required for approved or rejected reviews",
        ));
    }
    let legacy_reviewer_label = required_bounded_option(
        &flags,
        "--expected-reviewer-identity",
        "th powder criterion review",
        256,
    )?;
    let result = control::call(
        &endpoint()?,
        OP_REVIEW_CREW_POWDER_CRITERION,
        json!({
            "crewSessionId": crew,
            "operationId": operation_id,
            "criterion": criterion,
            "criterionId": criterion_id,
            "decision": decision,
            "proof": proof,
            "expectedReviewerIdentity": legacy_reviewer_label,
        }),
    )?;
    if flags.json {
        emit_json_ok("powder criterion review", result);
    } else {
        println!("Powder criterion {criterion} reviewed for Crew {crew}.");
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
    let flags = StrictFlags::parse(
        args,
        &["--operation-id", "--proof", "--criterion-proofs-json"],
        &["--confirm", "--json"],
    )?;
    flags.require_positionals(
        1,
        "th powder complete <crew-session> --operation-id <id> --proof <text> --criterion-proofs-json <json> --confirm [--json]",
    )?;
    let crew = bounded_nonempty_text(&flags.positionals[0], "crew-session", 256)?;
    let proof = flags.options.get("--proof").ok_or_else(|| {
        CliError::usage("th powder complete: missing required flag --proof <text>")
    })?;
    let proof = bounded_nonempty_text(proof, "--proof", COMPLETION_PROOF_MAX_BYTES)?;
    let operation_id = required_operation_id(&flags, "th powder complete")?;
    let criterion_proofs =
        parse_criterion_proofs_json(flags.options.get("--criterion-proofs-json").ok_or_else(
            || {
                CliError::usage(
                    "th powder complete: missing required flag --criterion-proofs-json <json>",
                )
            },
        )?)?;
    if !flags.confirm {
        return Err(CliError::gated(
            "th powder complete requires explicit --confirm before any side effect",
        ));
    }
    let result = control::call(
        &endpoint()?,
        OP_COMPLETE_CREW_POWDER,
        json!({
            "crewSessionId": crew,
            "operationId": operation_id,
            "proof": proof,
            "criterionProofs": criterion_proofs,
        }),
    )?;
    if flags.json {
        emit_json_ok("powder complete", result);
    } else {
        println!("Powder card completed with proof for Crew {crew}.");
    }
    Ok(())
}

fn required_operation_id(flags: &StrictFlags, command: &str) -> Result<String, CliError> {
    let operation_id = required_bounded_option(flags, "--operation-id", command, 128)?;
    if !operation_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(CliError::usage(
            "--operation-id accepts only ASCII letters, digits, '-', '_', '.', and ':'",
        ));
    }
    Ok(operation_id)
}

fn required_bounded_option(
    flags: &StrictFlags,
    name: &str,
    command: &str,
    maximum_bytes: usize,
) -> Result<String, CliError> {
    let value = flags.options.get(name).ok_or_else(|| {
        CliError::usage(format!("{command}: missing required flag {name} <value>"))
    })?;
    bounded_nonempty_text(value, name, maximum_bytes)
}

fn parse_criterion_proofs_json(value: &str) -> Result<Value, CliError> {
    let parsed: Value = serde_json::from_str(value)
        .map_err(|_| CliError::usage("--criterion-proofs-json must be a valid JSON array"))?;
    let proofs = parsed
        .as_array()
        .ok_or_else(|| CliError::usage("--criterion-proofs-json must be a JSON array"))?;
    if proofs.len() > 128 {
        return Err(CliError::usage(
            "--criterion-proofs-json accepts at most 128 entries",
        ));
    }
    let mut criteria = HashSet::new();
    for proof in proofs {
        let object = proof
            .as_object()
            .ok_or_else(|| CliError::usage("each criterion proof must be a JSON object"))?;
        let criterion = object.get("criterion").and_then(Value::as_u64);
        let criterion_id = object.get("criterionId").and_then(Value::as_str);
        let url = object.get("url").and_then(Value::as_str);
        if object.len() != 3
            || criterion.is_none()
            || !matches!(
                criterion_id,
                Some(value) if !value.trim().is_empty() && value.len() <= 256
            )
            || !matches!(
                url,
                Some(value) if !value.trim().is_empty() && value.len() <= 4096
            )
        {
            return Err(CliError::usage(
                "criterion proofs require exactly criterion, criterionId, and url",
            ));
        }
        if !criteria.insert(criterion.unwrap()) {
            return Err(CliError::usage(
                "criterion proof indexes may be provided only once",
            ));
        }
    }
    Ok(parsed)
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
    confirm: bool,
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
            confirm: booleans.contains("--confirm"),
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
  work-log append <message>             append to this Crew run's work log\n\
  evidence [--crew S] [--limit 1-20]   read bounded bound-card/run evidence\n\
  criterion review <crew> [flags]      record an exact run-scoped review\n\
  complete <crew-session> [flags]      complete a Crew card as its Captain\n\
\n\
All authority comes from the caller's T-Hub session binding.\n\
Card ids, run ids, Powder profiles, endpoints, and credentials are not accepted."
    );
}

fn print_work_log_help() {
    println!(
        "usage: th powder work-log append <message> --operation-id <id> [--json]\n\
\n\
Append one non-empty message up to 16 KiB UTF-8 to the calling Crew session's Powder work log.\n\
Exact retry must reuse the same operation id and identical message."
    );
}

fn print_criterion_help() {
    println!(
        "usage: th powder criterion review <crew-session> --operation-id <id> --criterion <index> --criterion-id <id> --decision <approved|rejected|cleared> [--proof <text>] --expected-reviewer-identity <id> [--json]\n\
\n\
Record one exact run-scoped criterion review. Approved and rejected decisions require proof.\n\
Cleared decisions must omit proof.\n\
--expected-reviewer-identity is a legacy caller-facing label retained for durable-intent compatibility.\n\
T-Hub verifies the authoritative receipt against the protected Powder profile operationIdentity, not this caller-supplied label."
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
        "usage: th powder complete <crew-session> --operation-id <id> --proof <text> --criterion-proofs-json <json> --confirm [--json]\n\
\n\
Complete the Crew-bound Powder card with non-empty proof up to 4096 UTF-8 bytes.\n\
Criterion proof JSON entries require criterion, criterionId, and url.\n\
Explicit --confirm is mandatory and is validated before endpoint discovery.\n\
The backend requires the caller to be the Crew session's owning Captain."
    );
}
