//! Status projection; live counters are owned and updated by orchestration.

use super::clip;

/// A shared estimate of the context sent to the model, excluding reserved output.
pub(crate) struct ContextEstimate {
    pub system: u64,
    pub tools: u64,
    pub messages: u64,
    pub window: u64,
}

impl ContextEstimate {
    pub fn calculate(
        compaction: &crate::agent::context::Compaction,
        system: &str,
        messages: &[crate::provider::canonical::Message],
    ) -> Result<Self, String> {
        let definitions = crate::tools::Tools::definitions();
        Ok(Self {
            system: (system.len() as u64).div_ceil(3),
            tools: (serde_json::to_vec(&definitions)
                .map_err(|_| "cannot estimate tools")?
                .len() as u64)
                .div_ceil(3),
            messages: compaction.estimate_projection(messages)?,
            window: compaction.window.max(1),
        })
    }

    pub fn used(&self) -> u64 {
        self.system
            .saturating_add(self.tools)
            .saturating_add(self.messages)
    }

    pub fn ratio(&self) -> f64 {
        (self.used() as f64 / self.window as f64).clamp(0.0, 1.0)
    }
}

pub(crate) fn update_context(
    status: &mut Status,
    agent: &crate::agent::Agent,
    messages: &[crate::provider::canonical::Message],
) {
    status.context_percent = ContextEstimate::calculate(&agent.compaction, &agent.system, messages)
        .ok()
        .map(|estimate| estimate.ratio() * 100.0);
}

/// Provider-independent values for the always-visible live status row.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Current model display name.
    pub model: String,
    /// Active provider profile name, shown by `/provider`.
    pub provider: String,
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
        let cost = self
            .cost_usd
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map_or_else(|| "—".into(), |n| format!("${n:.4}"));
        use unicode_width::UnicodeWidthStr;
        let right = format!(
            "ctx {} │ {}{}",
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::agent::context::Compaction;
    use crate::provider::canonical::{Message, Role};
    use sha2::{Digest, Sha256};

    #[test]
    fn context_tracks_current_projection_and_falls_after_compaction_and_clear() {
        let messages = vec![
            Message::text(Role::User, "old context ".repeat(10_000)),
            Message::text(Role::Assistant, "recent answer"),
        ];
        let mut compaction = Compaction::new(200_000, 0.9);
        let empty = ContextEstimate::calculate(&compaction, "system", &[]).unwrap();
        let full = ContextEstimate::calculate(&compaction, "system", &messages).unwrap();
        assert!(full.ratio() > empty.ratio());
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&messages[..1]).unwrap()).into();
        let checkpoint = serde_json::to_vec(&serde_json::json!({
            "version": 1, "boundary": 1, "digest": digest, "text": "summary"
        }))
        .unwrap();
        assert!(compaction.restore(&checkpoint, &messages));
        let compacted = ContextEstimate::calculate(&compaction, "system", &messages).unwrap();
        assert!(compacted.ratio() < full.ratio());
        assert!(compacted.ratio() >= empty.ratio());
        compaction.clear();
        let cleared = ContextEstimate::calculate(&compaction, "system", &[]).unwrap();
        assert_eq!(cleared.used(), empty.used());
        let mut status = Status {
            context_percent: Some(full.ratio() * 100.0),
            ..Status::default()
        };
        let line = status.line(100);
        assert!(line.contains("ctx "));
        assert!(!line.contains('%'));
        status.context_percent = Some(200.0);
        let full_bar = if super::super::widgets::Glyphs::detect().ascii {
            "ctx ##########"
        } else {
            "ctx ▮▮▮▮▮▮▮▮▮▮"
        };
        assert!(status.line(100).contains(full_bar));
    }
}
