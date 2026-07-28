use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct WorktreeFixture {
    directory: PathBuf,
    repo: PathBuf,
}

impl WorktreeFixture {
    fn new() -> Self {
        let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("th-worktree-hints-{}-{id}", std::process::id()));
        let repo = directory.join("repo");
        let linked = directory.join("feature");
        std::fs::create_dir_all(&repo).unwrap();

        let git = |cwd: &PathBuf, args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("seed.txt"), "seed").unwrap();
        git(&repo, &["add", "seed.txt"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=T-Hub Test",
                "commit",
                "-qm",
                "seed",
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                linked.to_str().unwrap(),
            ],
        );
        Self { directory, repo }
    }

    fn run(&self, args: &[&str]) -> Output {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let result = match request["command"].as_str().unwrap() {
                    "list_terminals" => serde_json::json!({ "terminals": [] }),
                    "list_worktrees" => serde_json::json!({ "worktrees": [] }),
                    command => panic!("unexpected control command {command}"),
                };
                writeln!(
                    stream,
                    "{}",
                    serde_json::json!({ "ok": true, "result": result })
                )
                .unwrap();
            }
        });
        let output = Command::new(env!("CARGO_BIN_EXE_th"))
            .env("T_HUB_CONTROL_ADDR", address)
            .env("T_HUB_CONTROL_TOKEN", "test-token")
            .env_remove("T_HUB_CONTROL_FILE")
            .env("NO_COLOR", "1")
            .args(args)
            .output()
            .expect("run th");
        server.join().unwrap();
        output
    }
}

impl Drop for WorktreeFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn prune_execution_fails_closed_before_repository_inspection() {
    let output = Command::new(env!("CARGO_BIN_EXE_th"))
        .env_remove("T_HUB_CONTROL_ADDR")
        .env_remove("T_HUB_CONTROL_TOKEN")
        .args(["worktree", "prune", "/does/not/exist", "--yes", "--json"])
        .output()
        .expect("run th");
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stderr.is_empty());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["command"], "worktree prune");
    assert_eq!(response["error"]["kind"], "gated");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("authoritative worktree safety service"));
}

#[test]
fn runtime_hints_describe_only_available_worktree_actions() {
    let fixture = WorktreeFixture::new();
    let repo = fixture.repo.to_str().unwrap();

    let output = fixture.run(&["worktree", "ls", repo]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let listing = String::from_utf8(output.stdout).unwrap();
    assert!(listing.contains("create a backend-guarded worktree"));
    assert!(!listing.contains("recycles a reapable one first"));

    let output = fixture.run(&["worktree", "prune", repo]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("the full lifecycle table"));
    assert!(!report.contains("--yes"));
    assert!(!report.contains("execute this plan"));
}
