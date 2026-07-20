use serde_json::{json, Value};

use super::{compact, emit_json_ok, endpoint, CliError, Flags};
use crate::control;

pub fn command_label(args: &[String]) -> String {
    match args.first().map(String::as_str) {
        Some("list" | "ls") | None => "history list".to_string(),
        Some("focus") => "history focus".to_string(),
        Some("resume") => "history resume".to_string(),
        Some(sub) if !sub.starts_with('-') => format!("history {sub}"),
        Some(_) => "history list".to_string(),
    }
}

pub fn run(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("list") | Some("ls") => list(&args[1..]),
        Some("focus") => focus(&args[1..]),
        Some("resume") => resume(&args[1..]),
        Some("-h") | Some("--help") | Some("help") => {
            print_help();
            Ok(())
        }
        Some(option) if option.starts_with('-') => list(args),
        None => list(&[]),
        Some(other) => Err(CliError::usage(format!(
            "unknown history command '{other}'. Use list, focus, or resume."
        ))),
    }
}

fn list(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        print_help();
        return Ok(());
    }
    reject_duplicate_value_options(args, &["--query", "--harness", "--limit"])?;
    let flags = Flags::parse(args, &["--query", "--harness", "--limit"])?;
    reject_unknown(
        &flags,
        &["--query", "--harness", "--limit"],
        &["--no-archived"],
    )?;
    if !flags.pos.is_empty() {
        return Err(CliError::usage(
            "th history list does not accept positional arguments",
        ));
    }
    let mut request = json!({
        "includeArchived": !flags.bools.contains("--no-archived")
    });
    if let Some(query) = flags.opts.get("--query") {
        request["query"] = json!(query);
    }
    if let Some(harness) = flags.opts.get("--harness") {
        if !matches!(harness.as_str(), "claude" | "codex") {
            return Err(CliError::usage(
                "--harness must be exactly 'claude' or 'codex'",
            ));
        }
        request["harness"] = json!(harness);
    }
    if let Some(limit) = flags.opts.get("--limit") {
        let limit = limit
            .parse::<usize>()
            .map_err(|_| CliError::usage("--limit must be an integer between 1 and 500"))?;
        if !(1..=500).contains(&limit) {
            return Err(CliError::usage(
                "--limit must be an integer between 1 and 500",
            ));
        }
        request["limit"] = json!(limit);
    } else if flags.all {
        request["limit"] = json!(500);
    }

    let result = control::call(&endpoint()?, "history_list", request)?;
    if flags.json {
        emit_json_ok("history list", result);
    } else {
        print_catalog(&result, flags.all);
    }
    Ok(())
}

fn focus(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        print_help();
        return Ok(());
    }
    let flags = Flags::parse(args, &[])?;
    reject_unknown(&flags, &[], &[])?;
    require_exact_positionals(&flags, "history focus", 1, "<historyId>")?;
    let history_id = flags.pos[0].clone();
    if history_id.trim().is_empty() {
        return Err(CliError::usage("historyId must not be blank"));
    }
    let result = control::call(
        &endpoint()?,
        "history_focus",
        json!({ "historyId": history_id }),
    )?;
    if flags.json {
        emit_json_ok("history focus", result);
    } else {
        println!("focused History conversation: {}", compact(&result));
    }
    Ok(())
}

fn resume(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        print_help();
        return Ok(());
    }
    reject_duplicate_value_options(args, &["--request-id", "--tab"])?;
    let flags = Flags::parse(args, &["--request-id", "--tab"])?;
    reject_unknown(&flags, &["--request-id", "--tab"], &["--confirm"])?;
    require_exact_positionals(&flags, "history resume", 1, "<historyId>")?;
    if flags.pos[0].trim().is_empty() {
        return Err(CliError::usage("historyId must not be blank"));
    }
    if !flags.bools.contains("--confirm") {
        return Err(CliError::gated(
            "history resume changes a running process; pass --confirm to authorize it",
        ));
    }
    let request_id = flags
        .opts
        .get("--request-id")
        .ok_or_else(|| CliError::usage("th history resume requires --request-id <stable-id>"))?;
    if request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-'))
    {
        return Err(CliError::usage(
            "--request-id must be 1-128 ASCII letters, digits, '_', ':', or '-'",
        ));
    }
    let mut request = json!({
        "historyId": flags.pos[0],
        "requestId": request_id,
    });
    if let Some(tab_id) = flags.opts.get("--tab") {
        if tab_id.trim().is_empty() {
            return Err(CliError::usage("--tab must not be empty"));
        }
        request["targetTabId"] = json!(tab_id);
    }
    let result = control::call(&endpoint()?, "history_resume", request).map_err(|error| {
        let mut error = CliError::from(error);
        if error.retryable || matches!(error.kind, "app_down" | "protocol") {
            error.retryable = true;
            error.message = format!(
                "{} Retry this exact History resume with the same --request-id '{}'; do not create a new request ID.",
                error.message, request_id
            );
        }
        error
    })?;
    if flags.json {
        emit_json_ok("history resume", result);
    } else {
        println!("resumed History conversation: {}", compact(&result));
    }
    Ok(())
}

fn help_requested(args: &[String]) -> bool {
    args.iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
}

fn reject_duplicate_value_options(args: &[String], options: &[&str]) -> Result<(), CliError> {
    for option in options {
        let count = args
            .iter()
            .filter(|argument| {
                argument.as_str() == *option
                    || argument
                        .strip_prefix(*option)
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
            .count();
        if count > 1 {
            return Err(CliError::usage(format!(
                "option '{option}' may be provided only once"
            )));
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "th history commands:\n\
  th history [list] [--query TEXT] [--harness claude|codex] [--limit 1..500]\n\
                    [--no-archived] [--all] [--json]\n\
  th history focus <historyId> [--json]\n\
  th history resume <historyId> --request-id ID --confirm [--tab TAB] [--json]"
    );
}

fn reject_unknown(
    flags: &Flags,
    value_options: &[&str],
    boolean_options: &[&str],
) -> Result<(), CliError> {
    if let Some(option) = flags
        .opts
        .keys()
        .find(|option| !value_options.contains(&option.as_str()))
        .or_else(|| {
            flags
                .bools
                .iter()
                .find(|option| !boolean_options.contains(&option.as_str()))
        })
    {
        return Err(CliError::usage(format!("unknown option '{option}'")));
    }
    Ok(())
}

fn require_exact_positionals(
    flags: &Flags,
    command: &str,
    expected: usize,
    usage: &str,
) -> Result<(), CliError> {
    if flags.pos.len() != expected {
        return Err(CliError::usage(format!(
            "th {command} requires exactly {usage}"
        )));
    }
    Ok(())
}

fn print_catalog(result: &Value, all: bool) {
    let entries = result
        .get("entries")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let cap = if all {
        entries.len()
    } else {
        entries.len().min(20)
    };
    let total = result
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or(entries.len() as u64);
    println!("History: {} shown, {total} known", cap);
    for entry in entries.iter().take(cap) {
        let harness = entry
            .get("harness")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let state = entry
            .get("continuityState")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let label = entry
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("Untitled conversation");
        let history_id = entry
            .get("historyId")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("  {harness:<6} {state:<16} {label}");
        println!("         {history_id}");
    }
    if cap < entries.len() {
        println!("  ... {} more (pass --all)", entries.len() - cap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn labels_subcommands() {
        assert_eq!(command_label(&args(&["list"])), "history list");
        assert_eq!(command_label(&[]), "history list");
    }

    #[test]
    fn resume_requires_explicit_confirmation_before_socket_discovery() {
        let error = resume(&args(&["history:v1:one", "--request-id", "request-one"])).unwrap_err();
        assert_eq!(error.kind, "gated");
        assert_eq!(error.code, super::super::exit::GATED);
    }

    #[test]
    fn list_rejects_unknown_flags_before_socket_discovery() {
        let error = list(&args(&["--mystery"])).unwrap_err();
        assert_eq!(error.kind, "usage");
    }

    #[test]
    fn duplicate_value_options_are_rejected() {
        let error = list(&args(&["--limit=10", "--limit", "20"])).unwrap_err();
        assert_eq!(error.kind, "usage");
        assert!(error.message.contains("only once"));
    }

    #[test]
    fn option_first_invocation_routes_to_list() {
        assert_eq!(command_label(&args(&["--json"])), "history list");
        let error = run(&args(&["--mystery"])).unwrap_err();
        assert_eq!(error.kind, "usage");
    }
}
