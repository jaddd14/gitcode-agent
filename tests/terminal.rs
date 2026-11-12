//! Deterministic terminal foundation acceptance tests, without touching the runner TTY.

#![allow(clippy::unwrap_used)]

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use gitcode::theme::{ColorMode, Theme};
use gitcode::tui::{
    header::{Header, HEADER_GAP, SAKURA},
    input::{Action, Editor},
    intro,
    render::{Frame, Renderer},
    statusbar::Status,
    term::{TerminalBackend, TerminalGuard, RESTORE, START},
};
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Backend {
    bytes: Mutex<Vec<u8>>,
    raw: Mutex<bool>,
}
impl TerminalBackend for Backend {
    fn raw(&self) -> io::Result<()> {
        *self.raw.lock().unwrap() = true;
        Ok(())
    }
    fn cooked(&self) -> io::Result<()> {
        *self.raw.lock().unwrap() = false;
        Ok(())
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.bytes.lock().unwrap().extend(bytes);
        Ok(())
    }
}
fn key(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) -> Action {
    editor.handle(Event::Key(KeyEvent::new(code, modifiers)))
}
#[test]
fn guard_restores_every_flag_on_drop_and_unwind() {
    let backend = Arc::new(Backend::default());
    {
        let guard = TerminalGuard::with_backend(backend.clone(), false).unwrap();
        assert!(*backend.raw.lock().unwrap());
        guard.restore();
        guard.restore();
    }
    assert!(!*backend.raw.lock().unwrap());
    assert_eq!(
        *backend.bytes.lock().unwrap(),
        format!("{START}{RESTORE}").as_bytes()
    );
    let backend = Arc::new(Backend::default());
    let copy = backend.clone();
    assert!(std::panic::catch_unwind(move || {
        let _guard = TerminalGuard::with_backend(copy, false).unwrap();
        panic!("intentional guard unwind");
    })
    .is_err());
    assert!(!*backend.raw.lock().unwrap());
    assert!(backend.bytes.lock().unwrap().ends_with(RESTORE.as_bytes()));
}
#[test]
fn editor_multiline_unicode_paste_and_ctrl_c() {
    let mut e = Editor::default();
    e.set_text("A👩‍💻е́界");
    key(&mut e, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(e.text(), "A👩‍💻е́");
    key(&mut e, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(e.text(), "A👩‍💻");
    key(&mut e, KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(e.text(), "A👩‍💻\n");
    e.set_text("first\\");
    key(&mut e, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(e.text(), "first\n");
    e.handle(Event::Paste("1\r\n2\n3\n4\n5".into()));
    assert!(e.view(80).lines.join("\n").contains("[pasted 5 lines]"));
    assert_eq!(
        key(&mut e, KeyCode::Enter, KeyModifiers::NONE),
        Action::Submit("first\n1\n2\n3\n4\n5".into())
    );
    e.set_text("draft");
    assert_eq!(
        key(&mut e, KeyCode::Char('c'), KeyModifiers::CONTROL),
        Action::Redraw
    );
    assert_eq!(e.text(), "");
    assert_eq!(
        key(&mut e, KeyCode::Char('c'), KeyModifiers::CONTROL),
        Action::Interrupt
    );
    assert_eq!(
        key(&mut e, KeyCode::Esc, KeyModifiers::NONE),
        Action::Interrupt
    );
    assert_eq!(
        key(&mut e, KeyCode::Esc, KeyModifiers::NONE),
        Action::Rewind
    );
}
#[test]
fn editor_history_navigation_and_width_snapshot() {
    let mut e = Editor::default();
    e.set_history(vec!["old".into(), "new".into()]);
    e.set_text("draft");
    key(&mut e, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(e.text(), "new");
    key(&mut e, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(e.text(), "old");
    key(&mut e, KeyCode::Down, KeyModifiers::NONE);
    key(&mut e, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(e.text(), "draft");
    e.set_text("a界b");
    assert_eq!(e.view(3).lines, ["a界", "b"]);
    assert_eq!((e.view(3).cursor_row, e.view(3).cursor_col), (1, 1));
}
#[test]
fn header_plain_snapshot_news_spacing_and_pixel_geometry() {
    assert!(SAKURA.iter().all(|row| row.len() == 16));
    let h = Header {
        version: "0.1.0",
        model: "test",
        provider: "local",
        cwd: "/work",
        branch: "main",
    };
    let lines = h.lines(140, &Theme::default(), ColorMode::None);
    assert!(lines.iter().all(|line| !line.contains("gitcode v")));
    assert!(!lines[0].contains('╭'));
    assert!(lines
        .iter()
        .all(|line| !line.contains('╰') && !line.contains('╯')));
    assert!(lines
        .iter()
        .any(|line| line.contains("/model — выбрать модель")));
    assert!(lines
        .iter()
        .any(|line| line.contains("/context — состав контекста")));
    assert!(lines
        .iter()
        .take(lines.len() - HEADER_GAP)
        .all(|s| unicode_width::UnicodeWidthStr::width(s.as_str()) == 96));
    assert!(lines[lines.len() - HEADER_GAP..]
        .iter()
        .all(String::is_empty));
    assert!(h
        .lines(12, &Theme::default(), ColorMode::None)
        .iter()
        .all(|s| unicode_width::UnicodeWidthStr::width(s.as_str()) <= 12));
}
#[test]
fn native_scrollback_sync_and_plain_output() {
    let mut e = Editor::default();
    e.set_text("hello");
    let status = Status {
        input: 12,
        output: 3,
        ..Status::default()
    };
    let frame = Frame::input(&e, &status, 80, 24);
    assert!(frame.lines[0].starts_with('╭'));
    assert!(frame.lines[1].contains("› hello"));
    let (cursor_row, cursor_col) = frame.cursor.unwrap();
    assert_eq!(cursor_row, 1);
    assert_eq!(
        unicode_width::UnicodeWidthStr::width(
            frame.lines[cursor_row].split_once("hello").unwrap().0
        ) + "hello".len(),
        cursor_col,
        "cursor must follow the visible input, including the frame border"
    );
    let mut r = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    r.paint(&frame).unwrap();
    r.finalize("answer\x1b[2J").unwrap();
    let output = String::from_utf8(r.into_inner()).unwrap();
    assert_eq!(output.matches("\x1b[?2026h").count(), 2);
    assert_eq!(output.matches("\x1b[?2026l").count(), 2);
    assert!(output.contains("\x1b8\r\x1b[5A\x1b[2K"));
    assert!(output.contains("answer\x1b[0m\r\n"));
    assert!(!output.contains("1049"));
    let mut r = Renderer::new(Vec::new(), 80, 24, ColorMode::None, false);
    r.paint(&frame).unwrap();
    r.finalize("plain").unwrap();
    assert_eq!(r.into_inner(), b"plain\n");
}

#[test]
fn editor_wraps_whole_words_and_keeps_cursor_position() {
    let mut editor = Editor::default();
    editor.set_text("привет мир");
    let view = editor.view(8);
    assert_eq!(view.lines, ["привет ", "мир"]);
    assert_eq!((view.cursor_row, view.cursor_col), (1, 3));
    editor.set_text("один длинноеслово");
    let view = editor.view(8);
    assert_eq!(view.lines, ["один ", "длинноес", "лово"]);
}
#[test]
fn editor_soft_wrap_hides_separators_without_changing_prompt() {
    let mut editor = Editor::default();
    for text in ["ровно ничего не меняется", "ровно   ничего не меняется"]
    {
        editor.set_text(text);
        let view = editor.view(5);
        assert_eq!(view.lines, ["ровно", "ничег", "о не ", "меняе", "тся"]);
        assert_eq!((view.cursor_row, view.cursor_col), (4, 3));
        assert_eq!(editor.text(), text);
        assert_eq!(
            key(&mut editor, KeyCode::Enter, KeyModifiers::NONE),
            Action::Submit(text.into())
        );
    }
    editor.set_text("12345 next");
    assert_eq!(editor.view(5).lines, ["12345", "next"]);
    key(&mut editor, KeyCode::Home, KeyModifiers::NONE);
    for _ in 0..6 {
        key(&mut editor, KeyCode::Right, KeyModifiers::NONE);
    }
    let view = editor.view(5);
    assert_eq!((view.cursor_row, view.cursor_col), (1, 0));

    editor.set_text("  abc\n  def");
    assert_eq!(editor.view(8).lines, ["  abc", "  def"]);
    editor.set_text("12345\n  def");
    assert_eq!(editor.view(5).lines, ["12345", "", "  def", ""]);
}

#[test]
fn held_deletion_repeats_accelerate_and_release_does_not_delete() {
    use crossterm::event::KeyEventKind;
    let mut editor = Editor::default();
    editor.set_text(&"я".repeat(50));
    key(&mut editor, KeyCode::Backspace, KeyModifiers::NONE);
    for _ in 0..8 {
        editor.handle(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Backspace,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )));
    }
    assert_eq!(editor.text().chars().count(), 40);
    let before = editor.text().to_owned();
    editor.handle(Event::Key(KeyEvent::new_with_kind(
        KeyCode::Backspace,
        KeyModifiers::NONE,
        KeyEventKind::Release,
    )));
    assert_eq!(editor.text(), before);
    key(&mut editor, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(editor.text().chars().count(), 39);
    key(&mut editor, KeyCode::Home, KeyModifiers::NONE);
    editor.handle(Event::Key(KeyEvent::new_with_kind(
        KeyCode::Delete,
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    )));
    assert_eq!(editor.text().chars().count(), 38);
}

#[test]
fn queue_count_is_never_rendered_under_the_prompt() {
    let status = Status {
        busy: true,
        queued: 1,
        ..Status::default()
    };
    let frame = Frame::input(&Editor::default(), &status, 80, 24);
    assert!(!frame.lines.iter().any(|row| row.contains("В очереди")));
}

#[test]
fn input_is_anchored_to_terminal_bottom_and_reflows_after_resize() {
    let editor = Editor::default();
    let status = Status::default();
    let mut renderer = Renderer::new(Vec::new(), 80, 40, ColorMode::None, true);
    renderer.finalize("startup information").unwrap();
    let frame = Frame::input(&editor, &status, 79, 40);
    renderer.paint_bottom(&frame).unwrap();
    renderer.resize(80, 24);
    let resized = Frame::input(&editor, &status, 79, 24);
    renderer.paint_bottom(&resized).unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    assert!(output.contains(&format!("\x1b[{};1H", 40 - frame.lines.len())));
    assert!(output.contains(&format!("\x1b[{};1H", 24 - resized.lines.len())));
    assert!(!output.contains("1049"));
}

#[test]
fn finalized_content_gets_room_above_the_bottom_composer() {
    let frame = Frame::input(&Editor::default(), &Status::default(), 79, 24);
    let mut renderer = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    renderer.paint_bottom(&frame).unwrap();
    renderer
        .finalize_styled(&["user message".into(), String::new()])
        .unwrap();
    renderer.paint_bottom(&frame).unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    let after_message = output
        .split_once("user message\x1b[0m\r\n\x1b[0m\r\n")
        .unwrap()
        .1;
    assert!(after_message.contains(&format!(
        "{}\x1b[{};1H",
        "\r\n".repeat(frame.lines.len()),
        24 - frame.lines.len()
    )));
}

#[test]
fn activity_has_a_blank_line_before_the_composer() {
    let status = Status {
        busy: true,
        ..Status::default()
    };
    let frame = Frame::input(&Editor::default(), &status, 79, 24);
    let activity = frame
        .lines
        .iter()
        .position(|line| line.contains("Думаю"))
        .unwrap();
    assert!(frame.lines[activity + 1].is_empty());
    assert!(frame.lines[activity + 2].starts_with('╭'));
    assert!(!frame
        .lines
        .iter()
        .any(|line| line.contains("отменить") || line.contains("[esc]")));
}

#[test]
fn flow_stream_reuses_visible_rows_without_repainting_the_whole_frame() {
    let first = Frame {
        lines: vec!["stable".into(), "tail".into(), "input".into()],
        cursor: Some((2, 0)),
    };
    let second = Frame {
        lines: vec!["tail".into(), "new".into(), "input".into()],
        cursor: Some((2, 0)),
    };
    let mut renderer = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    renderer.paint_flow_with_styled_prefix(&first, 2).unwrap();
    renderer
        .paint_stream_flow(&["stable".into()], &second, 2)
        .unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    let update = output.split("\x1b[?2026h").nth(2).unwrap();
    assert!(!update.contains("\x1b[2K"));
    assert!(!update.contains("stable"));
    assert!(update.contains("new"));
}

#[test]
fn growing_bottom_composer_scrolls_history_instead_of_overwriting_it() {
    let frame = Frame::input(&Editor::default(), &Status::default(), 79, 24);
    let busy = Frame::input(
        &Editor::default(),
        &Status {
            busy: true,
            ..Status::default()
        },
        79,
        24,
    );
    let mut renderer = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    renderer.paint_bottom(&frame).unwrap();
    renderer.paint_bottom(&busy).unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    let growth = busy.lines.len() - frame.lines.len();
    assert!(output.contains(&format!("\x1b[24;1H{}", "\r\n".repeat(growth))));
}

#[test]
fn streaming_update_repaints_changed_rows_without_erasing_the_live_frame() {
    let first = Frame {
        lines: vec!["old response".into(), "composer".into(), "status".into()],
        cursor: Some((1, 2)),
    };
    let second = Frame {
        lines: vec![
            "updated response".into(),
            "composer".into(),
            "status".into(),
        ],
        cursor: Some((1, 2)),
    };
    let mut renderer = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    renderer.paint_bottom(&first).unwrap();
    renderer.paint_bottom(&second).unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    let update = output.split("\x1b[?2026h").nth(2).unwrap();
    assert!(update.contains("updated response"));
    assert!(!update.contains("composer"));
    assert!(!update.contains("\x1b[2K"));
}

#[test]
fn streamed_scrollback_and_repaint_share_one_synchronized_write() {
    let first = Frame {
        lines: vec!["live".into(), "input".into()],
        cursor: Some((1, 0)),
    };
    let second = Frame {
        lines: vec!["next".into(), "input".into()],
        cursor: Some((1, 0)),
    };
    let mut renderer = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    renderer.paint_bottom(&first).unwrap();
    renderer
        .paint_stream(&["committed".into()], &second, 1)
        .unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    let batch = output.split("\x1b[?2026h").nth(2).unwrap();
    assert!(batch.contains("committed") && batch.contains("next"));
    assert_eq!(batch.matches("\x1b[?2026l").count(), 1);
}

#[test]
fn visible_streamed_rows_move_to_scrollback_without_clearing_the_screen() {
    let first = Frame {
        lines: vec!["stable".into(), "tail".into(), "input".into()],
        cursor: Some((2, 0)),
    };
    let second = Frame {
        lines: vec!["tail".into(), "new".into(), "input".into()],
        cursor: Some((2, 0)),
    };
    let mut renderer = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    renderer.paint_bottom_with_styled_prefix(&first, 2).unwrap();
    renderer
        .paint_stream(&["stable".into()], &second, 2)
        .unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    let update = output.split("\x1b[?2026h").nth(2).unwrap();
    assert!(!update.contains("\x1b[2K"));
    assert!(!update.contains("stable"));
    assert!(update.contains("new"));
}

#[test]
fn notification_card_has_yellow_borders_and_inner_padding() {
    let card = gitcode::tui::widgets::card_with_glyphs(
        "/model",
        &["Активная модель".into()],
        "",
        40,
        gitcode::tui::widgets::Glyphs { ascii: false },
    );
    assert!(card[1].starts_with("│ Активная модель"));
    let mut renderer = Renderer::new(Vec::new(), 80, 24, ColorMode::TrueColor, true);
    renderer.finalize_card(&card).unwrap();
    let output = String::from_utf8(renderer.into_inner()).unwrap();
    let yellow = gitcode::theme::foreground(Theme::default().warn, ColorMode::TrueColor);
    assert!(output.contains(&format!("{yellow}╭")));
    assert!(output.contains(&format!("{yellow}│\x1b[0m Активная модель")));
}

#[test]
fn animation_is_deterministic_and_has_a_bounded_deadline() {
    assert_eq!(intro::DURATION.as_millis(), 700);
    let f = intro::frame(intro::DURATION, 80, 24, ColorMode::None);
    assert_eq!(
        f,
        gitcode::tui::header::mascot(&Theme::default(), ColorMode::None)
    );
    assert_eq!(
        intro::frame(std::time::Duration::ZERO, 40, 6, ColorMode::None),
        intro::frame(std::time::Duration::ZERO, 40, 6, ColorMode::None)
    );
}

#[cfg(unix)]
#[test]
fn signal_cleanup_child() {
    let Ok(path) = std::env::var("GITCODE_TERMINAL_SIGNAL_TEST") else {
        return;
    };
    struct FileBackend(std::path::PathBuf);
    impl TerminalBackend for FileBackend {
        fn raw(&self) -> io::Result<()> {
            Ok(())
        }
        fn cooked(&self) -> io::Result<()> {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&self.0)?
                .write_all(b"COOKED")
        }
        fn write(&self, bytes: &[u8]) -> io::Result<()> {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.0)?
                .write_all(bytes)
        }
    }
    let _guard = TerminalGuard::with_backend(Arc::new(FileBackend(path.into())), true).unwrap();
    use std::io::Write;
    std::io::stdout().write_all(b"READY\n").unwrap();
    std::io::stdout().flush().unwrap();
    std::thread::park();
}
#[cfg(unix)]
#[test]
fn unix_termination_signals_restore_before_default_exit() {
    use std::io::BufRead;
    for signal in ["INT", "TERM", "HUP"] {
        let path =
            std::env::temp_dir().join(format!("gitcode-terminal-{}-{signal}", std::process::id()));
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "signal_cleanup_child", "--nocapture"])
            .env("GITCODE_TERMINAL_SIGNAL_TEST", &path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(stdout.read_line(&mut line).unwrap() > 0);
            if line.trim() == "READY" {
                break;
            }
        }
        assert!(std::process::Command::new("kill")
            .args(["-s", signal, &child.id().to_string()])
            .status()
            .unwrap()
            .success());
        assert!(!child.wait().unwrap().success());
        let output = std::fs::read_to_string(&path).unwrap();
        assert_eq!(output, format!("{START}{RESTORE}COOKED"));
        std::fs::remove_file(path).unwrap();
    }
}
