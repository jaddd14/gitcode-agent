//! Status projection; live counters are owned and updated by orchestration.

use super::clip;

/// Provider-independent values for the always-visible live status row.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Current model display name.
    pub model: String,
    /// Current permission mode display name.
    pub permission: String,
    /// Cached git branch name.
    pub branch: String,
    /// Cached working tree dirty flag.
    pub dirty: bool,
    /// Input tokens reported so far.
    pub input: u64,
    /// Output tokens reported so far.
    pub output: u64,
    /// Cache read tokens reported so far.
    pub cache_read: u64,
    /// Cache write tokens reported so far.
    pub cache_write: u64,
    /// Current generation speed; absent when not measured.
    pub tokens_per_second: Option<f64>,
    /// Context occupancy in percent; absent until known.
    pub context_percent: Option<f64>,
    /// Accumulated dollar cost; absent for unknown pricing.
    pub cost_usd: Option<f64>,
    /// Whether an active provider turn is currently running.
    pub busy: bool,
    /// Elapsed wall-clock seconds for the active turn.
    pub elapsed_secs: u64,
    /// Single in-place retry or rate-limit notice line (`▲ ...`).
    pub retry_notice: Option<String>,
    /// Transient toast notification message.
    pub toast: Option<String>,
    /// Number of queued user prompts waiting behind the active turn.
    pub queued: usize,
}
/// Compact real counts without fabricating unknown totals.
pub fn compact(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}
impl Status {
    /// Render safely at the current width, preserving compact counters before metadata.
    pub fn line(&self, width: usize) -> String {
        let rate = self
            .tokens_per_second
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map_or_else(|| "—".into(), |n| format!("{n:.1}"));
        let context = self
            .context_percent
            .filter(|n| n.is_finite())
            .map_or_else(|| "—".into(), |n| format!("{:.0}%", n.clamp(0.0, 100.0)));
        let cost = self
            .cost_usd
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map_or_else(|| "—".into(), |n| format!("${n:.4}"));
        use unicode_width::UnicodeWidthStr;
        let right = format!(
            "ctx {context} {} │ {}{}",
            super::widgets::progress(
                self.context_percent.unwrap_or(0.0) / 100.0,
                8,
                super::widgets::Glyphs::detect()
            ),
            self.branch,
            if self.dirty { "*" } else { "" }
        );
        let parts = [
            format!("✿ {}", self.model),
            format!("{}↑ {}↓", compact(self.input), compact(self.output)),
            format!("кэш {}", compact(self.cache_read)),
            format!("{rate} ток/с"),
            cost,
        ];
        let budget = width.saturating_sub(right.width() + 1);
        let mut left = String::new();
        for part in parts {
            let candidate = if left.is_empty() {
                part
            } else {
                format!("{left} │ {part}")
            };
            if candidate.width() > budget {
                break;
            }
            left = candidate;
        }
        if right.width() >= width {
            return clip(&right, width);
        }
        format!(
            "{}{right}",
            super::widgets::fit(&left, width - right.width())
        )
    }
}
