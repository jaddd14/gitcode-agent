//! Status projection; live counters are owned and updated by orchestration.

use super::clip;

/// Provider-independent values for the always-visible live status row.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Current model display name.
    pub model: String,
    /// Explicit reasoning effort, or none for the provider default.
    pub effort: Option<String>,
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
    /// Output from the current turn, excluding earlier turns.
    pub turn_output: u64,
    /// Whether the displayed live output count includes an estimate.
    pub output_estimated: bool,
    /// Cache read tokens reported so far.
    pub cache_read: u64,
    /// Cache write tokens reported so far.
    pub cache_write: u64,
    /// Current generation speed; absent when not measured.
    pub tokens_per_second: Option<f64>,
    /// Number of currently running background shell commands.
    pub background_jobs: usize,
    /// Context occupancy in percent; absent until known.
    pub context_percent: Option<f64>,
    /// Accumulated dollar cost; absent for unknown pricing.
    pub cost_usd: Option<f64>,
    /// Whether an active provider turn is currently running.
    pub busy: bool,
    /// The active request is summarizing earlier conversation.
    pub compacting: bool,
    /// Elapsed wall-clock seconds for the active turn.
    pub elapsed_secs: u64,
    /// Milliseconds used to animate the active indicator.
    pub elapsed_millis: u64,
    /// Single in-place retry or rate-limit notice line (`▲ ...`).
    pub retry_notice: Option<String>,
    /// Transient toast notification message.
    pub toast: Option<String>,
    /// Number of characters in the recently copied selection, while its notice is visible.
    pub copied_chars: Option<usize>,
    /// Whether entering BYPASS is awaiting explicit confirmation.
    pub bypass_confirmation: bool,
    /// Whether the confirmation menu currently points to the affirmative choice.
    pub bypass_selected_yes: bool,
    /// Number of queued user prompts waiting behind the active turn.
    pub queued: usize,
    /// Submitted prompts awaiting application to the model conversation.
    pub queued_prompts: Vec<String>,
    /// Live and recently completed delegated tasks.
    pub subagents: Vec<crate::agent::agency::SubagentRun>,
    /// Tool invocations started in the current conversation.
    pub tools_used: usize,
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
            {
                let value = self
                    .context_percent
                    .filter(|n| n.is_finite())
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0);
                let filled = (value / 10.0).floor() as usize;
                if super::widgets::Glyphs::detect().ascii {
                    format!("{}{}", "#".repeat(filled), "-".repeat(10 - filled))
                } else {
                    format!("{}{}", "▮".repeat(filled), "▯".repeat(10 - filled))
                }
            },
            self.branch,
            if self.dirty { "*" } else { "" }
        );
        let parts = [
            format!(
                "✿ {}{}",
                self.model,
                self.effort
                    .as_ref()
                    .map_or_else(String::new, |value| format!(" {value}"))
            ),
            format!(
                "{}↑ {}{}↓",
                compact(self.input),
                if self.output_estimated { "~" } else { "" },
                compact(self.output)
            ),
            format!("кэш {}", compact(self.cache_read)),
            format!("{rate} ток/с"),
            if self.background_jobs == 0 {
                cost
            } else {
                format!("{} в фоне", self.background_jobs)
            },
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
