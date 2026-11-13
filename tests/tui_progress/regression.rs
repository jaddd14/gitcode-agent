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

#[test]
fn streaming_estimates_are_replaced_by_provider_counts_without_affecting_accounting() {
    let base = Status {
        output: 100,
        ..Status::default()
    };
    let mut status = base.clone();
    let mut live = LiveUsage::default();
    live.add_text("Привет мир!");
    live.apply(&mut status, &base, Duration::from_secs(2));
    assert_eq!(status.turn_output, 3);
    assert_eq!(status.output, 103);
    assert!(status.output_estimated);
    assert_eq!(live.total().output_tokens, 0);
    live.snapshot(Usage {
        output_tokens: 8,
        ..Usage::default()
    });
    live.apply(&mut status, &base, Duration::from_secs(2));
    assert_eq!((status.turn_output, status.output), (8, 108));
    assert!(!status.output_estimated);
    live.finish_request();
    live.add_text("abcd");
    live.apply(&mut status, &base, Duration::from_secs(3));
    assert_eq!((status.turn_output, status.output), (9, 109));
    assert!(status.output_estimated);
}

#[test]
fn activity_indicator_animates_and_uses_requested_colors() {
    use crate::{
        theme::{self, ColorMode, Theme},
        tui::{input::Editor, render::Frame, widgets},
    };
    let mut status = Status {
        busy: true,
        elapsed_secs: 562,
        turn_output: 42,
        ..Status::default()
    };
    let first = Frame::input(&Editor::default(), &status, 80, 24)
        .lines
        .into_iter()
        .find(|line| line.contains("Думаю"))
        .unwrap();
    status.elapsed_millis = 150;
    let second = Frame::input(&Editor::default(), &status, 80, 24)
        .lines
        .into_iter()
        .find(|line| line.contains("Думаю"))
        .unwrap();
    assert!(first.starts_with(" ✻ ") || first.starts_with(" * "));
    assert_eq!(first.chars().nth(1), second.chars().nth(1));
    assert!(first.contains("(9м 22с ·"));
    let theme = Theme::default();
    let styled = widgets::style_live_row(&first, &theme, ColorMode::TrueColor, 0);
    let animated = widgets::style_live_row(&second, &theme, ColorMode::TrueColor, 150);
    assert_ne!(styled, animated);
    assert_eq!(widgets::unstyled(&styled), widgets::unstyled(&animated));
    assert!(styled.starts_with(&format!(
        " {}",
        theme::foreground(theme.warn, ColorMode::TrueColor)
    )));
    assert!(styled.contains(&format!(
        "{} (",
        theme::foreground(theme.placeholder, ColorMode::TrueColor)
    )));
}

#[test]
fn delegated_tasks_appear_below_status_divider_with_bounded_plain_rows() {
    use crate::{
        agent::agency::{SubagentRun, SubagentRunState},
        tui::{input::Editor, render::Frame},
    };
    let status = Status {
        subagents: vec![SubagentRun {
            id: "a1".into(),
            name: "explore".into(),
            task: "Проверить файлы\u{1b}[31m".into(),
            current_tool: Some("Grep".into()),
            tokens: 1200,
            elapsed_ms: 0,
            state: SubagentRunState::Running,
        }],
        ..Status::default()
    };
    for width in [30, 80] {
        let frame = Frame::input(&Editor::default(), &status, width, 20);
        let prompt = frame
            .lines
            .iter()
            .position(|s| s.contains("Спроси, или / для команд"))
            .unwrap();
        let agent = frame
            .lines
            .iter()
            .position(|s| s.contains("explore"))
            .unwrap();
        let divider = frame
            .lines
            .iter()
            .position(|line| {
                line == &crate::tui::widgets::divider(width, crate::tui::widgets::Glyphs::detect())
            })
            .unwrap();
        assert!(divider > prompt);
        assert_eq!(agent, divider + 1);
        if width >= 80 {
            assert!(frame.lines[agent].contains("Grep"));
        }
        assert!(!frame.lines[agent].contains('\u{1b}'));
        assert!(frame
            .lines
            .iter()
            .all(|s| unicode_width::UnicodeWidthStr::width(s.as_str()) <= width));
    }
}

#[test]
fn interruption_uses_tool_marker_and_keeps_a_following_gap() {
    use crate::theme::{ColorMode, Theme};
    let rows = super::interruption_rows(&Theme::default(), ColorMode::None);
    assert!(rows[0].starts_with("● ") || rows[0].starts_with("* "));
    assert!(rows[0].contains("Прервано."));
    assert_eq!(rows[1], "");
}

#[test]
fn successful_turn_clears_agent_rows_only_when_generation_finishes() {
    use crate::{
        agent::agency::{SubagentRun, SubagentRunState},
        tui::{input::Editor, render::Frame},
    };
    let mut status = Status {
        busy: true,
        subagents: vec![SubagentRun {
            id: "a1".into(),
            name: "explore".into(),
            task: "проверка".into(),
            current_tool: None,
            tokens: 8,
            elapsed_ms: 0,
            state: SubagentRunState::Completed,
        }],
        ..Status::default()
    };
    let visible = Frame::input(&Editor::default(), &status, 80, 24);
    assert!(visible.lines.iter().any(|line| line.contains("explore")));
    let mut failed = status.clone();
    super::finish_turn_status(&mut failed, false);
    assert!(Frame::input(&Editor::default(), &failed, 80, 24)
        .lines
        .iter()
        .any(|line| line.contains("explore")));
    super::finish_turn_status(&mut status, true);
    assert!(!status.busy);
    assert!(status.subagents.is_empty());
    assert!(!Frame::input(&Editor::default(), &status, 80, 24)
        .lines
        .iter()
        .any(|line| line.contains("explore")));
}

#[test]
fn running_subagent_fills_smoothly_but_finished_mark_stays_fixed() {
    use crate::{
        agent::agency::{SubagentRun, SubagentRunState},
        tui::{input::Editor, render::Frame},
    };
    let run = SubagentRun {
        id: "id".into(),
        name: "explore".into(),
        task: "task".into(),
        current_tool: None,
        tokens: 0,
        elapsed_ms: 0,
        state: SubagentRunState::Running,
    };
    let mut status = Status {
        busy: true,
        subagents: vec![run],
        ..Status::default()
    };
    let first = Frame::input(&Editor::default(), &status, 80, 20)
        .lines
        .into_iter()
        .find(|row| row.contains("explore"))
        .unwrap();
    status.elapsed_millis = 500;
    let second = Frame::input(&Editor::default(), &status, 80, 20)
        .lines
        .into_iter()
        .find(|row| row.contains("explore"))
        .unwrap();
    assert_ne!(first, second);
    status.subagents[0].state = SubagentRunState::Completed;
    let done = Frame::input(&Editor::default(), &status, 80, 20)
        .lines
        .into_iter()
        .find(|row| row.contains("explore"))
        .unwrap();
    status.elapsed_millis = 1000;
    let done_later = Frame::input(&Editor::default(), &status, 80, 20)
        .lines
        .into_iter()
        .find(|row| row.contains("explore"))
        .unwrap();
    assert_eq!(done, done_later);
}
