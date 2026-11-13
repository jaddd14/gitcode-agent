//! Slash completion is a pure, immediately painted editor overlay.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use gitcode::tui::{
    input::{Action, Editor},
    render::Frame,
    statusbar::Status,
};
use unicode_width::UnicodeWidthStr;

fn key(editor: &mut Editor, code: KeyCode) -> Action {
    editor.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[test]
fn slash_opens_without_completion_action_and_escape_preserves_text() {
    let mut editor = Editor::default();
    assert_eq!(key(&mut editor, KeyCode::Char('/')), Action::Redraw);
    assert!(editor.completion_open());
    let frame = Frame::input(&editor, &Status::default(), 80, 36);
    assert!(frame.lines.iter().any(|line| line.contains("Команды")));
    assert!(frame.lines.iter().all(|line| line.width() <= 80));
    key(&mut editor, KeyCode::Down);
    assert_eq!(editor.completion_selected(), 1);
    assert_eq!(key(&mut editor, KeyCode::Esc), Action::Redraw);
    assert!(!editor.completion_open());
    assert_eq!(editor.text(), "/");
}

#[test]
fn tab_inserts_and_enter_executes_selected_command() {
    let mut editor = Editor::default();
    editor.set_text("/us");
    key(&mut editor, KeyCode::Tab);
    assert_eq!(editor.text(), "/usage ");
    assert!(editor.completion_open());
    assert_eq!(
        key(&mut editor, KeyCode::Enter),
        Action::Submit("/usage ".into())
    );
    editor.set_text("/us");
    assert_eq!(
        key(&mut editor, KeyCode::Enter),
        Action::Submit("/usage".into())
    );
}

#[test]
fn every_builtin_is_reachable_in_the_same_popup() {
    let choices = gitcode::tui::commands::candidates("/");
    assert_eq!(
        choices.len(),
        gitcode::extensibility::BUILTIN_COMMANDS.len()
    );
    for command in gitcode::extensibility::BUILTIN_COMMANDS {
        let selected = choices
            .iter()
            .position(|choice| choice.name == command.name)
            .unwrap_or(usize::MAX);
        let rows = gitcode::tui::commands::popup("/", selected, 80, 9);
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("/{} ", command.name))),
            "{}",
            command.name
        );
    }
}

#[test]
fn ordinary_paste_and_arguments_do_not_open_the_command_menu() {
    let mut editor = Editor::default();
    editor.set_text(" new (вот тут был слеш new но пришлось убрать) · новый диалог\n│  /clear · очистить диалог");
    assert!(!editor.completion_open());
    let frame = Frame::input(&editor, &Status::default(), 80, 36);
    assert!(!frame.lines.iter().any(|line| line.contains("Команды")));

    editor.set_text("/usage extra");
    assert!(!editor.completion_open());
    editor.set_text("/usage\nmore");
    assert!(!editor.completion_open());
    editor.set_text("/not-a-command");
    assert!(!editor.completion_open());

    editor.set_text("/usa");
    assert!(editor.completion_open());
    editor.set_text("/usage ");
    assert!(editor.completion_open());
}
