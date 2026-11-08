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
    let lines = h.lines(80, &Theme::default(), ColorMode::None);
    assert_eq!(
        lines.join("\n"),
        concat!(
            "     ▀▀▀▀▀▀        gitcode 0.1.0\n",
            " ▀▀▀▀▀▀▀▀▀▀▀▀▀▀    test · local\n",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀   /work · main\n",
            " ▀▀▀▀▀▀▀▀▀▀▀▀▀▀    \n",
            "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀   Новое: терминальный интерфейс\n",
            "  ▀▀▀▀    ▀▀▀▀     Совет: Shift+Enter — новая строка\n\n\n"
        )
    );
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
    assert_eq!(frame.lines[0], "› hello");
    assert_eq!(frame.cursor, Some((0, 7)));
    let mut r = Renderer::new(Vec::new(), 80, 24, ColorMode::None, true);
    r.paint(&frame).unwrap();
    r.finalize("answer\x1b[2J").unwrap();
    let output = String::from_utf8(r.into_inner()).unwrap();
    assert_eq!(output.matches("\x1b[?2026h").count(), 2);
    assert_eq!(output.matches("\x1b[?2026l").count(), 2);
    assert!(output.contains("\x1b8\r\x1b[2A\x1b[Janswer[2J\r\n"));
    assert!(!output.contains("1049"));
    let mut r = Renderer::new(Vec::new(), 80, 24, ColorMode::None, false);
    r.paint(&frame).unwrap();
    r.finalize("plain").unwrap();
    assert_eq!(r.into_inner(), b"plain\n");
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
