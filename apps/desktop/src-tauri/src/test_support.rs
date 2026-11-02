//! Shared fixtures for unit tests.

use std::path::{Path, PathBuf};

/// Install a long-running executable at `path` and return the shell command
/// that keeps it alive for `seconds`.
///
/// The process name tmux reports (`pane_current_command`, read from argv[0])
/// is the file name of `path`, so a sleeper installed as `codex` passes the
/// harness liveness probe exactly like a real Codex CLI would.
///
/// Tests used to copy `/bin/sleep` under the harness name. Ubuntu 26.04 (the
/// GitHub `ubuntu-latest` image since late 2026) ships uutils coreutils, whose
/// `/bin/sleep` is a multicall binary that picks the utility from argv[0]: a
/// copy named `codex` exits at once with "unknown program 'codex'". Perl ignores
/// argv[0], is Essential on Debian and Ubuntu, and is present on every
/// supported developer host, so a copy of it keeps the requested name and stays
/// alive on both GNU and uutils systems.
pub(crate) fn install_named_sleeper(path: &Path, seconds: u32) -> String {
    std::fs::copy(perl_path(), path).unwrap();
    format!("{} -e 'sleep {seconds}'", path.display())
}

fn perl_path() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
        .map(|directory| directory.join("perl"))
        .find(|candidate| candidate.is_file())
        .expect("perl is required by the tmux harness test fixtures")
}
