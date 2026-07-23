//! Reachability-only compatibility helpers for the existing Preview webview.
//!
//! Lifecycle ownership lives exclusively in `preview::PreviewService`.

#[cfg(windows)]
fn wsl_host_ip() -> Option<String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let mut command = Command::new("wsl.exe");
    command
        .arg("-d")
        .arg(crate::files::host_distro())
        .arg("-e")
        .arg("bash")
        .arg("-lc")
        .arg("hostname -I");
    command.creation_flags(0x0800_0000);
    let output =
        crate::bounded_exec::output_with_timeout(command, crate::bounded_exec::WSL_PROBE_TIMEOUT)
            .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.split_whitespace().next()?.trim();
    if first.is_empty() || first.starts_with("127.") || !first.contains('.') {
        return None;
    }
    Some(first.to_string())
}

#[tauri::command]
pub async fn preview_host() -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        use std::sync::OnceLock;
        static CACHE: OnceLock<Option<String>> = OnceLock::new();
        Ok(CACHE.get_or_init(wsl_host_ip).clone())
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

fn tcp_reachable(host: &str, port: u16, timeout_ms: u64) -> Result<bool, String> {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    let host = host.trim();
    if host.is_empty() {
        return Err("empty host".into());
    }
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("could not resolve {host}:{port}: {error}"))?;
    let budget = Duration::from_millis(timeout_ms.clamp(50, 10_000));
    for address in addresses {
        if TcpStream::connect_timeout(&address, budget).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[tauri::command]
pub async fn probe_tcp(host: String, port: u16, timeout_ms: u64) -> Result<bool, String> {
    tcp_reachable(&host, port, timeout_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_probe_reports_a_live_loopback_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_reachable("127.0.0.1", port, 250).unwrap());
        assert!(tcp_reachable("", port, 250).is_err());
    }
}
