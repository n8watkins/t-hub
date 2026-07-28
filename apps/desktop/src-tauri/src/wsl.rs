use std::process::Command;

/// Build a Windows command that checks one exact executable inside WSL.
///
/// Use `-e` with direct argv forwarding.
/// The `wsl.exe -- bash -c <script> <arg0> <arg1>` form can flatten the command
/// line and lose Bash positional arguments, which makes a valid executable fail
/// a `test -x "$1"` probe.
#[cfg(any(windows, test))]
pub(crate) fn executable_probe_command(distro: &str, path: &str) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "-e", "test", "-x", path]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_probe_forwards_the_path_without_a_shell() {
        let command = executable_probe_command(
            "Ubuntu-24.04",
            "/home/test user/.local/lib/t-hub/agents/digest/t-hub-agent",
        );
        let args: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert_eq!(command.get_program(), "wsl.exe");
        assert_eq!(
            args,
            [
                "-d",
                "Ubuntu-24.04",
                "-e",
                "test",
                "-x",
                "/home/test user/.local/lib/t-hub/agents/digest/t-hub-agent",
            ]
        );
        assert!(!args.iter().any(|argument| argument == "bash"));
    }
}
