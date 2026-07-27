import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  statSync,
} from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const desktopDir = resolve(scriptDir, "..");
const tauriDir = resolve(desktopDir, "src-tauri");
const resourcePath = resolve(tauriDir, "resources", "t-hub-agent");
const targetDir = resolve(tauriDir, "target-agent-resource");
const builtAgentPath = resolve(targetDir, "release", "t-hub-agent");

function fail(message) {
  throw new Error(`prepare-agent-resource: ${message}`);
}

function validateLinuxAgent(path) {
  const metadata = statSync(path);
  if (!metadata.isFile() || metadata.size < 64 * 1024) {
    fail(`expected a non-empty Linux helper at ${path}`);
  }

  const header = readFileSync(path).subarray(0, 20);
  const isElf64X64 =
    header.length >= 20 &&
    header[0] === 0x7f &&
    header[1] === 0x45 &&
    header[2] === 0x4c &&
    header[3] === 0x46 &&
    header[4] === 2 &&
    header[5] === 1 &&
    header[18] === 0x3e &&
    header[19] === 0;
  if (!isElf64X64) {
    fail(`expected an x86-64 Linux ELF helper at ${path}`);
  }
}

function run(program, args, options = {}) {
  const result = spawnSync(program, args, {
    cwd: desktopDir,
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit",
    env: options.env ?? process.env,
  });
  if (result.error) {
    fail(`${program} failed to start: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const detail = options.capture
      ? `: ${(result.stderr || result.stdout || "").trim()}`
      : "";
    fail(`${program} exited with status ${result.status}${detail}`);
  }
  return options.capture ? result.stdout.trim() : "";
}

function wslPath(windowsPath) {
  const output = run(
    "wsl.exe",
    ["-e", "wslpath", "-u", windowsPath],
    { capture: true },
  );
  if (!output.startsWith("/") || output.includes("\n") || output.includes("\r")) {
    fail(`WSL returned an invalid path for ${windowsPath}`);
  }
  return output;
}

function buildOnWindows() {
  const tauriWsl = wslPath(tauriDir);
  const targetWsl = wslPath(targetDir);
  const buildScript =
    'set -eu; unset CARGO_HOME RUSTUP_HOME RUSTC RUSTDOC RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER; cd "$1"; CARGO_TARGET_DIR="$2" cargo build --locked --release -p t-hub-agent';
  run("wsl.exe", [
    "-e",
    "bash",
    "-lc",
    buildScript,
    "t-hub-agent-build",
    tauriWsl,
    targetWsl,
  ]);
}

function buildOnLinux() {
  run(
    "cargo",
    [
      "build",
      "--locked",
      "--release",
      "-p",
      "t-hub-agent",
      "--manifest-path",
      resolve(tauriDir, "Cargo.toml"),
    ],
    {
      env: {
        ...process.env,
        CARGO_TARGET_DIR: targetDir,
      },
    },
  );
}

mkdirSync(dirname(resourcePath), { recursive: true });

const suppliedAgent = process.env.T_HUB_AGENT_RESOURCE_SOURCE;
if (suppliedAgent) {
  const source = resolve(suppliedAgent);
  validateLinuxAgent(source);
  copyFileSync(source, resourcePath);
} else if (process.platform === "win32") {
  buildOnWindows();
  validateLinuxAgent(builtAgentPath);
  copyFileSync(builtAgentPath, resourcePath);
} else if (process.platform === "linux") {
  buildOnLinux();
  validateLinuxAgent(builtAgentPath);
  copyFileSync(builtAgentPath, resourcePath);
} else {
  fail(
    "release packaging requires Linux, Windows with WSL, or T_HUB_AGENT_RESOURCE_SOURCE",
  );
}

chmodSync(resourcePath, 0o755);
validateLinuxAgent(resourcePath);
console.log(`Prepared bundled WSL helper: ${resourcePath}`);
