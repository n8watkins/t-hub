//! Release-time WSL helper deployment.
//!
//! The Windows package carries the exact Linux `t-hub-agent` built from the
//! same source tree as the desktop executable. Before the bridge connects, the
//! helper is hash-compared with `~/.local/bin/t-hub-agent` in the configured
//! distro and atomically replaced when it differs.

use std::path::{Path, PathBuf};

#[cfg(any(windows, test))]
use anyhow::{bail, Context, Result};
#[cfg(any(windows, test))]
use sha2::{Digest, Sha256};

#[cfg(any(windows, test))]
use std::{fs::File, io::Read};
#[cfg(windows)]
use std::{process::Command, time::Duration};

#[cfg(windows)]
use crate::bounded_exec;

#[cfg(any(windows, test))]
const MIN_AGENT_BYTES: u64 = 64 * 1024;
#[cfg(windows)]
const COMMAND_OUTPUT_LIMIT: usize = 32 * 1024;
#[cfg(windows)]
const INSTALL_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(windows)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(windows)]
const INSTALLED_DIGEST_SCRIPT: &str = r#"
set -eu
target="$HOME/.local/bin/t-hub-agent"
if [ -f "$target" ]; then
  digest=$(sha256sum -- "$target")
  printf '%s\n' "${digest%% *}"
fi
"#;

#[cfg(any(windows, test))]
const INSTALL_SCRIPT: &str = r#"
set -eu
source_path=$1
expected=$2
install_dir="$HOME/.local/bin"
target="$install_dir/t-hub-agent"
stage="$install_dir/.t-hub-agent.t-hub-stage-$$"

cleanup() {
  rm -f -- "$stage"
}
trap cleanup EXIT HUP INT TERM

source_digest=$(sha256sum -- "$source_path")
source_digest=${source_digest%% *}
if [ "$source_digest" != "$expected" ]; then
  printf '%s\n' "bundled helper digest mismatch" >&2
  exit 12
fi

mkdir -p -- "$install_dir"
cp -- "$source_path" "$stage"
chmod 0755 "$stage"

stage_digest=$(sha256sum -- "$stage")
stage_digest=${stage_digest%% *}
if [ "$stage_digest" != "$expected" ]; then
  printf '%s\n' "staged helper digest mismatch" >&2
  exit 13
fi

mv -f -- "$stage" "$target"
trap - EXIT HUP INT TERM

final_digest=$(sha256sum -- "$target")
final_digest=${final_digest%% *}
if [ "$final_digest" != "$expected" ]; then
  printf '%s\n' "installed helper digest mismatch" >&2
  exit 14
fi
printf '%s\n' "$final_digest"
"#;

#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeployOutcome {
    AlreadyCurrent,
    Installed,
}

pub(crate) fn bundled_agent_path(resource_dir: &Path) -> PathBuf {
    resource_dir.join("resources").join("t-hub-agent")
}

#[cfg(any(windows, test))]
fn bundled_agent_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("opening bundled WSL helper {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("reading bundled WSL helper metadata {}", path.display()))?;
    if !metadata.is_file() || metadata.len() < MIN_AGENT_BYTES {
        bail!(
            "bundled WSL helper is missing or too small: {}",
            path.display()
        );
    }

    let mut header = [0_u8; 20];
    file.read_exact(&mut header)
        .with_context(|| format!("reading bundled WSL helper header {}", path.display()))?;
    let is_elf64_x64 = header[0..4] == [0x7f, b'E', b'L', b'F']
        && header[4] == 2
        && header[5] == 1
        && header[18..20] == [0x3e, 0];
    if !is_elf64_x64 {
        bail!(
            "bundled WSL helper is not an x86-64 Linux ELF: {}",
            path.display()
        );
    }

    let mut digest = Sha256::new();
    digest.update(header);
    std::io::copy(&mut file, &mut DigestWriter(&mut digest))
        .with_context(|| format!("hashing bundled WSL helper {}", path.display()))?;
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(any(windows, test))]
struct DigestWriter<'a>(&'a mut Sha256);

#[cfg(any(windows, test))]
impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(any(windows, test))]
fn normalized_digest(stdout: &[u8]) -> Result<Option<String>> {
    let value = std::str::from_utf8(stdout)
        .context("WSL helper digest output was not UTF-8")?
        .trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("WSL helper digest output was malformed");
    }
    Ok(Some(value.to_ascii_lowercase()))
}

#[cfg(windows)]
fn configure_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(windows)]
fn wsl_bash_command(distro: &str, script: &str) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args([
        "-d",
        distro,
        "--cd",
        "~",
        "-e",
        "bash",
        "-lc",
        script,
        "t-hub-agent-installer",
    ]);
    configure_no_window(&mut command);
    command
}

#[cfg(windows)]
fn run_checked(command: Command, timeout: Duration, operation: &str) -> Result<Vec<u8>> {
    let output =
        bounded_exec::output_with_timeout_and_limit(command, timeout, COMMAND_OUTPUT_LIMIT)
            .with_context(|| format!("{operation} did not complete"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{operation} failed: {}", stderr.trim());
    }
    Ok(output.stdout)
}

#[cfg(windows)]
fn wsl_resource_path(distro: &str, resource: &Path) -> Result<String> {
    let resource = resource
        .to_str()
        .context("bundled WSL helper path is not valid UTF-8")?;
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "-e", "wslpath", "-u", resource]);
    configure_no_window(&mut command);
    let stdout = run_checked(
        command,
        PROBE_TIMEOUT,
        "resolving the bundled helper path in WSL",
    )?;
    let path = std::str::from_utf8(&stdout)
        .context("WSL helper path output was not UTF-8")?
        .trim();
    if !path.starts_with('/')
        || path.len() > 4096
        || path.chars().any(|character| character.is_control())
    {
        bail!("WSL returned an invalid bundled helper path");
    }
    Ok(path.to_string())
}

#[cfg(windows)]
pub(crate) fn deploy_bundled_agent(distro: &str, resource: &Path) -> Result<DeployOutcome> {
    let expected_digest = bundled_agent_digest(resource)?;

    let installed_stdout = run_checked(
        wsl_bash_command(distro, INSTALLED_DIGEST_SCRIPT),
        PROBE_TIMEOUT,
        "probing the installed WSL helper",
    )?;
    if normalized_digest(&installed_stdout)?.as_deref() == Some(expected_digest.as_str()) {
        return Ok(DeployOutcome::AlreadyCurrent);
    }

    let source_path = wsl_resource_path(distro, resource)?;
    let mut install = wsl_bash_command(distro, INSTALL_SCRIPT);
    install.arg(source_path).arg(&expected_digest);
    let installed_stdout = run_checked(
        install,
        INSTALL_TIMEOUT,
        "installing the bundled WSL helper",
    )?;
    if normalized_digest(&installed_stdout)?.as_deref() != Some(expected_digest.as_str()) {
        bail!("installed WSL helper did not report the bundled digest");
    }

    Ok(DeployOutcome::Installed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };

    fn fake_elf(payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0_u8; MIN_AGENT_BYTES as usize];
        bytes[0..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        bytes[18..20].copy_from_slice(&[0x3e, 0]);
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn validates_and_hashes_the_complete_bundled_agent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t-hub-agent");
        let bytes = fake_elf(b"matching release helper");
        fs::write(&path, &bytes).unwrap();

        assert_eq!(
            bundled_agent_digest(&path).unwrap(),
            format!("{:x}", Sha256::digest(bytes))
        );
    }

    #[test]
    fn rejects_non_linux_or_truncated_resources() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t-hub-agent");
        fs::write(&path, b"not an agent").unwrap();
        assert!(bundled_agent_digest(&path).is_err());

        fs::write(&path, vec![0_u8; MIN_AGENT_BYTES as usize]).unwrap();
        assert!(bundled_agent_digest(&path).is_err());
    }

    #[test]
    fn install_script_replaces_the_helper_atomically_and_verifies_it() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let source = temp.path().join("bundled-agent");
        let bytes = fake_elf(b"new release");
        let expected = format!("{:x}", Sha256::digest(&bytes));
        fs::create_dir_all(&home).unwrap();
        fs::write(&source, &bytes).unwrap();

        let output = Command::new("bash")
            .args([
                "-c",
                INSTALL_SCRIPT,
                "t-hub-agent-installer",
                source.to_str().unwrap(),
                &expected,
            ])
            .env("HOME", &home)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let installed = home.join(".local/bin/t-hub-agent");
        assert_eq!(fs::read(&installed).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(normalized_digest(&output.stdout).unwrap(), Some(expected));
        assert!(!home.join(".local/bin/.t-hub-agent.t-hub-stage").exists());
    }

    #[test]
    fn install_script_preserves_the_existing_helper_on_digest_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let bin = home.join(".local/bin");
        let source = temp.path().join("bundled-agent");
        fs::create_dir_all(&bin).unwrap();
        fs::write(&source, fake_elf(b"candidate")).unwrap();
        fs::write(bin.join("t-hub-agent"), b"existing helper").unwrap();

        let output = Command::new("bash")
            .args([
                "-c",
                INSTALL_SCRIPT,
                "t-hub-agent-installer",
                source.to_str().unwrap(),
                &"0".repeat(64),
            ])
            .env("HOME", &home)
            .stdin(Stdio::null())
            .output()
            .unwrap();

        assert!(!output.status.success());
        assert_eq!(
            fs::read(bin.join("t-hub-agent")).unwrap(),
            b"existing helper"
        );
    }

    #[test]
    fn resource_path_is_stable_inside_the_tauri_resource_directory() {
        assert_eq!(
            bundled_agent_path(Path::new("C:/Program Files/T-Hub")),
            Path::new("C:/Program Files/T-Hub/resources/t-hub-agent")
        );
    }
}
