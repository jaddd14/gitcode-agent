//! Tests for Milestone M12 usage ledger, pricing, reports, budget guard, and rate limits.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::{theme, usage};

use std::collections::BTreeMap;
use theme::ColorMode;
use usage::{
    lookup_pricing, render_cost_short, render_usage_panel, BudgetGuard, BudgetStatus, ModelPricing,
    RateLimitInfo, UsageEntry, UsageLedger, UsageSummary,
};

#[test]
fn pricing_lookup_ledger_roundtrip_and_reports() {
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "custom-model".to_owned(),
        ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            cache_read_per_mtok: 0.1,
            cache_write_per_mtok: 0.5,
        },
    );
    assert!(lookup_pricing("claude-opus-5-5", &overrides).is_none());
    let configured = lookup_pricing("custom-model", &overrides).unwrap();
    assert!((configured.cost_usd(1_000_000, 1_000_000, 0, 0) - 3.0).abs() < 1e-6);
    assert!(lookup_pricing("unknown-local-llm", &overrides).is_none());
    assert!(lookup_pricing("custom-model", &overrides).is_some());

    let home = std::env::temp_dir().join(format!("gitcode-usage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let ledger = UsageLedger::open(&home).unwrap();
    let entry1 = UsageEntry {
        timestamp_ms: 1_700_000_000_000,
        session_id: "sess-1".into(),
        project: "/tmp/proj".into(),
        provider: "anthropic".into(),
        model: "claude-opus-5-5".into(),
        agent: "main".into(),
        input_tokens: 1_000,
        output_tokens: 500,
        cache_read_tokens: 1_000,
        cache_write_tokens: 0,
        reasoning_tokens: 100,
        estimated: false,
        cost_usd: Some(0.054),
        latency_ms: 500,
        tokens_per_sec: 42.0,
    };
    let entry2 = UsageEntry {
        model: "unknown-model".into(),
        cost_usd: None,
        ..entry1.clone()
    };
    ledger.record(&entry1).unwrap();
    ledger.record(&entry2).unwrap();

    let all = ledger.entries(None).unwrap();
    assert_eq!(all.len(), 2);
    let summary = UsageSummary::from_entries(&all);
    assert_eq!(summary.requests, 2);
    assert_eq!(summary.unpriced_requests, 1);
    assert!((summary.cache_hit_rate - 0.5).abs() < 1e-6);

    let short = render_cost_short(&summary);
    assert!(short.contains("$0.0540 USD"));

    let panel = render_usage_panel(&all, "sess-1", 1_700_000_001_000, ColorMode::None);
    assert!(panel.iter().any(|l| l.starts_with("Current session:")));
    assert!(panel.iter().any(|l| l.contains("█")));

    let guard = BudgetGuard {
        max_budget_usd: Some(1.0),
        max_turns: 5,
        soft_warn_fraction: 0.8,
    };
    assert_eq!(guard.check(1, 0.2), BudgetStatus::Ok);
    assert!(matches!(guard.check(2, 0.85), BudgetStatus::SoftWarning(_)));
    assert!(matches!(guard.check(5, 0.2), BudgetStatus::Exceeded(_)));
    assert!(matches!(guard.check(2, 1.05), BudgetStatus::Exceeded(_)));

    let mut headers = BTreeMap::new();
    headers.insert("x-ratelimit-remaining-requests".into(), "49".into());
    headers.insert("x-ratelimit-remaining-tokens".into(), "90000".into());
    headers.insert("x-ratelimit-reset-tokens".into(), "12s".into());
    let rl = RateLimitInfo::from_headers(&headers);
    assert_eq!(rl.remaining_requests, Some(49));
    assert_eq!(rl.remaining_tokens, Some(90_000));
    assert_eq!(rl.reset.as_deref(), Some("12s"));
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn ledger_rejects_link_substitution_and_non_private_files() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let home = std::env::temp_dir().join(format!("gitcode-usage-links-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let ledger = UsageLedger::open(&home).unwrap();
    let victim = home.join("victim");
    std::fs::write(&victim, "untouched").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
    let path = home.join(".gitcode/usage.jsonl");
    symlink(&victim, &path).unwrap();
    assert!(ledger.entries(None).is_err());
    let entry = UsageEntry {
        timestamp_ms: 1,
        session_id: "s".into(),
        project: "p".into(),
        provider: "p".into(),
        model: "m".into(),
        agent: "a".into(),
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        estimated: false,
        cost_usd: None,
        latency_ms: 1,
        tokens_per_sec: 1.0,
    };
    assert!(ledger.record(&entry).is_err());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&victim, &path).unwrap();
    assert!(ledger.record(&entry).is_err());
    assert!(ledger.entries(None).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir_all(home.join(".gitcode")).unwrap();
    let outside = home.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, home.join(".gitcode")).unwrap();
    assert!(UsageLedger::open(&home).is_err());
    let _ = std::fs::remove_dir_all(&home);
}
