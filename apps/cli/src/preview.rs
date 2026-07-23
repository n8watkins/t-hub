//! Provider-neutral Preview lifecycle CLI adapter.

use serde_json::{json, Value};

use crate::control;
use crate::{compact, emit_json_ok, endpoint, CliError, Flags};

pub fn command_label(args: &[String]) -> String {
    match args.first().map(String::as_str) {
        Some(sub) if !sub.starts_with('-') => format!("preview {sub}"),
        _ => "preview".to_string(),
    }
}

pub fn run(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("discover") => discover(&args[1..]),
        Some("status") => scoped_read("status", "preview_status", &args[1..]),
        Some("select") => select(&args[1..]),
        Some("start") => start(&args[1..]),
        Some("stop") => stop(&args[1..]),
        Some("restart") => restart(&args[1..]),
        Some("refresh") => scoped_mutation("refresh", "preview_refresh", &args[1..], false),
        Some("open") => scoped_mutation("open", "preview_open", &args[1..], false),
        Some("-h") | Some("--help") | Some("help") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(CliError::usage(format!(
            "unknown preview command '{other}'. Use discover, status, select, start, stop, restart, refresh, or open."
        ))),
    }
}

fn discover(args: &[String]) -> Result<(), CliError> {
    let flags = parse(args, &[])?;
    require_positionals(&flags, "preview discover", 1, "<rootPath>")?;
    call_and_render(
        &flags,
        "preview discover",
        "preview_discover",
        json!({ "rootPath": flags.pos[0] }),
    )
}

fn scoped_read(action: &str, command: &str, args: &[String]) -> Result<(), CliError> {
    let flags = parse(args, &["--project-id", "--workspace-id"])?;
    require_positionals(
        &flags,
        &format!("preview {action}"),
        0,
        "no positional arguments",
    )?;
    let input = json!({ "scope": scope(&flags)? });
    call_and_render(&flags, &format!("preview {action}"), command, input)
}

fn select(args: &[String]) -> Result<(), CliError> {
    let flags = parse(
        args,
        &[
            "--project-id",
            "--workspace-id",
            "--request-id",
            "--target-id",
            "--fingerprint",
        ],
    )?;
    require_positionals(&flags, "preview select", 1, "<rootPath>")?;
    let input = json!({
        "rootPath": flags.pos[0],
        "target": target_ref(&flags)?,
        "requestId": request_id(&flags)?,
    });
    call_and_render(&flags, "preview select", "preview_select", input)
}

fn start(args: &[String]) -> Result<(), CliError> {
    let flags = parse(
        args,
        &[
            "--project-id",
            "--workspace-id",
            "--request-id",
            "--target-id",
            "--fingerprint",
        ],
    )?;
    require_confirmation(&flags, "start")?;
    require_positionals(&flags, "preview start", 1, "<rootPath>")?;
    let mut input = json!({
        "rootPath": flags.pos[0],
        "scope": scope(&flags)?,
        "requestId": request_id(&flags)?,
    });
    let target_id = flags.opts.get("--target-id");
    let fingerprint = flags.opts.get("--fingerprint");
    match (target_id, fingerprint) {
        (None, None) => {}
        (Some(_), Some(_)) => input["target"] = target_ref(&flags)?,
        _ => {
            return Err(CliError::usage(
                "--target-id and --fingerprint must be provided together",
            ))
        }
    }
    call_and_render(&flags, "preview start", "preview_start", input)
}

fn stop(args: &[String]) -> Result<(), CliError> {
    let flags = parse(
        args,
        &["--project-id", "--workspace-id", "--request-id", "--run-id"],
    )?;
    require_confirmation(&flags, "stop")?;
    require_positionals(&flags, "preview stop", 0, "no positional arguments")?;
    let mut input = json!({
        "scope": scope(&flags)?,
        "requestId": request_id(&flags)?,
    });
    if let Some(run_id) = flags.opts.get("--run-id") {
        input["expectedRunId"] = json!(run_id);
    }
    call_and_render(&flags, "preview stop", "preview_stop", input)
}

fn restart(args: &[String]) -> Result<(), CliError> {
    let flags = parse(args, &["--project-id", "--workspace-id", "--request-id"])?;
    require_confirmation(&flags, "restart")?;
    require_positionals(&flags, "preview restart", 1, "<rootPath>")?;
    call_and_render(
        &flags,
        "preview restart",
        "preview_restart",
        json!({
            "rootPath": flags.pos[0],
            "scope": scope(&flags)?,
            "requestId": request_id(&flags)?,
        }),
    )
}

fn scoped_mutation(
    action: &str,
    command: &str,
    args: &[String],
    confirmation: bool,
) -> Result<(), CliError> {
    let flags = parse(args, &["--project-id", "--workspace-id", "--request-id"])?;
    if confirmation {
        require_confirmation(&flags, action)?;
    }
    require_positionals(
        &flags,
        &format!("preview {action}"),
        0,
        "no positional arguments",
    )?;
    call_and_render(
        &flags,
        &format!("preview {action}"),
        command,
        json!({
            "scope": scope(&flags)?,
            "requestId": request_id(&flags)?,
        }),
    )
}

fn parse(args: &[String], value_options: &[&str]) -> Result<Flags, CliError> {
    let flags = Flags::parse(args, value_options)?;
    if flags.all {
        return Err(CliError::usage(
            "--all is not supported by Preview commands",
        ));
    }
    if let Some(option) = flags
        .opts
        .keys()
        .find(|option| !value_options.contains(&option.as_str()))
        .or_else(|| {
            flags
                .bools
                .iter()
                .find(|option| option.as_str() != "--confirm")
        })
    {
        return Err(CliError::usage(format!("unknown option '{option}'")));
    }
    Ok(flags)
}

fn scope(flags: &Flags) -> Result<Value, CliError> {
    let project_id = required_option(flags, "--project-id")?;
    let mut scope = json!({ "projectId": project_id });
    if let Some(workspace_id) = flags.opts.get("--workspace-id") {
        scope["workspaceId"] = json!(workspace_id);
    }
    Ok(scope)
}

fn target_ref(flags: &Flags) -> Result<Value, CliError> {
    Ok(json!({
        "scope": scope(flags)?,
        "targetId": required_option(flags, "--target-id")?,
        "discoveryFingerprint": required_option(flags, "--fingerprint")?,
    }))
}

fn request_id(flags: &Flags) -> Result<String, CliError> {
    let value = required_option(flags, "--request-id")?;
    if value.len() > 160 {
        return Err(CliError::usage(
            "--request-id must contain between 1 and 160 bytes",
        ));
    }
    Ok(value)
}

fn required_option(flags: &Flags, option: &str) -> Result<String, CliError> {
    flags
        .opts
        .get(option)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or_else(|| CliError::usage(format!("th preview requires {option} <value>")))
}

fn require_confirmation(flags: &Flags, action: &str) -> Result<(), CliError> {
    if !flags.bools.contains("--confirm") {
        return Err(CliError::gated(format!(
            "preview {action} changes a running process; pass --confirm before endpoint discovery"
        )));
    }
    Ok(())
}

fn require_positionals(
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

fn call_and_render(
    flags: &Flags,
    label: &str,
    command: &str,
    input: Value,
) -> Result<(), CliError> {
    let result = control::call(&endpoint()?, command, input)?;
    if flags.json {
        emit_json_ok(label, result);
    } else {
        println!(
            "{}: {}",
            label.trim_start_matches("preview "),
            compact(&result)
        );
    }
    Ok(())
}

fn print_help() {
    println!(
        "th preview commands:\n\
  th preview discover <rootPath> [--json]\n\
  th preview status --project-id ID [--workspace-id ID] [--json]\n\
  th preview select <rootPath> --project-id ID --target-id ID --fingerprint SHA --request-id ID [--json]\n\
  th preview start <rootPath> --project-id ID --request-id ID --confirm [--target-id ID --fingerprint SHA] [--json]\n\
  th preview stop --project-id ID --request-id ID --confirm [--run-id ID] [--json]\n\
  th preview restart <rootPath> --project-id ID --request-id ID --confirm [--json]\n\
  th preview refresh --project-id ID --request-id ID [--json]\n\
  th preview open --project-id ID --request-id ID [--json]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn process_changes_require_confirmation_before_endpoint_discovery() {
        for args in [
            strings(&[
                "start",
                "/tmp/project",
                "--project-id",
                "project-1",
                "--request-id",
                "request-1",
            ]),
            strings(&[
                "stop",
                "--project-id",
                "project-1",
                "--request-id",
                "request-1",
            ]),
            strings(&[
                "restart",
                "/tmp/project",
                "--project-id",
                "project-1",
                "--request-id",
                "request-1",
            ]),
        ] {
            let error = run(&args).expect_err("confirmation gate");
            assert_eq!(error.code, crate::exit::GATED);
            assert_eq!(error.kind, "gated");
        }
    }

    #[test]
    fn target_identity_must_be_complete() {
        let error = start(&strings(&[
            "/tmp/project",
            "--project-id",
            "project-1",
            "--target-id",
            "root:dev",
            "--request-id",
            "request-1",
            "--confirm",
        ]))
        .expect_err("incomplete target identity");
        assert_eq!(error.code, crate::exit::USAGE);
        assert!(error.message.contains("must be provided together"));
    }

    #[test]
    fn scope_shape_is_stable() {
        let flags = parse(
            &strings(&["--project-id", "project-1", "--workspace-id", "workspace-2"]),
            &["--project-id", "--workspace-id"],
        )
        .unwrap();
        assert_eq!(
            scope(&flags).unwrap(),
            json!({"projectId": "project-1", "workspaceId": "workspace-2"})
        );
    }
}
