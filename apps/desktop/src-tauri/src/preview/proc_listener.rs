//! Bounded Linux listener ownership inspection.

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use super::endpoint::ListenerOwnership;

const MAX_TABLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STAT_BYTES: usize = 4096;
const MAX_PROCESSES: usize = 32_768;
const MAX_FDS: usize = 4096;

pub(crate) fn listener_ownership(port: u16) -> Result<Option<ListenerOwnership>, String> {
    let view = LinuxProcView {
        root: Path::new("/proc"),
        expected_uid: unsafe { libc::geteuid() },
    };
    listener_ownership_with(&view, port)
}

#[cfg(test)]
pub(crate) fn process_group_identity_for_pid(pid: u32) -> Result<ListenerOwnership, String> {
    let view = LinuxProcView {
        root: Path::new("/proc"),
        expected_uid: unsafe { libc::geteuid() },
    };
    process_group_identity_with(&view, pid)
}

trait ProcView {
    fn read(&self, relative: &str, max: usize) -> Result<Option<Vec<u8>>, String>;
    fn pids(&self, max: usize) -> Result<Vec<u32>, String>;
    fn fd_targets(&self, pid: u32, max: usize) -> Result<Option<Vec<String>>, String>;
    fn uid(&self, pid: u32) -> Result<Option<u32>, String>;
    fn expected_uid(&self) -> u32;
}

struct LinuxProcView<'a> {
    root: &'a Path,
    expected_uid: u32,
}

impl ProcView for LinuxProcView<'_> {
    fn read(&self, relative: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
        let path = self.root.join(relative);
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("open {}: {error}", path.display())),
        };
        let metadata = file
            .metadata()
            .map_err(|error| format!("inspect {}: {error}", path.display()))?;
        if metadata.len() > max as u64 {
            return Err(format!("{} exceeds its byte bound", path.display()));
        }
        let mut bytes = Vec::with_capacity((metadata.len() as usize).min(max));
        file.take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if bytes.len() > max {
            return Err(format!("{} exceeds its byte bound", path.display()));
        }
        Ok(Some(bytes))
    }

    fn pids(&self, max: usize) -> Result<Vec<u32>, String> {
        let mut result = Vec::new();
        for entry in std::fs::read_dir(self.root)
            .map_err(|error| format!("enumerate {}: {error}", self.root.display()))?
        {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            if result.len() == max {
                return Err("Preview process enumeration exceeds its bound".into());
            }
            result.push(pid);
        }
        result.sort_unstable();
        Ok(result)
    }

    fn fd_targets(&self, pid: u32, max: usize) -> Result<Option<Vec<String>>, String> {
        let directory = self.root.join(pid.to_string()).join("fd");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(format!("enumerate {}: {error}", directory.display())),
        };
        let mut result = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else { continue };
            if result.len() == max {
                return Err("Preview file descriptor enumeration exceeds its bound".into());
            }
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            result.push(target.to_string_lossy().into_owned());
        }
        Ok(Some(result))
    }

    fn uid(&self, pid: u32) -> Result<Option<u32>, String> {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(self.root.join(pid.to_string())) {
            Ok(metadata) => Ok(Some(metadata.uid())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("inspect Preview listener owner: {error}")),
        }
    }

    fn expected_uid(&self) -> u32 {
        self.expected_uid
    }
}

fn listener_ownership_with(
    view: &impl ProcView,
    port: u16,
) -> Result<Option<ListenerOwnership>, String> {
    if port == 0 {
        return Err("Preview listener port must be nonzero".into());
    }
    let tcp = view
        .read("net/tcp", MAX_TABLE_BYTES)?
        .ok_or("Preview IPv4 listener table disappeared")?;
    let tcp6 = view.read("net/tcp6", MAX_TABLE_BYTES)?.unwrap_or_default();
    let inodes = parse_listener_inodes(&tcp, &tcp6, port)?;
    if inodes.is_empty() {
        return Ok(None);
    }
    let mut owners = BTreeSet::new();
    for pid in view.pids(MAX_PROCESSES)? {
        let Some(targets) = view.fd_targets(pid, MAX_FDS)? else {
            continue;
        };
        let owns = targets.iter().any(|target| {
            target
                .strip_prefix("socket:[")
                .and_then(|value| value.strip_suffix(']'))
                .is_some_and(|inode| inodes.contains(inode))
        });
        if !owns {
            continue;
        }
        let uid = view.uid(pid)?.ok_or("Preview listener owner disappeared")?;
        if uid != view.expected_uid() {
            return Err("Preview listener belongs to another WSL uid".into());
        }
        let identity = process_group_identity_with(view, pid)?;
        owners.insert((identity.process_group_id, identity.process_group_started_at));
    }
    match owners.into_iter().collect::<Vec<_>>().as_slice() {
        [] => Ok(None),
        [(group, started)] => Ok(Some(ListenerOwnership {
            process_group_id: *group,
            process_group_started_at: *started,
        })),
        _ => Err("Preview listener ownership is ambiguous".into()),
    }
}

fn parse_listener_inodes(tcp: &[u8], tcp6: &[u8], port: u16) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    for table in [tcp, tcp6] {
        let text = std::str::from_utf8(table).map_err(|_| "Preview listener table is not UTF-8")?;
        for line in text.lines().skip(1) {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.is_empty() {
                continue;
            }
            if fields.len() < 10 {
                return Err("Preview listener table has a malformed row".into());
            }
            let (_, encoded_port) = fields[1]
                .rsplit_once(':')
                .ok_or("Preview listener address is malformed")?;
            let observed_port = u16::from_str_radix(encoded_port, 16)
                .map_err(|_| "Preview listener port is malformed")?;
            if observed_port == port && fields[3] == "0A" {
                let inode = fields[9];
                if !inode.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("Preview listener socket inode is malformed".into());
                }
                result.insert(inode.to_string());
            }
        }
    }
    Ok(result)
}

fn process_group_identity_with(
    view: &impl ProcView,
    pid: u32,
) -> Result<ListenerOwnership, String> {
    let process = stat_fields(view, pid)?;
    let group = number(&process, 2, "process group")? as u32;
    let leader = stat_fields(view, group)?;
    if number(&leader, 2, "leader process group")? as u32 != group {
        return Err("Preview process-group leader identity changed".into());
    }
    Ok(ListenerOwnership {
        process_group_id: group,
        process_group_started_at: number(&leader, 19, "leader start ticks")?,
    })
}

fn stat_fields(view: &impl ProcView, pid: u32) -> Result<Vec<String>, String> {
    let bytes = view
        .read(&format!("{pid}/stat"), MAX_STAT_BYTES)?
        .ok_or("Preview listener process disappeared")?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| "Preview listener process stat is not UTF-8")?;
    Ok(text
        .rsplit_once(") ")
        .ok_or("Preview listener process stat is malformed")?
        .1
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

fn number(fields: &[String], index: usize, field: &str) -> Result<u64, String> {
    fields
        .get(index)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("Preview listener {field} is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeProc {
        files: BTreeMap<String, Vec<u8>>,
        pids: Vec<u32>,
        fds: BTreeMap<u32, Option<Vec<String>>>,
        uids: BTreeMap<u32, Option<u32>>,
        expected_uid: u32,
    }

    impl ProcView for FakeProc {
        fn read(&self, relative: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
            let value = self.files.get(relative).cloned();
            if value.as_ref().is_some_and(|value| value.len() > max) {
                return Err("oversize".into());
            }
            Ok(value)
        }
        fn pids(&self, max: usize) -> Result<Vec<u32>, String> {
            if self.pids.len() > max {
                return Err("too many pids".into());
            }
            Ok(self.pids.clone())
        }
        fn fd_targets(&self, pid: u32, max: usize) -> Result<Option<Vec<String>>, String> {
            let value = self.fds.get(&pid).cloned().flatten();
            if value.as_ref().is_some_and(|value| value.len() > max) {
                return Err("too many fds".into());
            }
            Ok(value)
        }
        fn uid(&self, pid: u32) -> Result<Option<u32>, String> {
            Ok(self.uids.get(&pid).copied().flatten())
        }
        fn expected_uid(&self) -> u32 {
            self.expected_uid
        }
    }

    fn table(address: &str, inode: u64) -> Vec<u8> {
        format!(
            "sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n0: {address} 00000000:0000 0A 0:0 00:0 0 1000 0 {inode}\n"
        )
        .into_bytes()
    }

    fn stat(pid: u32, group: u32, start: u64) -> Vec<u8> {
        let mut fields = vec![
            "S".to_string(),
            "1".to_string(),
            group.to_string(),
            group.to_string(),
            "0".to_string(),
            "-1".to_string(),
        ];
        fields.resize(19, "0".to_string());
        fields.push(start.to_string());
        format!("{pid} (fixture) {}\n", fields.join(" ")).into_bytes()
    }

    fn owned_fixture(address: &str) -> FakeProc {
        let mut view = FakeProc {
            pids: vec![20],
            expected_uid: 1000,
            ..Default::default()
        };
        view.files.insert("net/tcp".into(), table(address, 55));
        view.files.insert("net/tcp6".into(), Vec::new());
        view.files.insert("20/stat".into(), stat(20, 10, 200));
        view.files.insert("10/stat".into(), stat(10, 10, 100));
        view.fds.insert(20, Some(vec!["socket:[55]".into()]));
        view.uids.insert(20, Some(1000));
        view
    }

    #[test]
    fn parses_ipv4_and_ipv6_listeners() {
        assert_eq!(
            parse_listener_inodes(&table("0100007F:1051", 11), &[], 4177).unwrap(),
            BTreeSet::from(["11".into()])
        );
        assert_eq!(
            parse_listener_inodes(
                &[],
                &table("00000000000000000000000000000001:1051", 12),
                4177
            )
            .unwrap(),
            BTreeSet::from(["12".into()])
        );
    }

    #[test]
    fn resolves_same_uid_and_same_group_but_refuses_foreign_or_multiple_groups() {
        let mut view = owned_fixture("0100007F:1051");
        view.pids.push(21);
        view.files.insert("21/stat".into(), stat(21, 10, 300));
        view.fds.insert(21, Some(vec!["socket:[55]".into()]));
        view.uids.insert(21, Some(1000));
        assert_eq!(
            listener_ownership_with(&view, 4177).unwrap(),
            Some(ListenerOwnership {
                process_group_id: 10,
                process_group_started_at: 100
            })
        );
        view.uids.insert(21, Some(2000));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("another WSL uid"));
        view.uids.insert(21, Some(1000));
        view.files.insert("21/stat".into(), stat(21, 30, 300));
        view.files.insert("30/stat".into(), stat(30, 30, 400));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn malformed_oversize_and_disappearing_evidence_fail_closed() {
        assert!(parse_listener_inodes(b"header\nbad\n", &[], 4177).is_err());
        let mut view = owned_fixture("0100007F:1051");
        view.files
            .insert("net/tcp".into(), vec![b'x'; MAX_TABLE_BYTES + 1]);
        assert!(listener_ownership_with(&view, 4177).is_err());
        let mut view = owned_fixture("0100007F:1051");
        view.uids.insert(20, None);
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("disappeared"));
    }
}
