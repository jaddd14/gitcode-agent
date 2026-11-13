//! Executable M18 regressions through the actual painter and editor.
#![allow(clippy::unwrap_used)]
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use gitcode::{
    theme::ColorMode,
    tui::{
        commands,
        input::{Action, Editor},
        render::{Frame, Renderer},
        statusbar::Status,
        widgets,
    },
};
use unicode_width::UnicodeWidthStr;

fn key(editor: &mut Editor, code: KeyCode) -> Action {
    editor.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[test]
fn slash_popup_is_immediate_navigable_and_keeps_text_on_escape() {
    let mut editor = Editor::default();
    let start = std::time::Instant::now();
    key(&mut editor, KeyCode::Char('/'));
    let frame = Frame::input(&editor, &Status::default(), 99, 36);
    assert!(start.elapsed() < std::time::Duration::from_millis(50));
    assert!(frame.lines.iter().any(|line| line.contains("Команды")));
    assert_eq!(
        commands::candidates("/").len(),
        gitcode::extensibility::BUILTIN_COMMANDS.len()
    );
    key(&mut editor, KeyCode::Down);
    assert_eq!(editor.completion_selected(), 1);
    key(&mut editor, KeyCode::Tab);
    assert!(editor.text().starts_with('/'));
    assert!(editor.text().ends_with(' '));
    key(&mut editor, KeyCode::Esc);
    assert!(!editor.completion_open());
    assert!(!editor.text().is_empty());
    editor.set_text("/usage");
    assert_eq!(
        key(&mut editor, KeyCode::Enter),
        Action::Submit("/usage".into())
    );
}

#[test]
fn actual_painter_preserves_closed_frames_and_panel_colors() {
    for width in [50, 80, 100, 120, 200] {
        let mut editor = Editor::default();
        editor.set_text("/");
        let frame = Frame::input(&editor, &Status::default(), width - 1, 36);
        assert!(frame.lines.iter().all(|line| line.width() < width));
        let mut renderer = Renderer::new(Vec::new(), width, 36, ColorMode::TrueColor, true);
        renderer.paint(&frame).unwrap();
        let output = String::from_utf8(renderer.into_inner()).unwrap();
        let visible = gitcode::tui::plain(&output);
        assert!(visible.lines().any(|line| line.ends_with('╮')));
        assert!(visible.lines().any(|line| line.ends_with('╯')));
        assert!(
            !output.contains("\x1b[48;"),
            "panels must inherit the terminal background"
        );
        assert!(output.contains("Команды"));
    }
}

#[test]
fn multiline_input_can_reach_twelve_rows_without_losing_status() {
    let mut editor = Editor::default();
    editor.set_text(
        &(0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let frame = Frame::input(&editor, &Status::default(), 79, 36);
    assert_eq!(frame.lines.len(), 16);
    assert!(frame.lines.iter().any(|line| line.contains("line 19")));
    assert!(frame.cursor.is_some_and(|(row, _)| row == 12));
    for height in [1, 4, 12, 24] {
        assert!(
            Frame::input(&editor, &Status::default(), 39, height)
                .lines
                .len()
                <= height
        );
    }
}

#[test]
fn command_cards_and_widgets_remain_cell_bounded() {
    for width in [50, 80, 100, 120, 200] {
        let rows = vec![vec!["модель 界".into(), "12345".into()]];
        let table = widgets::Table {
            headers: &["Имя", "Токены"],
            rows: &rows,
            numeric: &[1],
        }
        .render(width - 2);
        for command in gitcode::extensibility::BUILTIN_COMMANDS {
            let frame = widgets::card(command.name, &table, "esc", width);
            assert!(frame.iter().all(|line| line.width() <= width));
        }
        assert!(
            widgets::sparkline(&[0, 1, 8, 4], width, widgets::Glyphs { ascii: true }).is_ascii()
        );
        let vchart = widgets::vertical_bar_chart(
            &[("пн", 10.0), ("вт", 25.0), ("ср", 5.0)],
            width,
            6,
            widgets::Glyphs { ascii: false },
        );
        assert!(vchart.len() > 6);
        assert!(vchart.join("\n").contains("пн: 10"));
        assert!(vchart.join("\n").contains("вт: 25"));
        assert!(vchart.join("\n").contains("ср: 5"));
        assert!(vchart.iter().all(|line| line.width() <= width));
        let empty = gitcode::tui::usage_panel::render(&[], "session", 0, 0, false, width);
        assert!(empty.iter().any(|line| line.contains("пока нет запросов")));
        assert!(empty.iter().all(|line| line.width() <= width));
    }
}

#[tokio::test]
async fn every_builtin_command_renders_valid_card_at_widths_60_80_120() {
    let client = gitcode::provider::transport::Client::new(
        "http://127.0.0.1:9",
        None,
        false,
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let engine = gitcode::perm::Engine::new(&root, gitcode::perm::Mode::Default).unwrap();
    let mut agent = gitcode::agent::Agent {
        client,
        tools: gitcode::tools::Tools::new(engine),
        model: "claude-sonnet-4-20250514".into(),
        system: "system prompt".into(),
        max_turns: 8,
        max_tokens: 4096,
    };
    let mut messages = Vec::new();
    let mut status = Status {
        model: "claude-sonnet-4-20250514".into(),
        permission: "DEFAULT".into(),
        branch: "main".into(),
        ..Status::default()
    };

    let mut all_names: Vec<&str> = gitcode::extensibility::BUILTIN_COMMANDS
        .iter()
        .map(|c| c.name)
        .collect();
    all_names.push("new");

    for width in [60usize, 80, 120] {
        for name in &all_names {
            let prompt = format!("/{name}");
            let card =
                commands::execute_command(&prompt, &mut agent, &mut messages, &mut status, width)
                    .await
                    .unwrap()
                    .unwrap();
            assert!(
                card.len() >= 3,
                "command /{name} at width {width} must produce a framed card"
            );
            assert!(
                card.iter().all(|line| line.width() <= width),
                "command /{name} overflowed width {width}"
            );
            let joined = card.join("\n");
            assert!(
                !joined.contains("пока не подключено"),
                "command /{name} must not fall through to an unconnected stub"
            );
        }
    }
}

#[test]
fn busy_activity_and_retry_notice_render_above_prompt_box() {
    let editor = Editor::default();
    let status = Status {
        model: "test-model".into(),
        permission: "DEFAULT".into(),
        branch: "main".into(),
        busy: true,
        elapsed_secs: 9,
        output: 1400,
        turn_output: 1400,
        queued: 2,
        retry_notice: Some("Лимит запросов. Повтор через 4с (попытка 2 из 5)".into()),
        ..Status::default()
    };
    let frame = Frame::input(&editor, &status, 80, 24);
    assert!(frame.lines[0].starts_with('▲'));
    assert!(frame.lines[1].is_empty());
    assert!(frame.lines[2].contains("Думаю… (9с · ↓ 1.4k токенов)"));
    assert!(!frame.lines.iter().any(|l| l.contains("В очереди")));
    assert!(frame.lines.iter().all(|l| l.width() <= 80));
}

#[test]
fn small_terminal_popup_and_busy_input_fit_viewport() {
    let mut editor = Editor::default();
    editor.set_text("/");
    let frame = Frame::input(&editor, &Status::default(), 39, 12);
    assert!(frame.lines.len() < 12);
    assert!(frame.lines.iter().any(|line| line.contains("Команды")));
    editor.set_text(&"line\n".repeat(20));
    let status = Status {
        busy: true,
        retry_notice: Some("Повтор через 1с".into()),
        ..Status::default()
    };
    let frame = Frame::input(&editor, &status, 39, 12);
    assert!(frame.lines.len() <= 12);
}

#[test]
fn intro_animation_frames_are_frozen_by_golden_snapshot() {
    let mut out = String::new();
    for ms in [0u64, 200, 450, 700] {
        out.push_str(&format!("=== {ms}ms ===\n"));
        for line in gitcode::tui::intro::frame(
            std::time::Duration::from_millis(ms),
            80,
            24,
            ColorMode::None,
        ) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    let path = format!(
        "{}/testdata/ui/intro-frames.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var_os("GITCODE_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &out).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), out);
}
