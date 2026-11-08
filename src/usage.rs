//! Usage ledger, model pricing table, `/usage` & `/cost` reports, budget guards, and rate-limit headers.

use crate::theme::{foreground, ColorMode, Theme};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

/// Prices in USD per 1,000,000 tokens for a specific model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ModelPricing {
    /// Input cost per 1M tokens.
    pub input_per_mtok: f64,
    /// Output cost per 1M tokens.
    pub output_per_mtok: f64,
    /// Cache read cost per 1M tokens.
    pub cache_read_per_mtok: f64,
    /// Cache write cost per 1M tokens.
    pub cache_write_per_mtok: f64,
}

impl ModelPricing {
    /// Compute USD cost for token counts.
    pub fn cost_usd(&self, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
        (input as f64 * self.input_per_mtok
            + output as f64 * self.output_per_mtok
            + cache_read as f64 * self.cache_read_per_mtok
            + cache_write as f64 * self.cache_write_per_mtok)
            / 1_000_000.0
    }
}

/// Look up explicitly configured model pricing.
/// Unverified built-in rates are deliberately omitted: relay and platform pricing can differ.
pub fn lookup_pricing(
    model: &str,
    overrides: &BTreeMap<String, ModelPricing>,
) -> Option<ModelPricing> {
    overrides.get(model).copied()
}

/// One append-only JSONL usage ledger record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageEntry {
    /// Event timestamp in Unix milliseconds.
    pub timestamp_ms: u64,
    /// Session identifier.
    pub session_id: String,
    /// Canonical project path or slug.
    pub project: String,
    /// Provider name.
    pub provider: String,
    /// Model identifier.
    pub model: String,
    /// Agent identifier (`main` or subagent name).
    pub agent: String,
    /// Uncached input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Cache read tokens.
    pub cache_read_tokens: u64,
    /// Cache write tokens.
    pub cache_write_tokens: u64,
    /// Reasoning tokens when reported.
    pub reasoning_tokens: u64,
    /// Whether token counts were estimated heuristically.
    pub estimated: bool,
    /// Exact USD cost if model pricing is known; `None` for unknown models.
    pub cost_usd: Option<f64>,
    /// Request latency in milliseconds.
    pub latency_ms: u64,
    /// Output generation throughput in tokens/sec.
    pub tokens_per_sec: f64,
}

/// Persist validated requests from one completed turn without inventing prices.
pub fn record_turn(
    home: &Path,
    session_id: &str,
    project: &Path,
    provider: &str,
    model: &str,
    requests: &[(crate::provider::canonical::Usage, u64)],
) -> io::Result<()> {
    let ledger = UsageLedger::open(home)?;
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    for (usage, latency_ms) in requests {
        ledger.record(&UsageEntry {
            timestamp_ms,
            session_id: session_id.into(),
            project: project.display().to_string(),
            provider: provider.into(),
            model: model.into(),
            agent: "main".into(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            estimated: usage.estimated,
            cost_usd: None,
            latency_ms: *latency_ms,
            tokens_per_sec: usage.output_tokens as f64 / (*latency_ms as f64 / 1000.0).max(0.001),
        })?;
    }
    Ok(())
}

/// Append-only usage ledger stored at `<home>/.gitcode/usage.jsonl`.
#[derive(Debug, Clone)]
pub struct UsageLedger {
    path: PathBuf,
}

impl UsageLedger {
    /// Open or initialize the private JSONL usage ledger under an explicit home directory.
    pub fn open(home: &Path) -> io::Result<Self> {
        let dir = home.join(".gitcode");
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        let path = dir.join("usage.jsonl");
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "usage ledger must not be a symlink",
                ));
            }
        }
        Ok(Self { path })
    }

    /// Append one usage record with `0600` file permissions.
    pub fn record(&self, entry: &UsageEntry) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        let line = serde_json::to_string(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        file.write_all(format!("{line}\n").as_bytes())?;
        file.sync_all()
    }

    /// Read all committed usage entries, optionally filtering by `since_ms`.
    pub fn entries(&self, since_ms: Option<u64>) -> io::Result<Vec<UsageEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: UsageEntry = serde_json::from_str(&line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if since_ms.is_none_or(|min| entry.timestamp_ms >= min) {
                out.push(entry);
            }
        }
        Ok(out)
    }
}

/// Aggregated statistics for a slice of usage records.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageSummary {
    /// Total requests.
    pub requests: u64,
    /// Total input tokens.
    pub input_tokens: u64,
    /// Total output tokens.
    pub output_tokens: u64,
    /// Total cache read tokens.
    pub cache_read_tokens: u64,
    /// Total cache write tokens.
    pub cache_write_tokens: u64,
    /// Total reasoning tokens.
    pub reasoning_tokens: u64,
    /// Known USD spend across priced requests.
    pub known_cost_usd: f64,
    /// Number of requests with unknown model pricing.
    pub unpriced_requests: u64,
    /// Mean output tokens per second.
    pub avg_tokens_per_sec: f64,
    /// Cache hit rate in `[0.0, 1.0]`.
    pub cache_hit_rate: f64,
}

impl UsageSummary {
    /// Summarize a slice of usage entries.
    pub fn from_entries(entries: &[UsageEntry]) -> Self {
        let mut s = Self::default();
        let mut tps_sum = 0.0;
        for e in entries {
            s.requests += 1;
            s.input_tokens += e.input_tokens;
            s.output_tokens += e.output_tokens;
            s.cache_read_tokens += e.cache_read_tokens;
            s.cache_write_tokens += e.cache_write_tokens;
            s.reasoning_tokens += e.reasoning_tokens;
            tps_sum += e.tokens_per_sec;
            if let Some(c) = e.cost_usd {
                s.known_cost_usd += c;
            } else {
                s.unpriced_requests += 1;
            }
        }
        if s.requests > 0 {
            s.avg_tokens_per_sec = tps_sum / s.requests as f64;
        }
        let denom = s.input_tokens + s.cache_read_tokens;
        if denom > 0 {
            s.cache_hit_rate = s.cache_read_tokens as f64 / denom as f64;
        }
        s
    }
}

/// Render `/cost` compact one-line summary.
pub fn render_cost_short(summary: &UsageSummary) -> String {
    if summary.requests == 0 || (summary.known_cost_usd == 0.0 && summary.unpriced_requests > 0) {
        format!(
            "Tokens: {} in / {} out (cache read {}) · Cost: tokens-only (unpriced model)",
            summary.input_tokens, summary.output_tokens, summary.cache_read_tokens
        )
    } else {
        format!(
            "Session: ${:.4} USD · {} reqs · {} in / {} out · cache hit {:.1}% · {:.1} tok/s",
            summary.known_cost_usd,
            summary.requests,
            summary.input_tokens,
            summary.output_tokens,
            summary.cache_hit_rate * 100.0,
            summary.avg_tokens_per_sec
        )
    }
}

/// Render `/usage` panel with session/window breakdowns and a flat-color daily bar chart.
pub fn render_usage_panel(
    entries: &[UsageEntry],
    session_id: &str,
    now_ms: u64,
    mode: ColorMode,
) -> Vec<String> {
    let day_ms = 86_400_000u64;
    let session_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.session_id == session_id)
        .cloned()
        .collect();
    let today_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.timestamp_ms >= now_ms.saturating_sub(day_ms))
        .cloned()
        .collect();
    let week_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.timestamp_ms >= now_ms.saturating_sub(7 * day_ms))
        .cloned()
        .collect();
    let month_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.timestamp_ms >= now_ms.saturating_sub(30 * day_ms))
        .cloned()
        .collect();

    let session = UsageSummary::from_entries(&session_entries);
    let today = UsageSummary::from_entries(&today_entries);
    let week = UsageSummary::from_entries(&week_entries);
    let month = UsageSummary::from_entries(&month_entries);
    let all = UsageSummary::from_entries(entries);

    let mut lines = vec![
        format!("Current session: {}", render_cost_short(&session)),
        format!("Today:           {}", render_cost_short(&today)),
        format!("Last 7 days:     {}", render_cost_short(&week)),
        format!("Last 30 days:    {}", render_cost_short(&month)),
        format!("All-time:        {}", render_cost_short(&all)),
    ];

    let mut by_day: BTreeMap<u64, u64> = BTreeMap::new();
    for e in entries {
        let day = e.timestamp_ms / day_ms;
        *by_day.entry(day).or_insert(0) += e.input_tokens + e.output_tokens;
    }
    let max_tokens = by_day.values().copied().max().unwrap_or(1).max(1);
    let theme = Theme::default();
    let prefix = if mode == ColorMode::None {
        String::new()
    } else {
        foreground(theme.rose, mode)
    };
    let reset = if mode == ColorMode::None {
        ""
    } else {
        "\x1b[0m"
    };
    for (day, total) in by_day.into_iter().rev().take(7).rev() {
        let width = ((total * 16) / max_tokens).max(1) as usize;
        let bar = "█".repeat(width);
        lines.push(format!("Day {day}: {prefix}{bar}{reset} {total} tok"));
    }
    lines
}

/// Budget guard outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetStatus {
    /// Within configured limits.
    Ok,
    /// Soft warning threshold crossed.
    SoftWarning(String),
    /// Hard turn or USD budget exceeded.
    Exceeded(String),
}

/// Session budget guard for `--max-budget-usd` and `--max-turns`.
#[derive(Debug, Clone)]
pub struct BudgetGuard {
    /// Optional hard USD spend limit.
    pub max_budget_usd: Option<f64>,
    /// Hard turn count limit.
    pub max_turns: u32,
    /// Fraction of budget at which a soft warning is emitted (e.g. `0.8`).
    pub soft_warn_fraction: f64,
}

impl BudgetGuard {
    /// Evaluate current session turns and USD spend against configured limits.
    pub fn check(&self, turns: u32, spent_usd: f64) -> BudgetStatus {
        if turns >= self.max_turns {
            return BudgetStatus::Exceeded(format!(
                "reached maximum turn limit ({})",
                self.max_turns
            ));
        }
        if let Some(limit) = self.max_budget_usd {
            if spent_usd >= limit {
                return BudgetStatus::Exceeded(format!(
                    "exceeded USD budget (${spent_usd:.4} / ${limit:.4})"
                ));
            }
            if spent_usd >= limit * self.soft_warn_fraction {
                return BudgetStatus::SoftWarning(format!(
                    "approaching USD budget (${spent_usd:.4} / ${limit:.4})"
                ));
            }
        }
        BudgetStatus::Ok
    }
}

/// Parsed provider rate-limit headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitInfo {
    /// Remaining requests in current window.
    pub remaining_requests: Option<u64>,
    /// Remaining tokens in current window.
    pub remaining_tokens: Option<u64>,
    /// Reset timestamp or duration text.
    pub reset: Option<String>,
}

impl RateLimitInfo {
    /// Extract rate-limit metadata from case-normalized HTTP response headers.
    pub fn from_headers(headers: &BTreeMap<String, String>) -> Self {
        let get = |keys: &[&str]| {
            keys.iter().find_map(|k| {
                headers
                    .iter()
                    .find(|(hk, _)| hk.eq_ignore_ascii_case(k))
                    .map(|(_, v)| v.as_str())
            })
        };
        Self {
            remaining_requests: get(&[
                "anthropic-ratelimit-requests-remaining",
                "x-ratelimit-remaining-requests",
            ])
            .and_then(|v| v.parse().ok()),
            remaining_tokens: get(&[
                "anthropic-ratelimit-tokens-remaining",
                "x-ratelimit-remaining-tokens",
            ])
            .and_then(|v| v.parse().ok()),
            reset: get(&[
                "anthropic-ratelimit-tokens-reset",
                "x-ratelimit-reset-tokens",
            ])
            .map(str::to_owned),
        }
    }
}
