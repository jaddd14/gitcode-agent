//! Cell-level regressions for the single-hue terminal chrome across all UI screens.
#![allow(clippy::unwrap_used)]
use gitcode::{
    agent::Agent,
    extensibility::BUILTIN_COMMANDS,
    perm::{Engine, Mode},
    provider::transport::Client,
    theme::{ColorMode, Rgb, Theme},
    tools::Tools,
    tui::{
        self, commands,
        header::{Header, HEADER_GAP},
        input::Editor,
        render::{Frame, Renderer},
        statusbar::Status,
    },
};
use std::time::Duration;

const OLD_GRAY: Rgb = Rgb(150, 132, 173);
const OLD_PURPLES: [Rgb; 5] = [
    Rgb(52, 24, 86),
    Rgb(59, 35, 86),
    Rgb(74, 29, 122),
    Rgb(122, 63, 181),
    Rgb(165, 123, 214),
];

fn cells(row: &str) -> Vec<(char, Option<Rgb>)> {
    let mut chars = row.chars().peekable();
    let mut color = None;
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.next_if_eq(&'[').is_some() {
            let mut code = String::new();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    if next == 'm' {
                        let values: Vec<u8> =
                            code.split(';').filter_map(|v| v.parse().ok()).collect();
                        if values.starts_with(&[38, 2]) && values.len() == 5 {
                            color = Some(Rgb(values[2], values[3], values[4]));
                        } else if values == [0] || values == [39] {
                            color = None;
                        }
                    }
                    break;
                }
                code.push(next);
            }
        } else {
            out.push((c, color));
        }
    }
    out
}

fn assert_frame_rows(rows: &[String], theme: &Theme) {
    let mut in_frame = false;
    let mut right_col: Option<usize> = None;
    for row in rows {
        let styled = if row.contains('\x1b') {
            row.clone()
        } else {
            tui::widgets::style_frame_row(row, theme, ColorMode::TrueColor)
        };
        let buffer = cells(&styled);
        for (_, color) in &buffer {
            assert_ne!(*color, Some(OLD_GRAY), "old gray found in row: {row}");
            for purple in OLD_PURPLES {
                assert_ne!(*color, Some(purple), "old purple found in row: {row}");
            }
        }
        let plain = tui::plain(&styled);
        if plain.starts_with('╭') && plain.ends_with('╮') {
            in_frame = true;
            right_col = Some(buffer.len() - 1);
            assert_eq!(
                buffer.last().copied(),
                Some(('╮', Some(theme.border))),
                "{plain}"
            );
            let accent_top = buffer
                .iter()
                .skip(1)
                .take(buffer.len().saturating_sub(2))
                .all(|(ch, _)| matches!(ch, '─' | '-'));
            for (position, (c, color)) in buffer.iter().enumerate() {
                if matches!(c, '╭' | '╮' | '─') {
                    let expected = if accent_top && position < 4 {
                        theme.rose
                    } else {
                        theme.border
                    };
                    assert_eq!(*color, Some(expected), "top border glyph in {plain}");
                }
            }
        } else if plain.starts_with('╰') && plain.ends_with('╯') {
            assert!(in_frame, "bottom border without top border: {plain}");
            assert_eq!(
                Some(buffer.len() - 1),
                right_col,
                "misaligned bottom right in {plain}"
            );
            assert_eq!(
                buffer.last().copied(),
                Some(('╯', Some(theme.border))),
                "{plain}"
            );
            let left_corner = if plain.contains("shift+tab") {
                theme.rose
            } else {
                theme.border
            };
            assert_eq!(buffer[0], ('╰', Some(left_corner)), "{plain}");
            in_frame = false;
            right_col = None;
        } else if in_frame {
            assert_eq!(
                Some(buffer.len() - 1),
                right_col,
                "misaligned right border on interior row: {plain}"
            );
            assert_eq!(
                buffer.last().copied(),
                Some(('│', Some(theme.border))),
                "right border must be neutral on interior row: {plain}"
            );
            if plain.starts_with(" › ") {
                assert_eq!(buffer[0].0, ' ', "column 0 gap on chevron row: {plain}");
                assert_eq!(
                    buffer[1],
                    ('›', Some(theme.rose)),
                    "column 1 rose chevron: {plain}"
                );
                assert_eq!(buffer[2].0, ' ', "column 2 space after chevron: {plain}");
            } else {
                let info_row = [
                    "Model    :",
                    "Session  :",
                    "Cache    :",
                    "Speed    :",
                    "Output   :",
                    "Context  :",
                    "Working  :",
                    "Tools    :",
                ]
                .iter()
                .any(|label| plain.contains(label));
                assert_eq!(
                    buffer[0],
                    ('│', Some(if info_row { theme.rose } else { theme.border })),
                    "left border color at column 0: {plain}"
                );
            }
        }
    }
    assert!(!in_frame, "unclosed frame detected");
}

#[test]
fn prompt_chrome_and_text_are_colored_independently() {
    let theme = Theme::default();
    for width in [60, 80, 100, 140] {
        for input in ["", "typed input\nsecond line", "/"] {
            let mut editor = Editor::default();
            editor.set_text(input);
            let frame = Frame::input(&editor, &Status::default(), width, 36);
            assert_frame_rows(&frame.lines, &theme);
            for row in &frame.lines {
                let styled = tui::widgets::style_frame_row(row, &theme, ColorMode::TrueColor);
                let buffer = cells(&styled);
                if row.starts_with(" › ") {
                    let expected = if input.is_empty() {
                        Some(theme.placeholder)
                    } else {
                        None
                    };
                    assert_eq!(buffer[3].1, expected);
                }
            }
            let mut renderer = Renderer::new(Vec::new(), width + 1, 36, ColorMode::TrueColor, true);
            renderer.paint(&frame).unwrap();
            let output = String::from_utf8(renderer.into_inner()).unwrap();
            assert!(!output.contains("38;2;150;132;173"));
            assert!(!output.contains("38;2;122;63;181"));
        }
    }
}

#[tokio::test]
async fn palette_audit_covers_header_cards_running_turn_retry_and_permissions() {
    let theme = Theme::default();
    let header = Header {
        version: "0.2.2",
        model: "claude-opus-5-5",
        provider: "anthropic",
        cwd: "~/workspace",
        branch: "main",
    };
    for width in [60, 80, 100, 140] {
        let rows = header.lines(width, &theme, ColorMode::TrueColor);
        assert_frame_rows(&rows[..rows.len() - HEADER_GAP], &theme);

        let busy_status = Status {
            model: "claude-opus-5-5".into(),
            permission: "DEFAULT".into(),
            branch: "main".into(),
            busy: true,
            elapsed_secs: 12,
            output: 2400,
            queued: 1,
            retry_notice: Some("Лимит запросов. Повтор через 5с (попытка 2 из 4)".into()),
            ..Status::default()
        };
        let mut editor = Editor::default();
        editor.set_text("first line\nsecond line");
        let busy_frame = Frame::input(&editor, &busy_status, width, 36);
        assert_frame_rows(&busy_frame.lines, &theme);

        let tool_rows =
            tui::tool_card::completed("Bash", "line 1\nline 2\nline 3\nline 4", false, 42, width);
        assert_frame_rows(&tool_rows, &theme);

        let perm_card = tui::widgets::card(
            "Разрешение · Bash",
            &[
                "Команда: cargo test".into(),
                "[y] да · [a] всегда · [n] нет".into(),
            ],
            "правило: workspace",
            width,
        );
        assert_frame_rows(&perm_card, &theme);
    }

    let tmp = std::env::temp_dir().join(format!("gitcode-rose-audit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let client = Client::new("http://127.0.0.1:9", None, false, Duration::from_secs(1)).unwrap();
    let engine = Engine::new(&tmp, Mode::Default).unwrap();
    let mut agent = Agent {
        client,
        tools: Tools::new(engine),
        model: "claude-opus-5-5".into(),
        system: "system".into(),
        max_turns: 4,
        max_tokens: 1024,
    };
    let mut messages = Vec::new();
    let mut status = Status {
        model: "claude-opus-5-5".into(),
        permission: "DEFAULT".into(),
        branch: "main".into(),
        ..Status::default()
    };
    let mut names: Vec<&str> = BUILTIN_COMMANDS.iter().map(|c| c.name).collect();
    names.push("new");
    for name in names {
        let card = commands::execute_command(
            &format!("/{name}"),
            &mut agent,
            &mut messages,
            &mut status,
            80,
        )
        .await
        .unwrap()
        .unwrap();
        assert_frame_rows(&card, &theme);
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn sanitization_removes_complete_csi_and_osc_sequences() {
    assert_eq!(tui::plain("\x1b[38;2;1;2;3mtext\x1b[0m"), "text");
    assert_eq!(tui::plain("a\x1b]0;hidden title\x07b"), "ab");
    assert_eq!(tui::plain("a\x1b]8;;https://example.test\x1b\\b"), "ab");
}
