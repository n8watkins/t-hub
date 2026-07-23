//! Canonical bounded Linux listener ownership inspection.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerOwnership {
    pub process_group_id: u32,
    pub process_group_started_at: u64,
}

const MAX_TABLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STAT_BYTES: usize = 4096;
const MAX_PROCESSES: usize = 32_768;
const MAX_FDS: usize = 4096;

pub fn listener_ownership(port: u16) -> Result<Option<ListenerOwnership>, String> {
    let view = LinuxProcView {
        root: Path::new("/proc"),
        expected_uid: unsafe { libc::geteuid() },
    };
    listener_ownership_with(&view, port)
}

pub fn process_group_identity_for_pid(pid: u32) -> Result<ListenerOwnership, String> {
    let view = LinuxProcView {
        root: Path::new("/proc"),
        expected_uid: unsafe { libc::geteuid() },
    };
    let process =
        process_identity(&view, pid)?.ok_or("Preview listener process disappeared during scan")?;
    process_group_identity_with(&view, &process)
}

trait ProcView {
    fn read(&self, relative: &str, max: usize) -> Result<Option<Vec<u8>>, String>;
    fn pids(&self, max: usize) -> Result<Vec<u32>, String>;
    fn fd_targets(&self, pid: u32, max: usize) -> Result<Option<Vec<Option<String>>>, String>;
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

    fn fd_targets(&self, pid: u32, max: usize) -> Result<Option<Vec<Option<String>>>, String> {
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
            if result.len() == max {
                return Err("Preview file descriptor enumeration exceeds its bound".into());
            }
            let target = entry
                .ok()
                .and_then(|entry| std::fs::read_link(entry.path()).ok())
                .map(|target| target.to_string_lossy().into_owned());
            result.push(target);
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
    let sockets = parse_listener_sockets(&tcp, &tcp6, port)?;
    if sockets.is_empty() {
        return Ok(None);
    }
    if sockets
        .iter()
        .any(|socket| socket.uid != view.expected_uid())
    {
        return Err("Preview listener table contains another WSL uid".into());
    }
    let inodes = sockets
        .into_iter()
        .map(|socket| socket.inode)
        .collect::<BTreeSet<_>>();
    let mut inode_owners = BTreeMap::<String, BTreeSet<(u32, u64)>>::new();
    for pid in view.pids(MAX_PROCESSES)? {
        let Some(targets) = view.fd_targets(pid, MAX_FDS)? else {
            continue;
        };
        if matched_listener_inodes(&targets, &inodes).is_empty() {
            continue;
        }
        let before =
            process_identity(view, pid)?.ok_or("Preview listener owner disappeared during scan")?;
        if before.uid != view.expected_uid() {
            return Err("Preview listener belongs to another WSL uid".into());
        }
        let before_targets = view
            .fd_targets(pid, MAX_FDS)?
            .ok_or("Preview listener owner descriptors disappeared during scan")?;
        let before_inodes = matched_listener_inodes(&before_targets, &inodes);
        if before_inodes.is_empty() {
            return Err("Preview listener ownership changed during scan".into());
        }
        let identity = process_group_identity_with(view, &before)?;
        let after_targets = view
            .fd_targets(pid, MAX_FDS)?
            .ok_or("Preview listener owner descriptors disappeared during scan")?;
        let after_inodes = matched_listener_inodes(&after_targets, &inodes);
        if after_inodes.is_empty() || after_inodes != before_inodes {
            return Err("Preview listener inode ownership changed during scan".into());
        }
        let after =
            process_identity(view, pid)?.ok_or("Preview listener owner disappeared during scan")?;
        if after != before {
            return Err("Preview listener owner identity changed during scan".into());
        }
        for inode in before_inodes {
            inode_owners
                .entry(inode)
                .or_default()
                .insert((identity.process_group_id, identity.process_group_started_at));
        }
    }
    if inode_owners.keys().cloned().collect::<BTreeSet<_>>() != inodes {
        return Err("Preview listener inode has no stable same-uid owner evidence".into());
    }
    let owners = inode_owners
        .into_values()
        .flatten()
        .collect::<BTreeSet<_>>();
    match owners.into_iter().collect::<Vec<_>>().as_slice() {
        [] => Ok(None),
        [(group, started)] => Ok(Some(ListenerOwnership {
            process_group_id: *group,
            process_group_started_at: *started,
        })),
        _ => Err("Preview listener ownership is ambiguous".into()),
    }
}

fn matched_listener_inodes(
    targets: &[Option<String>],
    inodes: &BTreeSet<String>,
) -> BTreeSet<String> {
    targets
        .iter()
        .flatten()
        .filter_map(|target| {
            target
                .strip_prefix("socket:[")
                .and_then(|value| value.strip_suffix(']'))
        })
        .filter(|inode| inodes.contains(*inode))
        .map(str::to_string)
        .collect()
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ListenerSocket {
    inode: String,
    uid: u32,
}

fn parse_listener_sockets(
    tcp: &[u8],
    tcp6: &[u8],
    port: u16,
) -> Result<BTreeSet<ListenerSocket>, String> {
    let mut result = BTreeSet::new();
    for (table, address_digits) in [(tcp, 8usize), (tcp6, 32usize)] {
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
            let (encoded_address, _) = fields[1]
                .rsplit_once(':')
                .ok_or("Preview listener address is malformed")?;
            if encoded_address.len() != address_digits
                || !encoded_address.bytes().all(|byte| byte.is_ascii_hexdigit())
                || encoded_port.len() != 4
                || !encoded_port.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("Preview listener address is malformed".into());
            }
            let observed_port = u16::from_str_radix(encoded_port, 16)
                .map_err(|_| "Preview listener port is malformed")?;
            let uid = fields[7]
                .parse::<u32>()
                .map_err(|_| "Preview listener socket uid is malformed")?;
            if observed_port == port && fields[3] == "0A" {
                let inode = fields[9];
                let inode = inode
                    .parse::<u64>()
                    .map_err(|_| "Preview listener socket inode is malformed")?;
                result.insert(ListenerSocket {
                    inode: inode.to_string(),
                    uid,
                });
            }
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    uid: u32,
    group: u32,
    started_at: u64,
}

fn process_identity(view: &impl ProcView, pid: u32) -> Result<Option<ProcessIdentity>, String> {
    let Some(uid) = view.uid(pid)? else {
        return Ok(None);
    };
    let Some(process) = stat_fields(view, pid)? else {
        return Ok(None);
    };
    let group = u32::try_from(number(&process, 2, "process group")?)
        .map_err(|_| "Preview listener process group exceeds u32")?;
    let started_at = number(&process, 19, "process start ticks")?;
    Ok(Some(ProcessIdentity {
        pid,
        uid,
        group,
        started_at,
    }))
}

fn process_group_identity_with(
    view: &impl ProcView,
    process: &ProcessIdentity,
) -> Result<ListenerOwnership, String> {
    let leader_before = process_identity(view, process.group)?
        .ok_or("Preview process-group leader disappeared during scan")?;
    if leader_before.uid != view.expected_uid() {
        return Err("Preview process-group leader belongs to another WSL uid".into());
    }
    if leader_before.pid != process.group || leader_before.group != process.group {
        return Err("Preview process-group leader identity changed".into());
    }
    let leader_after = process_identity(view, process.group)?
        .ok_or("Preview process-group leader disappeared during scan")?;
    if leader_after != leader_before {
        return Err("Preview process-group leader changed during scan".into());
    }
    Ok(ListenerOwnership {
        process_group_id: process.group,
        process_group_started_at: leader_before.started_at,
    })
}

fn stat_fields(view: &impl ProcView, pid: u32) -> Result<Option<Vec<String>>, String> {
    let Some(bytes) = view.read(&format!("{pid}/stat"), MAX_STAT_BYTES)? else {
        return Ok(None);
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| "Preview listener process stat is not UTF-8")?;
    let (prefix, rest) = text
        .rsplit_once(") ")
        .ok_or("Preview listener process stat is malformed")?;
    let observed_pid = prefix
        .split_once(" (")
        .and_then(|(value, _)| value.parse::<u32>().ok())
        .ok_or("Preview listener process stat pid is malformed")?;
    if observed_pid != pid {
        return Err("Preview listener process stat pid changed".into());
    }
    Ok(Some(rest.split_whitespace().map(str::to_string).collect()))
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
    use std::cell::RefCell;
    use std::collections::VecDeque;

    type FdTargets = Vec<Option<String>>;
    type ScriptedFdTargets = VecDeque<Option<FdTargets>>;

    #[derive(Default)]
    struct FakeProc {
        files: BTreeMap<String, Vec<u8>>,
        scripted_files: RefCell<BTreeMap<String, VecDeque<Option<Vec<u8>>>>>,
        pids: Vec<u32>,
        fds: BTreeMap<u32, Option<FdTargets>>,
        scripted_fds: RefCell<BTreeMap<u32, ScriptedFdTargets>>,
        uids: BTreeMap<u32, Option<u32>>,
        scripted_uids: RefCell<BTreeMap<u32, VecDeque<Option<u32>>>>,
        expected_uid: u32,
    }

    impl ProcView for FakeProc {
        fn read(&self, relative: &str, max: usize) -> Result<Option<Vec<u8>>, String> {
            let scripted = self
                .scripted_files
                .borrow_mut()
                .get_mut(relative)
                .and_then(VecDeque::pop_front);
            let value = scripted.unwrap_or_else(|| self.files.get(relative).cloned());
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
        fn fd_targets(&self, pid: u32, max: usize) -> Result<Option<Vec<Option<String>>>, String> {
            let value = self
                .scripted_fds
                .borrow_mut()
                .get_mut(&pid)
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| self.fds.get(&pid).cloned().flatten());
            if value.as_ref().is_some_and(|value| value.len() > max) {
                return Err("too many fds".into());
            }
            Ok(value)
        }
        fn uid(&self, pid: u32) -> Result<Option<u32>, String> {
            Ok(self
                .scripted_uids
                .borrow_mut()
                .get_mut(&pid)
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| self.uids.get(&pid).copied().flatten()))
        }
        fn expected_uid(&self) -> u32 {
            self.expected_uid
        }
    }

    fn table(rows: &[(&str, u32, u64)]) -> Vec<u8> {
        let mut result =
            "sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n"
                .to_string();
        for (index, (address, uid, inode)) in rows.iter().enumerate() {
            result.push_str(&format!(
                "{index}: {address} 00000000:0000 0A 0:0 00:0 0 {uid} 0 {inode}\n"
            ));
        }
        result.into_bytes()
    }

    fn stat(pid: u32, group: u64, start: u64) -> Vec<u8> {
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
        view.files
            .insert("net/tcp".into(), table(&[(address, 1000, 55)]));
        view.files.insert("net/tcp6".into(), Vec::new());
        view.files.insert("20/stat".into(), stat(20, 10, 200));
        view.files.insert("10/stat".into(), stat(10, 10, 100));
        view.fds.insert(20, Some(vec![Some("socket:[55]".into())]));
        view.uids.insert(20, Some(1000));
        view.uids.insert(10, Some(1000));
        view
    }

    #[test]
    fn parses_ipv4_and_ipv6_listeners_with_socket_uid() {
        assert_eq!(
            parse_listener_sockets(&table(&[("0100007F:1051", 1000, 11)]), &[], 4177).unwrap(),
            BTreeSet::from([ListenerSocket {
                inode: "11".into(),
                uid: 1000
            }])
        );
        assert_eq!(
            parse_listener_sockets(
                &[],
                &table(&[("00000000000000000000000000000001:1051", 1001, 12)]),
                4177
            )
            .unwrap(),
            BTreeSet::from([ListenerSocket {
                inode: "12".into(),
                uid: 1001
            }])
        );
    }

    #[test]
    fn resolves_same_uid_and_same_group() {
        let mut view = owned_fixture("0100007F:1051");
        view.pids.push(21);
        view.files.insert("21/stat".into(), stat(21, 10, 300));
        view.fds.insert(21, Some(vec![Some("socket:[55]".into())]));
        view.uids.insert(21, Some(1000));
        assert_eq!(
            listener_ownership_with(&view, 4177).unwrap(),
            Some(ListenerOwnership {
                process_group_id: 10,
                process_group_started_at: 100
            })
        );
    }

    #[test]
    fn rejects_hidden_foreign_socket_and_foreign_visible_owner() {
        let mut view = owned_fixture("0100007F:1051");
        view.files.insert(
            "net/tcp".into(),
            table(&[("0100007F:1051", 1000, 55), ("00000000:1051", 2000, 56)]),
        );
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("table contains another"));

        let mut view = owned_fixture("0100007F:1051");
        view.uids.insert(20, Some(2000));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("belongs to another"));
    }

    #[test]
    fn rejects_multiple_process_groups() {
        let mut view = owned_fixture("0100007F:1051");
        view.pids.push(21);
        view.files.insert("21/stat".into(), stat(21, 30, 300));
        view.files.insert("30/stat".into(), stat(30, 30, 400));
        view.fds.insert(21, Some(vec![Some("socket:[55]".into())]));
        view.uids.insert(21, Some(1000));
        view.uids.insert(30, Some(1000));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn rejects_unresolved_same_uid_inode_and_inode_switch() {
        let mut view = owned_fixture("0100007F:1051");
        view.files.insert(
            "net/tcp".into(),
            table(&[("0100007F:1051", 1000, 55), ("00000000:1051", 1000, 56)]),
        );
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("no stable same-uid owner"));

        let view = owned_fixture("0100007F:1051");
        view.scripted_fds.borrow_mut().insert(
            20,
            VecDeque::from([
                Some(vec![Some("socket:[55]".into())]),
                Some(vec![Some("socket:[55]".into())]),
                Some(vec![Some("socket:[56]".into())]),
            ]),
        );
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("inode ownership changed"));
    }

    #[test]
    fn rejects_owner_reuse_disappearance_and_foreign_leader() {
        let view = owned_fixture("0100007F:1051");
        view.scripted_files.borrow_mut().insert(
            "20/stat".into(),
            VecDeque::from([Some(stat(20, 10, 200)), Some(stat(20, 10, 201))]),
        );
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("identity changed"));

        let view = owned_fixture("0100007F:1051");
        view.scripted_uids
            .borrow_mut()
            .insert(20, VecDeque::from([Some(1000), None]));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("disappeared"));

        let mut view = owned_fixture("0100007F:1051");
        view.uids.insert(10, Some(2000));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("leader belongs"));
    }

    #[test]
    fn malformed_shapes_overflow_and_failed_fd_entries_fail_closed() {
        assert!(parse_listener_sockets(b"header\nbad\n", &[], 4177).is_err());
        assert!(parse_listener_sockets(&table(&[("0100007Z:1051", 1000, 55)]), &[], 4177).is_err());
        let malformed_uid = b"header\n0: 0100007F:1051 00000000:0000 0A 0:0 00:0 0 nope 0 55\n";
        assert!(parse_listener_sockets(malformed_uid, &[], 4177).is_err());

        let mut view = owned_fixture("0100007F:1051");
        view.files
            .insert("net/tcp".into(), vec![b'x'; MAX_TABLE_BYTES + 1]);
        assert!(listener_ownership_with(&view, 4177).is_err());

        let mut view = owned_fixture("0100007F:1051");
        view.files
            .insert("20/stat".into(), stat(20, u32::MAX as u64 + 1, 200));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("exceeds u32"));

        let mut view = owned_fixture("0100007F:1051");
        view.fds.insert(20, Some(vec![None; MAX_FDS + 1]));
        assert!(listener_ownership_with(&view, 4177)
            .unwrap_err()
            .contains("too many fds"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_ipv6_and_separate_child_group_ownership_resolve() {
        let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = process_group_identity_for_pid(std::process::id()).unwrap();
        assert_eq!(listener_ownership(port).unwrap(), Some(expected.clone()));
        drop(listener);

        use std::io::BufRead;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let mut command = Command::new("/usr/bin/python3");
        command
            .args([
                "-c",
                "import socket,time; s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); print(s.getsockname()[1],flush=True); time.sleep(30)",
            ])
            .stdout(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = command.spawn().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().parse::<u16>().unwrap();
        let child_identity = process_group_identity_for_pid(child.id()).unwrap();
        assert_ne!(child_identity, expected);
        assert_eq!(listener_ownership(port).unwrap(), Some(child_identity));
        let _ = child.kill();
        let _ = child.wait();
    }
}
