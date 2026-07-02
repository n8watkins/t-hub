//! Preview panel state (T11): localhost dev-URL surfacing per session.
//!
//! gpui 0.2.2 has NO web-content element (no iframe/webview equivalent), so a
//! true embedded preview is not feasible natively - see the §5 T11 entry for
//! the catalogue of what IS. This panel builds the best available: a live
//! per-session list of detected local dev URLs with client-side reachability
//! probes, fetched page titles, and one-click external open.
//!
//! URL sources (both fold through [`PreviewState::fold_urls`]):
//!  - HOST PUSH: the embedding shell forwards each attached tile's
//!    `TermSession::visible_urls()` scan (T6 built that as a public API for
//!    exactly this). Wrap-aware and authoritative for tiles on screen.
//!  - CAPTURE SCAN: the feed polls `read_terminal` (tmux capture text) for
//!    every live session and scans it with the same `term::scan` URL scanner,
//!    covering sessions that are not attached as native tiles. Long URLs that
//!    wrap at the pane edge are a known miss here (capture text carries no
//!    wrap flags); the host-push path handles those.
//!
//! gpui-free: state + reducers + view-models; painting lives in `panels::view`.

use std::collections::HashMap;

use crate::term::scan::scan_urls;

/// Webview parity (`detectedUrls`): keep at most 8 URLs per session,
/// most-recently-seen first.
pub const URL_CAP: usize = 8;

/// A parsed local URL. Only http/https on a loopback-ish host qualifies -
/// "local dev URLs" per the T11 brief, same host set the webview treats as
/// swappable (`localhost`, `127.0.0.1`, `0.0.0.0`, `::1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalUrl {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path + query as written (leading `/`), empty for bare origins.
    pub path: String,
}

impl LocalUrl {
    /// Canonical string form (used as the dedup key).
    pub fn canonical(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}:{}{}", self.host, self.port, self.path)
    }

    /// The URL to hand to the OS opener: `0.0.0.0`/`::` listen-addrs are not
    /// connectable destinations, so they open as `localhost` (the webview
    /// solves the same class of problem with its `preview_host` swap).
    pub fn open_target(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        let host = match self.host.as_str() {
            "0.0.0.0" | "::" => "localhost",
            h => h,
        };
        format!("{scheme}://{host}:{}{}", self.port, self.path)
    }

    /// The host to CONNECT to for probing (same listen-addr swap).
    pub fn connect_host(&self) -> &str {
        match self.host.as_str() {
            "0.0.0.0" | "::" => "127.0.0.1",
            "localhost" => "127.0.0.1",
            h => h,
        }
    }
}

/// Parse `url` (as found by the T6 scanner) into a [`LocalUrl`], rejecting
/// anything that is not http(s) on a local host.
pub fn parse_local_url(url: &str) -> Option<LocalUrl> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(ix) => {
            let (a, p) = rest.split_at(ix);
            let p = if p.starts_with('?') { format!("/{p}") } else { p.to_string() };
            (a, p)
        }
        None => (rest, String::new()),
    };
    // Bracketed IPv6 authority, else host[:port].
    let (host, port) = if let Some(r) = authority.strip_prefix('[') {
        let end = r.find(']')?;
        let host = &r[..end];
        let port = match r[end + 1..].strip_prefix(':') {
            Some(p) => p.parse::<u16>().ok()?,
            None => if https { 443 } else { 80 },
        };
        (host.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()?),
            None => (authority.to_string(), if https { 443 } else { 80 }),
        }
    };
    if host.is_empty() {
        return None;
    }
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "::")
        || host.ends_with(".localhost");
    if !local {
        return None;
    }
    Some(LocalUrl { https, host, port, path })
}

/// Scan a block of terminal text (capture output or any line source) for
/// local dev URLs, oldest-to-newest occurrence order.
pub fn scan_local_urls(text: &str) -> Vec<LocalUrl> {
    let mut out = Vec::new();
    for line in text.lines() {
        for m in scan_urls(line) {
            if let Some(u) = parse_local_url(&m.url) {
                out.push(u);
            }
        }
    }
    out
}

/// Client-side reachability of one URL.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Probe {
    #[default]
    Unknown,
    Probing,
    /// TCP connect succeeded; `status` set when the HTTP HEAD-ish fetch parsed.
    Reachable { status: Option<u16> },
    Refused,
}

/// One detected URL on one session.
#[derive(Debug, Clone, PartialEq)]
pub struct UrlEntry {
    pub url: LocalUrl,
    pub probe: Probe,
    /// `<title>` from the probe fetch, when the page offered one.
    pub title: Option<String>,
    pub last_seen_ms: u64,
}

/// URLs detected on one session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionUrls {
    /// tmux-derived session id (`th_`-stripped, the `list_terminals` `id`).
    pub session: String,
    pub session_title: String,
    pub cwd: String,
    pub live: bool,
    /// Most-recently-seen first, capped at [`URL_CAP`].
    pub urls: Vec<UrlEntry>,
}

/// The preview panel state: one URL list per session.
#[derive(Debug, Default)]
pub struct PreviewState {
    sessions: HashMap<String, SessionUrls>,
    order: Vec<String>,
}

impl PreviewState {
    /// Get-or-create a session's URL list, tracking first-seen order.
    fn session_mut(&mut self, id: &str) -> &mut SessionUrls {
        if !self.sessions.contains_key(id) {
            self.order.push(id.to_string());
            self.sessions.insert(
                id.to_string(),
                SessionUrls { session: id.to_string(), ..Default::default() },
            );
        }
        self.sessions.get_mut(id).expect("just inserted")
    }

    /// Sync session metadata from a `list_terminals` sweep. Sessions that
    /// disappeared keep their URLs but are marked not-live (a dev server the
    /// user closed the terminal of may still be running); they drop entirely
    /// once they have no URLs to show.
    pub fn fold_sessions(&mut self, live: &[(String, String, String)]) {
        for s in self.sessions.values_mut() {
            s.live = false;
        }
        for (id, title, cwd) in live {
            let entry = self.session_mut(id);
            entry.session_title = title.clone();
            entry.cwd = cwd.clone();
            entry.live = true;
        }
        self.order.retain(|id| {
            let keep = self
                .sessions
                .get(id)
                .map(|s| s.live || !s.urls.is_empty())
                .unwrap_or(false);
            if !keep {
                self.sessions.remove(id);
            }
            keep
        });
    }

    /// Fold newly observed URLs for a session (host push or capture scan),
    /// in oldest-to-newest order. Re-seen URLs keep their probe state and
    /// move to the front (newest-first ordering, webview parity), new ones
    /// enter at the front as [`Probe::Unknown`]; the list caps at [`URL_CAP`].
    pub fn fold_urls(&mut self, session: &str, urls: Vec<LocalUrl>, now_ms: u64) {
        if urls.is_empty() {
            return;
        }
        let key = session.strip_prefix("th_").unwrap_or(session).to_string();
        let entry = self.session_mut(&key);
        for url in urls {
            let canon = url.canonical();
            let existing = entry.urls.iter().position(|e| e.url.canonical() == canon);
            let mut e = match existing {
                Some(ix) => entry.urls.remove(ix),
                None => UrlEntry { url, probe: Probe::Unknown, title: None, last_seen_ms: 0 },
            };
            e.last_seen_ms = now_ms;
            entry.urls.insert(0, e);
        }
        entry.urls.truncate(URL_CAP);
    }

    /// URLs whose probe state is still Unknown, marking them Probing.
    /// The feed spawns a probe for each returned `(session, url)`.
    pub fn take_unprobed(&mut self) -> Vec<(String, LocalUrl)> {
        let mut out = Vec::new();
        for id in &self.order {
            if let Some(s) = self.sessions.get_mut(id) {
                for e in s.urls.iter_mut().filter(|e| e.probe == Probe::Unknown) {
                    e.probe = Probe::Probing;
                    out.push((id.clone(), e.url.clone()));
                }
            }
        }
        out
    }

    /// Fold a completed probe.
    pub fn fold_probe(
        &mut self,
        session: &str,
        canonical: &str,
        probe: Probe,
        title: Option<String>,
    ) {
        if let Some(s) = self.sessions.get_mut(session) {
            if let Some(e) = s.urls.iter_mut().find(|e| e.url.canonical() == canonical) {
                e.probe = probe;
                if title.is_some() {
                    e.title = title;
                }
            }
        }
    }

    /// Reset one URL's probe so the next poll re-checks it.
    pub fn reprobe(&mut self, session: &str, canonical: &str) {
        if let Some(s) = self.sessions.get_mut(session) {
            if let Some(e) = s.urls.iter_mut().find(|e| e.url.canonical() == canonical) {
                e.probe = Probe::Unknown;
            }
        }
    }

    /// Sessions that have URLs to show, in first-seen session order.
    pub fn rows(&self) -> Vec<&SessionUrls> {
        self.order
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .filter(|s| !s.urls.is_empty())
            .collect()
    }

    /// Live session ids the feed should capture-scan.
    pub fn scan_targets(&self) -> Vec<String> {
        self.order
            .iter()
            .filter(|id| self.sessions.get(*id).map(|s| s.live).unwrap_or(false))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lu(s: &str) -> LocalUrl {
        parse_local_url(s).expect(s)
    }

    #[test]
    fn parse_accepts_local_hosts_only() {
        assert_eq!(
            lu("http://localhost:3000"),
            LocalUrl { https: false, host: "localhost".into(), port: 3000, path: String::new() }
        );
        assert_eq!(lu("http://127.0.0.1:8787/app").path, "/app");
        assert_eq!(lu("https://localhost").port, 443);
        assert_eq!(lu("http://localhost").port, 80);
        assert_eq!(lu("http://[::1]:5173").host, "::1");
        assert_eq!(lu("http://app.localhost:3000").host, "app.localhost");
        assert!(parse_local_url("http://example.com:3000").is_none());
        assert!(parse_local_url("http://192.168.1.4:3000").is_none());
        assert!(parse_local_url("file:///tmp/x").is_none());
        assert!(parse_local_url("http://localhost:notaport").is_none());
    }

    #[test]
    fn open_and_connect_targets_swap_listen_addrs() {
        let u = lu("http://0.0.0.0:5173/x");
        assert_eq!(u.open_target(), "http://localhost:5173/x");
        assert_eq!(u.connect_host(), "127.0.0.1");
        let v = lu("http://localhost:3000");
        assert_eq!(v.open_target(), "http://localhost:3000");
        assert_eq!(v.connect_host(), "127.0.0.1");
    }

    #[test]
    fn scan_finds_local_urls_and_skips_remote() {
        let text = "ready on http://localhost:3000\nsee https://example.com/docs\nalso http://127.0.0.1:8787/api?x=1 done";
        let urls: Vec<String> = scan_local_urls(text).iter().map(|u| u.canonical()).collect();
        assert_eq!(urls, vec!["http://localhost:3000", "http://127.0.0.1:8787/api?x=1"]);
    }

    #[test]
    fn fold_dedups_caps_and_orders_newest_first() {
        let mut st = PreviewState::default();
        st.fold_urls("th_abc", vec![lu("http://localhost:3000")], 100);
        // 9 more distinct ports; the cap holds the newest 8.
        let batch: Vec<LocalUrl> =
            (1..=9).map(|i| lu(&format!("http://localhost:400{i}"))).collect();
        st.fold_urls("abc", batch, 200);
        let rows = st.rows();
        assert_eq!(rows.len(), 1, "th_ prefix and bare id fold to one session");
        let urls = &rows[0].urls;
        assert_eq!(urls.len(), URL_CAP);
        assert_eq!(urls[0].url.canonical(), "http://localhost:4009", "newest first");
        assert!(
            !urls.iter().any(|u| u.url.canonical() == "http://localhost:3000"),
            "oldest evicted by the cap"
        );

        // Re-seeing an old URL moves it to the front and keeps probe state.
        st.fold_probe("abc", "http://localhost:4002", Probe::Reachable { status: Some(200) }, Some("App".into()));
        st.fold_urls("abc", vec![lu("http://localhost:4002")], 300);
        let rows = st.rows();
        let first = &rows[0].urls[0];
        assert_eq!(first.url.canonical(), "http://localhost:4002");
        assert_eq!(first.probe, Probe::Reachable { status: Some(200) });
        assert_eq!(first.title.as_deref(), Some("App"));
        assert_eq!(first.last_seen_ms, 300);
    }

    #[test]
    fn probe_lifecycle_and_reprobe() {
        let mut st = PreviewState::default();
        st.fold_urls("s1", vec![lu("http://localhost:3000")], 1);
        let unprobed = st.take_unprobed();
        assert_eq!(unprobed.len(), 1);
        assert!(st.take_unprobed().is_empty(), "marked probing, not re-taken");
        st.fold_probe("s1", "http://localhost:3000", Probe::Refused, None);
        assert_eq!(st.rows()[0].urls[0].probe, Probe::Refused);
        st.reprobe("s1", "http://localhost:3000");
        assert_eq!(st.take_unprobed().len(), 1, "reprobe re-arms the poll");
    }

    #[test]
    fn dead_sessions_keep_urls_until_empty() {
        let mut st = PreviewState::default();
        st.fold_sessions(&[
            ("aaa".into(), "th_aaa".into(), "/p1".into()),
            ("bbb".into(), "th_bbb".into(), "/p2".into()),
        ]);
        st.fold_urls("aaa", vec![lu("http://localhost:3000")], 1);
        // Session aaa dies: its URL list survives (dev server may outlive the
        // terminal), but it stops being a scan target. bbb (no URLs) drops.
        st.fold_sessions(&[]);
        let rows = st.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session, "aaa");
        assert!(!rows[0].live);
        assert!(st.scan_targets().is_empty());
    }
}
