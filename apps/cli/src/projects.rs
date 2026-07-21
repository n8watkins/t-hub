//! Project registration and explicit Git initialization adapters.

use serde_json::{json, Value};

use crate::control;
use crate::{compact, emit_json_ok, endpoint, CliError, Flags};

pub fn command_label(args: &[String]) -> String {
    match args.first().map(String::as_str) {
        Some(sub) if !sub.starts_with('-') => format!("projects {sub}"),
        _ => "projects".to_string(),
    }
}

pub fn run(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => list(&args[1..]),
        "register" => register(&args[1..]),
        "init" | "initialize-git" => initialize_git(&args[1..]),
        other => Err(CliError::usage(format!(
            "th projects: unknown subcommand '{other}'; use list, register, or init"
        ))),
    }
}

fn list(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(args, &[])?;
    let result = control::call(&endpoint()?, "list_projects", json!({}))?;
    if flags.json {
        emit_json_ok("projects list", result);
    } else {
        println!("{}", compact(&result));
    }
    Ok(())
}

fn register(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(args, &["--name", "--remote-url"])?;
    let root = flags.positional(0, "projects register", "<rootPath>")?;
    let name = required_name(&flags, "projects register")?;
    let mut input = json!({ "repoRoot": root, "name": name });
    if let Some(remote) = flags.opts.get("--remote-url") {
        input["remoteUrl"] = Value::String(remote.clone());
    }
    let result = control::call(&endpoint()?, "register_project", input)?;
    if flags.json {
        emit_json_ok("projects register", result);
    } else {
        println!("project registered: {}", compact(&result));
    }
    Ok(())
}

fn initialize_git(args: &[String]) -> Result<(), CliError> {
    let flags = Flags::parse(args, &["--name"])?;
    let root = flags.positional(0, "projects init", "<rootPath>")?;
    let name = required_name(&flags, "projects init")?;
    let result = control::call(
        &endpoint()?,
        "initialize_git",
        json!({ "repoRoot": root, "name": name }),
    )?;
    if flags.json {
        emit_json_ok("projects init", result);
    } else {
        println!("Git initialized: {}", compact(&result));
    }
    Ok(())
}

fn required_name(flags: &Flags, command: &str) -> Result<String, CliError> {
    flags
        .opts
        .get("--name")
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or_else(|| CliError::usage(format!("th {command}: --name must be non-empty")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_required_and_nonempty() {
        for args in [
            vec!["/tmp/project".into()],
            vec!["/tmp/project".into(), "--name".into(), " ".into()],
        ] {
            let flags = Flags::parse(&args, &["--name"]).unwrap();
            assert!(required_name(&flags, "projects register").is_err());
        }
    }
}
