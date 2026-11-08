//! Diff rendering is bounded and preserves explicit old/new line identities.
use gitcode::{theme::ColorMode, tui::diff};

#[test]
fn numbered_replacement_and_new_file() {
    let lines = diff::render(
        Some(b"same\nold\nend\n"),
        Some(b"same\nnew\nend\n"),
        80,
        ColorMode::None,
    );
    assert!(lines.iter().any(|s| s.contains("1     1   same")));
    assert!(lines.iter().any(|s| s.contains("- old")));
    assert!(lines.iter().any(|s| s.contains("+ new")));
    assert!(!lines.join("\n").contains('\x1b'));
    assert!(diff::render(None, Some(b"created\n"), 80, ColorMode::None)[0].contains("+ created"));
}

#[test]
fn large_changes_and_untrusted_controls_are_bounded() {
    let text = "line\n".repeat(10000);
    let rows = diff::render(None, Some(text.as_bytes()), 30, ColorMode::None);
    assert!(rows.len() <= 201);
    assert!(rows.last().is_some_and(|s| s.contains("truncated")));
    let rows = diff::render(None, Some(b"\x1b[31mhello\n"), 80, ColorMode::None);
    assert!(!rows.join("\n").contains('\x1b'));
}
