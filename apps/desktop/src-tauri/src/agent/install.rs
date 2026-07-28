//! Release-time WSL helper deployment.
//!
//! The Windows package carries the exact Linux `t-hub-agent` built from the
//! same source tree as the desktop executable. Before the bridge connects, the
//! helper is hash-compared with a digest-versioned executable in the configured
//! distro and atomically installed when it is absent or damaged.

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

#[cfg(any(windows, test))]
const INSTALLED_DIGEST_SCRIPT: &str = r#"
set -eu
expected=$1
install_root="$HOME/.local/lib/t-hub/agents"
install_dir="$install_root/$expected"
target="$install_dir/t-hub-agent"
for directory in \
  "$HOME/.local" \
  "$HOME/.local/lib" \
  "$HOME/.local/lib/t-hub" \
  "$install_root" \
  "$install_dir"
do
  if [ -L "$directory" ]; then
    printf '%s\n' "helper install path cannot contain a symbolic-link directory" >&2
    exit 18
  fi
done
if [ ! -L "$target" ] && [ -f "$target" ] && [ -x "$target" ]; then
  digest=$(sha256sum -- "$target")
  digest=${digest%% *}
  if [ "$digest" = "$expected" ]; then
    printf '%s\n%s\n' "$digest" "$target"
  fi
fi
"#;

#[cfg(any(windows, test))]
const INSTALL_SCRIPT: &str = r#"
set -eu
source_path=$1
expected=$2
install_root="$HOME/.local/lib/t-hub/agents"
install_dir="$install_root/$expected"
target="$install_dir/t-hub-agent"
stage="$install_dir/.t-hub-agent-stage-$$"

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

for directory in \
  "$HOME/.local" \
  "$HOME/.local/lib" \
  "$HOME/.local/lib/t-hub" \
  "$install_root" \
  "$install_dir"
do
  if [ -L "$directory" ]; then
    printf '%s\n' "helper install path cannot contain a symbolic-link directory" >&2
    exit 16
  fi
done
mkdir -p -- "$install_dir"
for directory in \
  "$HOME/.local" \
  "$HOME/.local/lib" \
  "$HOME/.local/lib/t-hub" \
  "$install_root" \
  "$install_dir"
do
  if [ -L "$directory" ]; then
    printf '%s\n' "helper install path gained a symbolic-link directory" >&2
    exit 17
  fi
done
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
if [ -L "$target" ] || [ ! -f "$target" ] || [ ! -x "$target" ]; then
  printf '%s\n' "installed helper is not an executable regular file" >&2
  exit 15
fi
printf '%s\n%s\n' "$final_digest" "$target"
"#;

#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeployOutcome {
    AlreadyCurrent,
    Installed,
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeployedAgent {
    pub(crate) outcome: DeployOutcome,
    pub(crate) wsl_path: String,
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
fn normalized_deployment(stdout: &[u8], expected_digest: &str) -> Result<Option<String>> {
    let value = std::str::from_utf8(stdout)
        .context("WSL helper deployment output was not UTF-8")?
        .trim();
    if value.is_empty() {
        return Ok(None);
    }
    let mut lines = value.lines();
    let digest = lines.next().unwrap_or_default();
    let path = lines.next().unwrap_or_default();
    if lines.next().is_some()
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("WSL helper deployment output was malformed");
    }
    let digest = digest.to_ascii_lowercase();
    if digest != expected_digest {
        bail!("WSL helper deployment reported an unexpected digest");
    }
    let expected_suffix = format!("/.local/lib/t-hub/agents/{digest}/t-hub-agent");
    if !path.starts_with('/')
        || path.len() > 4096
        || path.chars().any(|character| character.is_control())
        || path.contains('\\')
        || path
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || part == "." || part == "..")
        || !path.ends_with(&expected_suffix)
        || path.len() == expected_suffix.len()
    {
        bail!("WSL helper deployment reported an invalid executable path");
    }
    Ok(Some(path.to_string()))
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
pub(crate) fn deploy_bundled_agent(distro: &str, resource: &Path) -> Result<DeployedAgent> {
    let expected_digest = bundled_agent_digest(resource)?;

    let mut probe = wsl_bash_command(distro, INSTALLED_DIGEST_SCRIPT);
    probe.arg(&expected_digest);
    let installed_stdout = run_checked(probe, PROBE_TIMEOUT, "probing the installed WSL helper")?;
    if let Some(wsl_path) = normalized_deployment(&installed_stdout, &expected_digest)? {
        return Ok(DeployedAgent {
            outcome: DeployOutcome::AlreadyCurrent,
            wsl_path,
        });
    }

    let source_path = wsl_resource_path(distro, resource)?;
    let mut install = wsl_bash_command(distro, INSTALL_SCRIPT);
    install.arg(source_path).arg(&expected_digest);
    let installed_stdout = run_checked(
        install,
        INSTALL_TIMEOUT,
        "installing the bundled WSL helper",
    )?;
    let wsl_path = normalized_deployment(&installed_stdout, &expected_digest)?
        .context("installed WSL helper did not report its verified path")?;

    Ok(DeployedAgent {
        outcome: DeployOutcome::Installed,
        wsl_path,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        process::{Command, Stdio},
    };

    #[test]
    fn deployment_result_preserves_outcome_and_verified_path() {
        for outcome in [DeployOutcome::AlreadyCurrent, DeployOutcome::Installed] {
            let deployed = DeployedAgent {
                outcome,
                wsl_path: "/home/test/.local/lib/t-hub/agents/digest/t-hub-agent".to_string(),
            };
            assert_eq!(deployed.outcome, outcome);
            assert!(deployed.wsl_path.ends_with("/digest/t-hub-agent"));
        }
    }

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

        let installed = home
            .join(".local/lib/t-hub/agents")
            .join(&expected)
            .join("t-hub-agent");
        assert_eq!(fs::read(&installed).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            normalized_deployment(&output.stdout, &expected).unwrap(),
            Some(installed.display().to_string())
        );
        assert!(fs::read_dir(installed.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".t-hub-agent-stage-")));
    }

    #[test]
    fn separately_verified_helper_versions_cannot_replace_each_other() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let first_source = temp.path().join("bundled-agent-first");
        let second_source = temp.path().join("bundled-agent-second");
        let first_bytes = fake_elf(b"first release");
        let second_bytes = fake_elf(b"second release");
        let first_digest = format!("{:x}", Sha256::digest(&first_bytes));
        let second_digest = format!("{:x}", Sha256::digest(&second_bytes));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(&first_source, &first_bytes).unwrap();
        std::fs::write(&second_source, &second_bytes).unwrap();

        let install = |source: &Path, digest: &str| {
            Command::new("bash")
                .args([
                    "-c",
                    INSTALL_SCRIPT,
                    "t-hub-agent-installer",
                    source.to_str().unwrap(),
                    digest,
                ])
                .env("HOME", &home)
                .stdin(Stdio::null())
                .output()
                .unwrap()
        };
        let first_output = install(&first_source, &first_digest);
        let second_output = install(&second_source, &second_digest);
        assert!(first_output.status.success());
        assert!(second_output.status.success());
        let first_verified_path = normalized_deployment(&first_output.stdout, &first_digest)
            .unwrap()
            .unwrap();
        let second_verified_path = normalized_deployment(&second_output.stdout, &second_digest)
            .unwrap()
            .unwrap();

        assert_ne!(first_verified_path, second_verified_path);
        assert_eq!(std::fs::read(first_verified_path).unwrap(), first_bytes);
        assert_eq!(std::fs::read(second_verified_path).unwrap(), second_bytes);
    }

    #[test]
    fn install_script_preserves_the_existing_helper_on_digest_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let expected = "0".repeat(64);
        let install_dir = home.join(".local/lib/t-hub/agents").join(&expected);
        let target = install_dir.join("t-hub-agent");
        let source = temp.path().join("bundled-agent");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(&source, fake_elf(b"candidate")).unwrap();
        fs::write(&target, b"existing helper").unwrap();

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

        assert!(!output.status.success());
        assert_eq!(fs::read(target).unwrap(), b"existing helper");
    }

    #[test]
    fn install_script_refuses_symbolic_link_directories() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let redirected = temp.path().join("redirected-agents");
        let source = temp.path().join("bundled-agent");
        let bytes = fake_elf(b"candidate");
        let expected = format!("{:x}", Sha256::digest(&bytes));
        fs::create_dir_all(home.join(".local/lib/t-hub")).unwrap();
        fs::create_dir_all(&redirected).unwrap();
        fs::write(&source, bytes).unwrap();
        symlink(&redirected, home.join(".local/lib/t-hub/agents")).unwrap();

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

        assert!(!output.status.success());
        assert!(fs::read_dir(redirected).unwrap().next().is_none());
    }

    #[test]
    fn installed_probe_accepts_only_executable_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let source = temp.path().join("bundled-agent");
        let bytes = fake_elf(b"matching release");
        let expected = format!("{:x}", Sha256::digest(&bytes));
        let install_dir = home.join(".local/lib/t-hub/agents").join(&expected);
        let target = install_dir.join("t-hub-agent");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(&target, &bytes).unwrap();

        let probe = || {
            Command::new("bash")
                .args([
                    "-c",
                    INSTALLED_DIGEST_SCRIPT,
                    "t-hub-agent-installer",
                    &expected,
                ])
                .env("HOME", &home)
                .stdin(Stdio::null())
                .output()
                .unwrap()
        };

        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            normalized_deployment(&probe().stdout, &expected).unwrap(),
            None
        );

        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            normalized_deployment(&probe().stdout, &expected).unwrap(),
            Some(target.display().to_string())
        );

        fs::write(&source, &bytes).unwrap();
        fs::remove_file(&target).unwrap();
        symlink(&source, &target).unwrap();
        assert_eq!(
            normalized_deployment(&probe().stdout, &expected).unwrap(),
            None
        );
    }

    #[test]
    fn deployment_output_requires_the_exact_digest_versioned_path() {
        let digest = "a".repeat(64);
        let valid =
            format!("{digest}\n/home/natkins/.local/lib/t-hub/agents/{digest}/t-hub-agent\n");
        assert_eq!(
            normalized_deployment(valid.as_bytes(), &digest).unwrap(),
            Some(format!(
                "/home/natkins/.local/lib/t-hub/agents/{digest}/t-hub-agent"
            ))
        );
        assert!(normalized_deployment(
            format!(
                "{digest}\n/home/natkins/.local/lib/t-hub/agents/{}/t-hub-agent\n",
                "b".repeat(64)
            )
            .as_bytes(),
            &digest
        )
        .is_err());
        assert!(normalized_deployment(
            format!(
                "{digest}\n/home/natkins/../root/.local/lib/t-hub/agents/{digest}/t-hub-agent\n"
            )
            .as_bytes(),
            &digest
        )
        .is_err());
        assert!(normalized_deployment(
            format!("{digest}\nC:\\Users\\natha\\t-hub-agent\n").as_bytes(),
            &digest
        )
        .is_err());
    }

    #[test]
    fn resource_path_is_stable_inside_the_tauri_resource_directory() {
        assert_eq!(
            bundled_agent_path(Path::new("C:/Program Files/T-Hub")),
            Path::new("C:/Program Files/T-Hub/resources/t-hub-agent")
        );
    }
}
