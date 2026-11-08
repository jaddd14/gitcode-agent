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
        clip(
            &format!(
                "↑{} ↓{} · {rate} ток/с · кэш {}/{} · контекст {context} · {cost} · {} · {} · {}{}",
                self.input,
                self.output,
                self.cache_read,
                self.cache_write,
                self.model,
                self.permission,
                self.branch,
                if self.dirty { "*" } else { "" }
            ),
            width,
        )
    }
}
