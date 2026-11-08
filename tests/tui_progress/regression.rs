//! Regression checks exercise the same private clock and accounting as the UI.

use super::{frame_clock, LiveUsage};
use crate::{provider::canonical::Usage, tui::statusbar::Status};
use std::time::Duration;

#[tokio::test]
async fn continuous_events_do_not_restart_frame_deadlines() {
    let mut frames = frame_clock();
    let mut events = tokio::time::interval(Duration::from_millis(1));
    events.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let end = tokio::time::sleep(Duration::from_millis(250));
    tokio::pin!(end);
    let mut paints = 0;
    let mut arrivals = 0;
    loop {
        tokio::select! {
            _ = &mut end => break,
            _ = events.tick() => arrivals += 1,
            _ = frames.tick() => paints += 1,
        }
    }
    assert!(arrivals >= 20, "insufficient event traffic: {arrivals}");
    assert!(
        paints >= 5,
        "redraw starved despite continuous input: {paints}"
    );
    assert_eq!(frames.period(), Duration::from_nanos(1_000_000_000 / 60));
    assert_eq!(
        frames.missed_tick_behavior(),
        tokio::time::MissedTickBehavior::Skip
    );
}

#[test]
fn request_snapshots_replace_but_completed_requests_accumulate() {
    let base = Status {
        input: 100,
        output: 200,
        cache_read: 300,
        cache_write: 400,
        ..Status::default()
    };
    let mut status = base.clone();
    let mut live = LiveUsage::default();
    live.snapshot(Usage {
        input_tokens: 10,
        output_tokens: 2,
        ..Usage::default()
    });
    live.snapshot(Usage {
        input_tokens: 10,
        output_tokens: 8,
        cache_read_tokens: 3,
        cache_write_tokens: 4,
        reasoning_tokens: 1,
        estimated: true,
    });
    live.finish_request();
    // Even a duplicate boundary without a new snapshot must not double count.
    live.finish_request();
    live.snapshot(Usage {
        input_tokens: 20,
        output_tokens: 1,
        cache_read_tokens: 5,
        cache_write_tokens: 6,
        reasoning_tokens: 2,
        estimated: false,
    });
    live.apply(&mut status, &base, Duration::from_secs(3));
    assert_eq!(
        (
            status.input,
            status.output,
            status.cache_read,
            status.cache_write
        ),
        (130, 209, 308, 410)
    );
    assert_eq!(status.tokens_per_second, Some(3.0));
    assert_eq!(live.total().reasoning_tokens, 3);
    assert!(live.total().estimated);
    // Final authoritative snapshot, including a retried request without deltas.
    live.snapshot(Usage {
        input_tokens: 20,
        output_tokens: 7,
        cache_read_tokens: 5,
        cache_write_tokens: 6,
        reasoning_tokens: 2,
        estimated: false,
    });
    live.finish_request();
    live.apply(&mut status, &base, Duration::from_secs(5));
    assert_eq!(
        (
            status.input,
            status.output,
            status.cache_read,
            status.cache_write
        ),
        (130, 215, 308, 410)
    );
    assert_eq!(status.tokens_per_second, Some(3.0));
    // Lower counters on a new request do not erase earlier requests.
    live.snapshot(Usage {
        input_tokens: 1,
        output_tokens: 1,
        ..Usage::default()
    });
    live.apply(&mut status, &base, Duration::from_secs(8));
    assert_eq!((status.input, status.output), (131, 216));
}
