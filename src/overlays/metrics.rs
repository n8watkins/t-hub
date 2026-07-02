//! Host metrics overlay (T9): the WSL health data.
//!
//! Data: the `host_metrics` control command, polled every 4s (webview parity,
//! `telemetry.ts METRICS_POLL_MS`). NOTE the wire shape is `t_hub_protocol`'s
//! **snake_case** - the one overlay source that is not camelCase.

use serde::Deserialize;

/// Poll cadence (webview `METRICS_POLL_MS`).
pub const METRICS_POLL_MS: u64 = 4_000;
/// A reading older than this renders with a stale marker.
pub const METRICS_STALE_MS: u64 = 15_000;

/// `host_metrics` result (`t_hub_protocol::HostMetrics`, snake_case).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HostMetrics {
    #[serde(default)]
    pub mem_total_kib: u64,
    #[serde(default)]
    pub mem_available_kib: u64,
    #[serde(default)]
    pub swap_total_kib: u64,
    #[serde(default)]
    pub swap_free_kib: u64,
    #[serde(default)]
    pub cpu_count: u32,
    #[serde(default)]
    pub load_avg: [f32; 3],
    #[serde(default)]
    pub process_count: u32,
    #[serde(default)]
    pub distro: Option<String>,
    #[serde(default)]
    pub captured_at_ms: u64,
}

/// One label/value line for the render fn. `color` of `None` means the muted
/// default; warnings carry explicit RGB.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricLine {
    pub label: &'static str,
    pub value: String,
    pub color: Option<(u8, u8, u8)>,
}

const RED: (u8, u8, u8) = (248, 113, 113);
const AMBER: (u8, u8, u8) = (251, 191, 36);

/// Metrics state: latest reading + fetch bookkeeping.
#[derive(Debug, Default)]
pub struct MetricsState {
    latest: Option<HostMetrics>,
    /// Wall time of the last successful fetch (drives the stale marker).
    fetched_at_ms: Option<u64>,
    /// Last fetch error - expected until the agent bridge connects, so it
    /// renders as a gentle hint rather than an alarm.
    pub error: Option<String>,
}

impl MetricsState {
    pub fn fold_metrics(&mut self, m: HostMetrics, now_ms: u64) {
        self.latest = Some(m);
        self.fetched_at_ms = Some(now_ms);
        self.error = None;
    }

    pub fn fold_error(&mut self, err: String) {
        // Keep the last good reading on a transient failure; just note the error.
        self.error = Some(err);
    }

    pub fn latest(&self) -> Option<&HostMetrics> {
        self.latest.as_ref()
    }

    /// True when the newest reading is older than [`METRICS_STALE_MS`].
    pub fn is_stale(&self, now_ms: u64) -> bool {
        match self.fetched_at_ms {
            Some(at) => now_ms.saturating_sub(at) > METRICS_STALE_MS,
            None => false,
        }
    }

    /// RAM used fraction (0..=1) of the latest reading.
    pub fn mem_used_frac(&self) -> Option<f32> {
        let m = self.latest.as_ref()?;
        if m.mem_total_kib == 0 {
            return None;
        }
        Some((m.mem_total_kib - m.mem_available_kib) as f32 / m.mem_total_kib as f32)
    }

    /// One-line summary for a collapsed header: "7.3/15.6G · 0.42".
    pub fn summary(&self) -> Option<String> {
        let m = self.latest.as_ref()?;
        let used = gib(m.mem_total_kib.saturating_sub(m.mem_available_kib));
        Some(format!("{:.1}/{:.1}G · {:.2}", used, gib(m.mem_total_kib), m.load_avg[0]))
    }

    /// The expanded rows, mirroring the webview's `WslHealth` panel: distro, RAM
    /// (thresholded), swap (only when present), load (1/5/15 + saturation), procs.
    pub fn rows(&self) -> Vec<MetricLine> {
        let Some(m) = self.latest.as_ref() else { return Vec::new() };
        let mut lines = Vec::new();
        if let Some(d) = &m.distro {
            lines.push(MetricLine { label: "Distro", value: d.clone(), color: None });
        }
        let used_gib = gib(m.mem_total_kib.saturating_sub(m.mem_available_kib));
        lines.push(MetricLine {
            label: "RAM",
            value: format!("{:.1} / {:.1} GiB", used_gib, gib(m.mem_total_kib)),
            color: mem_color(self.mem_used_frac().unwrap_or(0.0)),
        });
        if m.swap_total_kib > 0 {
            lines.push(MetricLine {
                label: "Swap",
                value: format!(
                    "{:.1} / {:.1} GiB",
                    gib(m.swap_total_kib.saturating_sub(m.swap_free_kib)),
                    gib(m.swap_total_kib)
                ),
                color: None,
            });
        }
        lines.push(MetricLine {
            label: "Load",
            value: format!("{:.2} {:.2} {:.2}", m.load_avg[0], m.load_avg[1], m.load_avg[2]),
            color: load_color(m.load_avg[0], m.cpu_count),
        });
        lines.push(MetricLine {
            label: "Procs",
            value: format!("{} processes ({} cores)", m.process_count, m.cpu_count),
            color: None,
        });
        lines
    }
}

pub fn gib(kib: u64) -> f32 {
    kib as f32 / (1024.0 * 1024.0)
}

/// Webview thresholds: >= 90% used red, >= 75% amber, else muted default.
pub fn mem_color(used_frac: f32) -> Option<(u8, u8, u8)> {
    if used_frac >= 0.9 {
        Some(RED)
    } else if used_frac >= 0.75 {
        Some(AMBER)
    } else {
        None
    }
}

/// Webview threshold: 1-minute load at or past the core count is saturation.
pub fn load_color(load1: f32, cpu_count: u32) -> Option<(u8, u8, u8)> {
    if cpu_count > 0 && load1 / cpu_count as f32 >= 1.0 {
        Some(AMBER)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> HostMetrics {
        // The exact snake_case wire shape host_metrics serializes.
        serde_json::from_value(serde_json::json!({
            "mem_total_kib": 16_777_216u64,     // 16 GiB
            "mem_available_kib": 4_194_304u64,  // 4 GiB free -> 75% used
            "swap_total_kib": 2_097_152u64,
            "swap_free_kib": 2_097_152u64,
            "cpu_count": 8,
            "load_avg": [0.42, 0.38, 0.31],
            "process_count": 213,
            "distro": "Ubuntu 24.04",
            "captured_at_ms": 1_750_000_000_000u64
        }))
        .unwrap()
    }

    #[test]
    fn parses_the_snake_case_wire_shape() {
        let m = metrics();
        assert_eq!(m.cpu_count, 8);
        assert_eq!(m.load_avg[0], 0.42);
        assert_eq!(m.distro.as_deref(), Some("Ubuntu 24.04"));
    }

    #[test]
    fn rows_and_summary_format_gib_and_thresholds() {
        let mut st = MetricsState::default();
        st.fold_metrics(metrics(), 1000);
        let rows = st.rows();
        assert_eq!(rows[0].label, "Distro");
        assert_eq!(rows[1].label, "RAM");
        assert_eq!(rows[1].value, "12.0 / 16.0 GiB");
        assert_eq!(rows[1].color, Some(AMBER)); // exactly 75% used
        assert_eq!(rows[2].label, "Swap");
        assert_eq!(rows[3].label, "Load");
        assert_eq!(rows[3].color, None);
        assert_eq!(st.summary().as_deref(), Some("12.0/16.0G · 0.42"));
    }

    #[test]
    fn threshold_edges() {
        assert_eq!(mem_color(0.74), None);
        assert_eq!(mem_color(0.75), Some(AMBER));
        assert_eq!(mem_color(0.90), Some(RED));
        assert_eq!(load_color(7.9, 8), None);
        assert_eq!(load_color(8.0, 8), Some(AMBER));
        assert_eq!(load_color(1.0, 0), None); // no cores known -> no warning
    }

    #[test]
    fn staleness_and_transient_errors_keep_the_last_reading() {
        let mut st = MetricsState::default();
        assert!(!st.is_stale(1_000_000)); // nothing fetched yet: not "stale"
        st.fold_metrics(metrics(), 1000);
        assert!(!st.is_stale(1000 + METRICS_STALE_MS));
        assert!(st.is_stale(1001 + METRICS_STALE_MS));
        st.fold_error("bridge not connected".to_string());
        assert!(st.latest().is_some());
        assert!(st.error.is_some());
    }
}
