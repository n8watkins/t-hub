//! Client-side URL reachability probe (T11 preview panel).
//!
//! CLIENT plane on purpose: what matters is whether the URL is reachable from
//! the box the native client runs on (that is where "open in browser" lands),
//! so no server surface is involved. The webview solves the same problem with
//! the Tauri-only `probe_tcp`/`preview_host` commands; natively a plain
//! `TcpStream` + a minimal HTTP/1.1 GET does it without any new dependency.
//!
//! The response parsing is pure and unit-tested; only [`probe_url`] does I/O
//! (the feed calls it from short-lived background threads).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::preview::{LocalUrl, Probe};

/// TCP connect budget (webview `probePreviewReachable` uses 1500ms).
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Whole-response read budget.
pub const READ_TIMEOUT: Duration = Duration::from_millis(1500);
/// Read at most this much of the body while looking for a `<title>`.
const MAX_READ: usize = 64 * 1024;

/// A completed probe: reachability + a page title when one was fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub probe: Probe,
    pub title: Option<String>,
}

/// Probe one local URL: TCP connect, then (http only - there is no TLS client
/// here) a one-shot GET to pull the status line and `<title>`. An https URL
/// that accepts the TCP connect reports `Reachable { status: None }`.
pub fn probe_url(url: &LocalUrl) -> ProbeOutcome {
    let addr = match (url.connect_host(), url.port).to_socket_addrs() {
        Ok(mut addrs) => match addrs.next() {
            Some(a) => a,
            None => return ProbeOutcome { probe: Probe::Refused, title: None },
        },
        Err(_) => return ProbeOutcome { probe: Probe::Refused, title: None },
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) else {
        return ProbeOutcome { probe: Probe::Refused, title: None };
    };
    if url.https {
        return ProbeOutcome { probe: Probe::Reachable { status: None }, title: None };
    }
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let path = if url.path.is_empty() { "/" } else { url.path.as_str() };
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\nAccept: text/html\r\nUser-Agent: t-hub-native-preview\r\n\r\n",
        url.connect_host(),
        url.port
    );
    if stream.write_all(req.as_bytes()).is_err() {
        // Connected but immediately unwritable: still proves a listener.
        return ProbeOutcome { probe: Probe::Reachable { status: None }, title: None };
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    while buf.len() < MAX_READ {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if text.to_ascii_lowercase().contains("</title>") {
                    break;
                }
            }
            Err(_) => break, // timeout or reset: parse what we have
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let (status, title) = parse_http_response(&text);
    ProbeOutcome { probe: Probe::Reachable { status }, title }
}

/// Parse an HTTP/1.x response prefix: status code + `<title>` text (whitespace
/// collapsed). Pure, so the interesting cases unit-test offline.
pub fn parse_http_response(text: &str) -> (Option<u16>, Option<String>) {
    let status = text.lines().next().and_then(|line| {
        let mut parts = line.split_whitespace();
        let proto = parts.next()?;
        if !proto.starts_with("HTTP/") {
            return None;
        }
        parts.next()?.parse::<u16>().ok()
    });
    (status, extract_title(text))
}

fn extract_title(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let after_tag = open + lower[open..].find('>')? + 1;
    let close = lower[after_tag..].find("</title")?;
    let raw = &text[after_tag..after_tag + close];
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_title() {
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><head>\n  <title>\n  My Dev App \n</title></head></html>";
        assert_eq!(parse_http_response(resp), (Some(200), Some("My Dev App".to_string())));
    }

    #[test]
    fn parses_status_without_title_and_attributed_title_tags() {
        let resp = "HTTP/1.1 404 Not Found\r\n\r\nnope";
        assert_eq!(parse_http_response(resp), (Some(404), None));
        let resp = "HTTP/1.0 200 OK\r\n\r\n<TITLE data-x=\"1\">Vite App</TITLE>";
        assert_eq!(parse_http_response(resp), (Some(200), Some("Vite App".to_string())));
    }

    #[test]
    fn garbage_yields_neither() {
        assert_eq!(parse_http_response("SSH-2.0-OpenSSH_9.6"), (None, None));
        assert_eq!(parse_http_response(""), (None, None));
        // Empty title collapses to None.
        assert_eq!(parse_http_response("HTTP/1.1 200 OK\r\n\r\n<title>  </title>").1, None);
    }
}
