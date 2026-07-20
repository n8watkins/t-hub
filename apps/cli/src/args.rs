use std::collections::{HashMap, HashSet};

use super::CliError;

/// Strict, dependency-free parser for grouped CLI commands.
///
/// Every accepted flag is declared by the caller, duplicate flags are refused,
/// and parsing completes before endpoint discovery or any other side effect.
pub(crate) struct StrictFlags {
    pub(crate) positionals: Vec<String>,
    pub(crate) options: HashMap<String, String>,
    pub(crate) booleans: HashSet<String>,
}

impl StrictFlags {
    pub(crate) fn parse(
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
            booleans,
        })
    }

    pub(crate) fn require_positionals(&self, expected: usize, usage: &str) -> Result<(), CliError> {
        if self.positionals.len() == expected {
            return Ok(());
        }
        Err(CliError::usage(format!(
            "usage: {usage} (expected {expected} positional argument{}, got {})",
            if expected == 1 { "" } else { "s" },
            self.positionals.len()
        )))
    }

    pub(crate) fn json(&self) -> bool {
        self.booleans.contains("--json")
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
