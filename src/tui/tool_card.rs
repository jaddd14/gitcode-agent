//! Compact tool calls and their results, arranged as a transcript tree.

use super::{plain, widgets};
use serde_json::Value;

/// Render the invocation immediately, while the tool is running.
pub fn started(name: &str, input: &Value, width: usize) -> Vec<String> {
    let glyphs = widgets::Glyphs::detect();
    let marker = if glyphs.ascii { "* " } else { "● " };
    let label = invocation(name, input);
    let width = width.max(3);
    super::markdown::wrap_words(&label, width.saturating_sub(2).max(1))
        .into_iter()
        .enumerate()
        .map(|(index, line)| format!("{}{line}", if index == 0 { marker } else { "  " }))
        .collect()
}

/// Render the result under its call. Keep indentation in command output intact.
pub fn completed(
    name: &str,
    output: &str,
    is_error: bool,
    _duration_ms: u64,
    width: usize,
) -> Vec<String> {
    let glyphs = widgets::Glyphs::detect();
    let branch = if glyphs.ascii { "  |_ " } else { "  ⎿  " };
    let continuation = "     ";
    let clean = plain(output);
    let mut lines: Vec<&str> = clean.lines().collect();
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut details = Vec::new();
    let mut states = Vec::new();

    if is_error {
        details.push(format!(
            "Error: {}",
            lines.first().copied().unwrap_or("Tool failed")
        ));
        details.extend(lines.iter().skip(1).map(|line| (*line).to_owned()));
    } else {
        match name {
            "Read" => details.push(format!(
                "Read {} {}",
                lines.len(),
                plural(lines.len(), "line", "lines")
            )),
            "LS" | "Glob" => {
                let (one, many) = if name == "LS" {
                    ("entry", "entries")
                } else {
                    ("match", "matches")
                };
                details.push(format!(
                    "Found {} {}",
                    lines.len(),
                    plural(lines.len(), one, many)
                ));
            }
            "Bash" | "BashOutput" => {
                for line in lines {
                    if let Some(code) = line
                        .strip_prefix("[exit code: ")
                        .and_then(|s| s.strip_suffix(']'))
                    {
                        states.push(format!("Exit code {code}"));
                    } else if line == "[command timed out]" {
                        states.push("Command timed out".into());
                    } else if line == "[output truncated]" {
                        states.push("Output truncated".into());
                    } else if let Some(state) = line.strip_prefix("finished=") {
                        details.push(if state == "true" {
                            "Finished".into()
                        } else {
                            "Running".into()
                        });
                    } else {
                        details.push(line.to_owned());
                    }
                }
                if details.is_empty() && states.is_empty() {
                    details.push("(No output)".into());
                }
            }
            _ => {
                details.extend(lines.iter().map(|line| (*line).to_owned()));
                if details.is_empty() {
                    details.push("Done".into());
                }
            }
        }
    }

    if details.is_empty() && !states.is_empty() {
        details.push(states.remove(0));
    }
    let hidden = details.len().saturating_sub(3);
    let mut rows: Vec<String> = details
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, line)| {
            widgets::ellipsis(
                &format!("{}{line}", if index == 0 { branch } else { continuation }),
                width,
                glyphs.ascii,
            )
        })
        .collect();
    if hidden > 0 {
        rows.push(widgets::ellipsis(
            &format!("{continuation}… {hidden} more lines"),
            width,
            glyphs.ascii,
        ));
    }
    for state in states {
        rows.push(widgets::ellipsis(
            &format!("{continuation}{state}"),
            width,
            glyphs.ascii,
        ));
    }
    rows
}

/// Render a subagent's Markdown result using the existing muted tool palette.
/// Only renderer-generated styling is retained; the agent's output is sanitized.
pub fn completed_agent(
    output: &str,
    width: usize,
    theme: &crate::theme::Theme,
    mode: crate::theme::ColorMode,
) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    let glyphs = widgets::Glyphs::detect();
    let branch = if glyphs.ascii { "  |_ " } else { "  ⎿  " };
    let continuation = "     ";
    let clean = plain(output);
    let mut lines: Vec<&str> = clean.lines().collect();
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push("Done");
    }
    let mut rows = Vec::new();
    for (index, line) in lines.iter().take(3).enumerate() {
        let prefix = if index == 0 { branch } else { continuation };
        let spans = super::markdown::inline(line);
        let full_width = UnicodeWidthStr::width(prefix)
            + spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                .sum::<usize>();
        let truncated = full_width > width;
        let limit = width.saturating_sub(usize::from(truncated));
        let mut used = 0;
        let mut body = String::new();
        let mut clipped = false;
        for grapheme in prefix.graphemes(true) {
            let cells = UnicodeWidthStr::width(grapheme);
            if used + cells > limit {
                clipped = true;
                break;
            }
            body.push_str(grapheme);
            used += cells;
        }
        if !clipped {
            for span in &spans {
                let mut piece = String::new();
                for grapheme in span.text.graphemes(true) {
                    let cells = UnicodeWidthStr::width(grapheme);
                    if used + cells > limit {
                        clipped = true;
                        break;
                    }
                    piece.push_str(grapheme);
                    used += cells;
                }
                if !piece.is_empty() {
                    if mode != crate::theme::ColorMode::None
                        && span.role == super::markdown::Role::Bold
                    {
                        body.push_str("\x1b[1m");
                    }
                    if mode != crate::theme::ColorMode::None
                        && span.role == super::markdown::Role::Italic
                    {
                        body.push_str("\x1b[3m");
                    }
                    body.push_str(&piece);
                    if mode != crate::theme::ColorMode::None
                        && span.role == super::markdown::Role::Bold
                    {
                        body.push_str("\x1b[22m");
                    }
                    if mode != crate::theme::ColorMode::None
                        && span.role == super::markdown::Role::Italic
                    {
                        body.push_str("\x1b[23m");
                    }
                }
                if clipped {
                    break;
                }
            }
        }
        if truncated && width > 0 {
            body.push_str(if glyphs.ascii { "." } else { "…" });
        }
        if mode != crate::theme::ColorMode::None {
            body = format!(
                "{}{body}{}",
                crate::theme::foreground(theme.placeholder, mode),
                crate::theme::reset(mode)
            );
        }
        rows.push(body);
    }
    let hidden = lines.len().saturating_sub(3);
    if hidden > 0 {
        let row = widgets::ellipsis(
            &format!("{continuation}… {hidden} more lines"),
            width,
            glyphs.ascii,
        );
        rows.push(if mode == crate::theme::ColorMode::None {
            row
        } else {
            format!(
                "{}{row}{}",
                crate::theme::foreground(theme.placeholder, mode),
                crate::theme::reset(mode)
            )
        });
    }
    rows
}

fn invocation(name: &str, input: &Value) -> String {
    let path = || value(input, &["path", "file_path", "notebook_path"]);
    let detail = match name {
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "LS" => path(),
        "Bash" => value(input, &["command"]),
        "Agent" => {
            let kind = value(input, &["subagent_type"]);
            let task = value(input, &["description"]);
            [kind, task]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        }
        "BashOutput" | "KillShell" => value(input, &["job_id"]),
        "Glob" => value(input, &["pattern"]),
        "Grep" => {
            let pattern = value(input, &["pattern"]);
            let path = value(input, &["path", "glob"]);
            match (pattern.is_empty(), path.is_empty()) {
                (false, false) => format!("{pattern} in {path}"),
                (false, true) => pattern,
                (true, false) => path,
                (true, true) => String::new(),
            }
        }
        "WebFetch" => value(input, &["url"]),
        "Skill" => value(input, &["name"]),
        "McpCall" => [value(input, &["server"]), value(input, &["tool"])]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/"),
        "TodoWrite" => "tasks".into(),
        _ => String::new(),
    };
    let detail = detail.replace('\r', "").replace('\n', " ↵ ");
    if detail.is_empty() {
        plain(name)
    } else {
        format!("{}({detail})", plain(name))
    }
}

fn value(input: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(plain)
        .unwrap_or_default()
}
fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bash_output_keeps_indentation_and_reports_hidden_lines() {
        let rows = completed("Bash", "one\n  indented\nthree\nfour\n", false, 25, 80);
        assert!(rows[0].contains("one"));
        assert_eq!(rows[1], "       indented");
        assert!(rows[3].contains("1 more lines"));
        assert!(!rows.iter().any(|row| row.contains("Exit 0")));
    }

    #[test]
    fn bash_empty_and_failure_states_are_visible() {
        assert!(completed("Bash", "", false, 0, 80)[0].contains("No output"));
        assert!(completed("Bash", "bad\n[exit code: 2]", false, 0, 80)
            .iter()
            .any(|r| r.contains("Exit code 2")));
        assert!(completed("Bash", "denied", true, 0, 80)[0].contains("Error: denied"));
    }

    #[test]
    fn agent_results_render_bold_and_inline_code_without_trusting_controls() {
        use crate::theme::{ColorMode, Theme};
        let theme = Theme::default();
        let output = "- **ОС:** Ubuntu 22.04\n- **Ядро:** Linux `6.8.0`\n- **Архитектура:** aarch64\n- **Скрыто:** ещё\n\x1b[31mred";
        let rows = completed_agent(output, 80, &theme, ColorMode::TrueColor);
        assert!(rows[0].contains("\x1b[1mОС:\x1b[22m"));
        assert!(rows[1].contains("\x1b[1mЯдро:\x1b[22m"));
        assert!(rows[1].contains("Linux 6.8.0"));
        assert!(!rows
            .iter()
            .any(|row| row.contains("**") || row.contains('`') || row.contains("\x1b[31m")));
        assert!(rows[0].starts_with(&crate::theme::foreground(
            theme.placeholder,
            ColorMode::TrueColor
        )));
        assert!(plain(&rows[3]).contains("2 more lines"));
        let plain_rows = completed_agent(output, 80, &theme, ColorMode::None);
        assert!(!plain_rows[0].contains('\x1b'));
        assert!(plain_rows[0].contains("ОС: Ubuntu"));
    }

    #[test]
    fn agent_bold_is_parsed_before_clipping_on_unicode_cell_boundaries() {
        use crate::theme::{ColorMode, Theme};
        let theme = Theme::default();
        for width in 0..30 {
            let rows = completed_agent(
                "- **Очень длинное название 界:** значение",
                width,
                &theme,
                ColorMode::TrueColor,
            );
            assert!(rows
                .iter()
                .all(|row| unicode_width::UnicodeWidthStr::width(plain(row).as_str()) <= width));
            assert!(rows.iter().all(|row| !plain(row).contains("**")));
            if rows[0].contains("\x1b[1m") {
                assert!(rows[0].contains("\x1b[22m"));
            }
        }
    }

    #[test]
    fn long_invocation_wraps_and_terminal_controls_are_removed() {
        let rows = started(
            "Bash",
            &json!({"command": "printf hello && printf world\u{1b}[31m"}),
            20,
        );
        assert!(rows.len() > 1);
        assert!(rows
            .iter()
            .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= 20));
        assert!(rows.iter().all(|row| !row.contains('\u{1b}')));
    }
}
