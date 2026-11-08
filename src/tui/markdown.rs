//! Incremental, line-based Markdown for native terminal scrollback.
//!
//! Feed arbitrary UTF-8 chunks to [`Markdown::push`], paint [`Markdown::render`]
//! as the mutable response area, then call [`Markdown::finish`] at EOF. Rendering
//! includes a speculative partial line without committing it. Re-render after a
//! resize; never append snapshots to scrollback repeatedly. The final snapshot
//! is suitable for `Renderer::finalize_styled`, and live rows for a `Frame` passed
//! to `Renderer::paint_styled`. Only generated SGR escapes reach styled output.
//!
//! This is deliberately not CommonMark: blocks are physical lines, tables use
//! equal-width columns, and fenced code has a flat color, not syntax highlighting.

use crate::theme::{foreground, ColorMode, Theme};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Semantic flat-palette role; no source-provided terminal styling is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Ordinary prose.
    Text,
    /// Heading text.
    Heading,
    /// List or quote marker.
    Marker,
    /// Inline or fenced code.
    Code,
    /// Visible link label and destination (never an OSC hyperlink).
    Link,
    /// Table structure or fence annotation.
    Muted,
}

/// Sanitized text with a semantic styling role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Printable text, without terminal escape sequences.
    pub text: String,
    /// Flat theme color to apply.
    pub role: Role,
}

/// Parsed physical line category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Empty line.
    Blank,
    /// Prose.
    Paragraph,
    /// ATX heading, level one through six.
    Heading(u8),
    /// Unordered or ordered list item.
    List,
    /// Block quote.
    Quote,
    /// Opening fence with optional language annotation, or closing fence.
    Fence,
    /// Literal fenced code, without syntax highlighting.
    Code,
    /// Table header recognized by the following delimiter row.
    TableHeader,
    /// Table body row.
    TableRow,
    /// Table delimiter row.
    TableRule,
}

/// One parsed line. Table cells are kept separate for width-aware layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Block category.
    pub kind: Kind,
    /// Structural prefix, repeated as spaces on wrapped continuations.
    pub prefix: String,
    /// Inline content (empty for tables).
    pub spans: Vec<Span>,
    /// Table cells, each containing inline spans.
    pub cells: Vec<Vec<Span>>,
}

#[derive(Debug, Clone, Copy, Default)]
enum Escape {
    #[default]
    Ground,
    Esc,
    Intermediate,
    Csi,
    String,
    StringEsc,
}

#[derive(Debug, Clone, Default)]
struct Sanitizer {
    state: Escape,
}
impl Sanitizer {
    fn push(&mut self, text: &str) -> String {
        let mut out = String::new();
        for c in text.chars() {
            self.state = match self.state {
                Escape::Ground => match c {
                    '\x1b' => Escape::Esc,
                    '\u{009b}' => Escape::Csi,
                    '\u{0090}' | '\u{0098}' | '\u{009d}' | '\u{009e}' | '\u{009f}' => {
                        Escape::String
                    }
                    '\n' => {
                        out.push(c);
                        Escape::Ground
                    }
                    '\t' => {
                        out.push_str("    ");
                        Escape::Ground
                    }
                    c if c.is_control() => Escape::Ground,
                    c => {
                        out.push(c);
                        Escape::Ground
                    }
                },
                Escape::Esc => match c {
                    '[' => Escape::Csi,
                    ']' | 'P' | 'X' | '^' | '_' => Escape::String,
                    '\x1b' => Escape::Esc,
                    '\x20'..='\x2f' => Escape::Intermediate,
                    _ => Escape::Ground,
                },
                Escape::Intermediate => match c {
                    '\x1b' => Escape::Esc,
                    '\x30'..='\x7e' => Escape::Ground,
                    _ => Escape::Intermediate,
                },
                Escape::Csi => match c {
                    '\x1b' => Escape::Esc,
                    '\x40'..='\x7e' => Escape::Ground,
                    _ => Escape::Csi,
                },
                Escape::String => match c {
                    '\x07' | '\u{009c}' => Escape::Ground,
                    '\x1b' => Escape::StringEsc,
                    _ => Escape::String,
                },
                Escape::StringEsc => match c {
                    '\\' | '\x07' | '\u{009c}' => Escape::Ground,
                    '\x1b' => Escape::StringEsc,
                    _ => Escape::String,
                },
            };
        }
        out
    }
}

/// Streaming parser. Completed lines are parsed once, not on every token.
///
/// A table delimiter can reclassify the immediately preceding prose line. Keep
/// snapshots mutable until EOF rather than treating `lines()` as an append log.
#[derive(Debug, Clone, Default)]
pub struct Markdown {
    sanitizer: Sanitizer,
    pending: String,
    lines: Vec<Line>,
    fence: Option<(char, usize)>,
    table_columns: Option<usize>,
}
impl Markdown {
    /// Construct an empty parser; no terminal or filesystem access is performed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a UTF-8 chunk. Escape sequences may straddle chunk boundaries.
    /// Newlines commit physical lines; an unfinished tail remains provisional.
    pub fn push(&mut self, chunk: &str) {
        let clean = self.sanitizer.push(chunk);
        for part in clean.split_inclusive('\n') {
            if let Some(line) = part.strip_suffix('\n') {
                self.pending.push_str(line);
                let pending = std::mem::take(&mut self.pending);
                self.parse_line(&pending);
            } else {
                self.pending.push_str(part);
            }
        }
    }

    /// Commit the unfinished final line and discard an unfinished ANSI command.
    /// Idempotent; an unclosed Markdown fence remains literal code through EOF.
    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.parse_line(&pending);
        }
        self.sanitizer = Sanitizer::default();
    }

    /// Completed parsed lines, excluding the provisional tail.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Render a complete snapshot, including the provisional tail, to trusted
    /// terminal rows. Width is in terminal cells; zero yields no rows. Wide
    /// graphemes that cannot fit even on an empty row become `�`, never split.
    /// `ColorMode::None` emits no escapes. Each styled span resets its own SGR.
    pub fn render(&self, width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
        if width == 0 {
            return Vec::new();
        }
        if self.pending.is_empty() {
            return render_lines(&self.lines, width, theme, mode);
        }
        // Clone parser state only for the speculative tail, not the full history.
        let mut preview = Self {
            fence: self.fence,
            table_columns: self.table_columns,
            ..Self::default()
        };
        // A provisional table delimiter may reclassify the last committed line.
        let previous = self
            .lines
            .last()
            .filter(|line| line.kind == Kind::Paragraph);
        let count = if let Some(line) = previous {
            preview.lines.push(line.clone());
            self.lines.len().saturating_sub(1)
        } else {
            self.lines.len()
        };
        preview.parse_line(&self.pending);
        let mut rows = render_lines(&self.lines[..count], width, theme, mode);
        rows.extend(render_lines(&preview.lines, width, theme, mode));
        rows
    }

    fn parse_line(&mut self, text: &str) {
        let trimmed = text.trim_start_matches(' ');
        let indent = text.len() - trimmed.len();
        if let Some((marker, length)) = self.fence {
            let run = trimmed.chars().take_while(|c| *c == marker).count();
            if indent <= 3 && run >= length && trimmed[run..].trim().is_empty() {
                self.fence = None;
                self.add(Kind::Fence, "", vec![], vec![]);
            } else {
                self.add(Kind::Code, "", vec![span(text, Role::Code)], vec![]);
            }
            return;
        }
        if indent <= 3 {
            if let Some(marker @ ('`' | '~')) = trimmed.chars().next() {
                let run = trimmed.chars().take_while(|c| *c == marker).count();
                if run >= 3 && (marker != '`' || !trimmed[run..].contains('`')) {
                    self.fence = Some((marker, run));
                    self.table_columns = None;
                    self.add(
                        Kind::Fence,
                        "",
                        vec![span(trimmed[run..].trim(), Role::Muted)],
                        vec![],
                    );
                    return;
                }
            }
        }
        if trimmed.is_empty() {
            self.table_columns = None;
            self.add(Kind::Blank, "", vec![], vec![]);
            return;
        }
        let cells = split_cells(text);
        if let Some(ref cells) = cells {
            if is_rule(cells)
                && self
                    .lines
                    .last()
                    .is_some_and(|line| line.kind == Kind::Paragraph)
            {
                if let Some(previous) = self.lines.last_mut() {
                    let source = previous
                        .spans
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<String>();
                    if let Some(headers) = split_cells(&source).filter(|h| h.len() == cells.len()) {
                        previous.kind = Kind::TableHeader;
                        previous.spans.clear();
                        previous.cells = headers.iter().map(|s| inline(s)).collect();
                        self.table_columns = Some(cells.len());
                        self.add(Kind::TableRule, "", vec![], vec![vec![]; cells.len()]);
                        return;
                    }
                }
            }
            if self.table_columns == Some(cells.len()) {
                self.add(
                    Kind::TableRow,
                    "",
                    vec![],
                    cells.iter().map(|s| inline(s)).collect(),
                );
                return;
            }
        }
        self.table_columns = None;
        if indent <= 3 {
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&hashes)
                && (trimmed.len() == hashes || trimmed[hashes..].starts_with(' '))
            {
                let body = trimmed[hashes..].trim();
                self.add(
                    Kind::Heading(hashes as u8),
                    "",
                    vec![span(body, Role::Heading)],
                    vec![],
                );
                return;
            }
            if let Some(body) = trimmed.strip_prefix('>') {
                self.add(
                    Kind::Quote,
                    "│ ",
                    inline(body.strip_prefix(' ').unwrap_or(body)),
                    vec![],
                );
                return;
            }
        }
        if let Some((marker, body)) = list_item(trimmed) {
            let prefix = format!("{}{} ", " ".repeat(indent.min(8)), marker);
            self.add(Kind::List, &prefix, inline(body), vec![]);
            return;
        }
        // Preserve source until a possible following table delimiter is known.
        self.add(Kind::Paragraph, "", vec![span(text, Role::Text)], vec![]);
    }

    fn add(&mut self, kind: Kind, prefix: &str, spans: Vec<Span>, cells: Vec<Vec<Span>>) {
        self.lines.push(Line {
            kind,
            prefix: prefix.to_owned(),
            spans,
            cells,
        });
    }
}

fn span(text: &str, role: Role) -> Span {
    Span {
        text: text.to_owned(),
        role,
    }
}

fn list_item(text: &str) -> Option<(String, &str)> {
    for marker in ["- ", "+ ", "* "] {
        if let Some(body) = text.strip_prefix(marker) {
            return Some(("•".into(), body));
        }
    }
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &text[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some((text[..digits + 1].to_owned(), &text[digits + 2..]));
        }
    }
    None
}

fn inline(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.starts_with('`') {
            let run = remaining.bytes().take_while(|b| *b == b'`').count();
            let delimiter = &remaining[..run];
            if let Some(end) = remaining[run..].find(delimiter) {
                if !plain.is_empty() {
                    out.push(span(&std::mem::take(&mut plain), Role::Text));
                }
                out.push(span(&remaining[run..run + end], Role::Code));
                remaining = &remaining[run + end + run..];
                continue;
            }
        }
        if remaining.starts_with('[') {
            if let Some(label_end) = remaining.find("](") {
                if let Some(end) = remaining[label_end + 2..].find(')') {
                    if !plain.is_empty() {
                        out.push(span(&std::mem::take(&mut plain), Role::Text));
                    }
                    let label = &remaining[1..label_end];
                    let url = &remaining[label_end + 2..label_end + 2 + end];
                    out.push(span(&format!("{label} ({url})"), Role::Link));
                    remaining = &remaining[label_end + 3 + end..];
                    continue;
                }
            }
        }
        if let Some(c) = remaining.chars().next() {
            plain.push(c);
            remaining = &remaining[c.len_utf8()..];
        }
    }
    if !plain.is_empty() {
        out.push(span(&plain, Role::Text));
    }
    out
}

fn split_cells(text: &str) -> Option<Vec<String>> {
    if !text.contains('|') {
        return None;
    }
    let text = text.trim();
    let text = text.strip_prefix('|').unwrap_or(text);
    let text = text.strip_suffix('|').unwrap_or(text);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    let mut ticks = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if escaped {
            cell.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '`' {
            let mut run = 1;
            cell.push(c);
            while chars.peek() == Some(&'`') {
                chars.next();
                cell.push('`');
                run += 1;
            }
            if ticks == 0 {
                ticks = run;
            } else if ticks == run {
                ticks = 0;
            }
        } else if c == '|' && ticks == 0 {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(c);
        }
    }
    if escaped {
        cell.push('\\');
    }
    cells.push(cell.trim().to_owned());
    Some(cells)
}

fn is_rule(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let body = cell.trim_matches(':');
            body.len() >= 3 && body.bytes().all(|b| b == b'-')
        })
}

/// Sanitize untrusted text and hard-wrap at Unicode grapheme boundaries.
/// Newlines are preserved, tabs expand to four spaces. Zero width yields no
/// rows; too-wide graphemes become `�`. Does not interpret Markdown or ANSI.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![];
    }
    let clean = Sanitizer::default().push(text);
    clean
        .split('\n')
        .flat_map(|line| {
            wrap_spans(&[span(line, Role::Text)], width)
                .into_iter()
                .map(|row| row.into_iter().map(|s| s.text).collect::<String>())
        })
        .collect()
}

fn wrap_spans(spans: &[Span], width: usize) -> Vec<Vec<Span>> {
    let mut rows: Vec<Vec<Span>> = vec![Vec::new()];
    let mut used = 0;
    for s in spans {
        for grapheme in s.text.graphemes(true) {
            let mut text = grapheme;
            let mut cells = UnicodeWidthStr::width(grapheme);
            if cells > width {
                text = "�";
                cells = 1;
            }
            if used + cells > width {
                rows.push(Vec::new());
                used = 0;
            }
            if let Some(row) = rows.last_mut() {
                if let Some(last) = row
                    .last_mut()
                    .filter(|last: &&mut Span| last.role == s.role)
                {
                    last.text.push_str(text);
                } else {
                    row.push(span(text, s.role));
                }
            }
            used += cells;
        }
    }
    rows
}

fn paint(spans: &[Span], theme: &Theme, mode: ColorMode) -> String {
    let mut out = String::new();
    for s in spans {
        if mode == ColorMode::None {
            out.push_str(&s.text);
            continue;
        }
        let color = match s.role {
            Role::Text => theme.text,
            Role::Heading => theme.purple_soft,
            Role::Marker => theme.pink,
            Role::Code => theme.pink_soft,
            Role::Link => theme.purple,
            Role::Muted => theme.dim,
        };
        out.push_str(&foreground(color, mode));
        out.push_str(&s.text);
        out.push_str("\x1b[0m");
    }
    out
}

fn render_lines(lines: &[Line], width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        if !line.cells.is_empty() {
            render_table(line, width, theme, mode, &mut out);
            continue;
        }
        let prefix = wrap(&line.prefix, width)
            .into_iter()
            .next()
            .unwrap_or_default();
        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
        // At tiny widths prefer content over a marker consuming the entire row.
        let (prefix, prefix_width) = if prefix_width >= width {
            (String::new(), 0)
        } else {
            (prefix, prefix_width)
        };
        let spans = if line.kind == Kind::Paragraph {
            inline(
                &line
                    .spans
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>(),
            )
        } else {
            line.spans.clone()
        };
        for (index, row) in wrap_spans(&spans, width - prefix_width).iter().enumerate() {
            let marker = if index == 0 {
                prefix.clone()
            } else {
                " ".repeat(prefix_width)
            };
            let mut rendered = paint(&[span(&marker, Role::Marker)], theme, mode);
            rendered.push_str(&paint(row, theme, mode));
            out.push(rendered);
        }
    }
    out
}

fn render_table(line: &Line, width: usize, theme: &Theme, mode: ColorMode, out: &mut Vec<String>) {
    let columns = line.cells.len();
    // Narrow terminals stack cells instead of overflowing with separators.
    if columns.saturating_mul(2).saturating_sub(1) > width {
        for cell in &line.cells {
            for row in wrap_spans(cell, width) {
                out.push(paint(&row, theme, mode));
            }
        }
        return;
    }
    let budget = width - (columns - 1);
    let widths = (0..columns)
        .map(|i| budget / columns + usize::from(i < budget % columns))
        .collect::<Vec<_>>();
    if line.kind == Kind::TableRule {
        out.push(paint(
            &[span(
                &widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("┼"),
                Role::Muted,
            )],
            theme,
            mode,
        ));
        return;
    }
    let cells = line
        .cells
        .iter()
        .zip(&widths)
        .map(|(cell, w)| wrap_spans(cell, *w))
        .collect::<Vec<_>>();
    let height = cells.iter().map(Vec::len).max().unwrap_or(1);
    for row in 0..height {
        let mut spans = Vec::new();
        for (column, cell) in cells.iter().enumerate() {
            if column > 0 {
                spans.push(span("│", Role::Muted));
            }
            let content = cell.get(row).cloned().unwrap_or_default();
            let used = content
                .iter()
                .map(|s| UnicodeWidthStr::width(s.text.as_str()))
                .sum::<usize>();
            spans.extend(content.into_iter().map(|mut s| {
                if line.kind == Kind::TableHeader && s.role == Role::Text {
                    s.role = Role::Heading;
                }
                s
            }));
            spans.push(span(
                &" ".repeat(widths[column].saturating_sub(used)),
                Role::Text,
            ));
        }
        out.push(paint(&spans, theme, mode));
    }
}
