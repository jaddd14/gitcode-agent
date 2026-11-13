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

use crate::theme::{default_foreground, foreground, ColorMode, Theme};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Semantic flat-palette role; no source-provided terminal styling is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Ordinary prose.
    Text,
    /// Bold inline text.
    Bold,
    /// Emphasized inline text.
    Italic,
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
    pending_continuation: bool,
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
                self.parse_pending(&pending);
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
            self.parse_pending(&pending);
        }
        self.sanitizer = Sanitizer::default();
    }

    /// Completed parsed lines, excluding the provisional tail.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Move completed, stable lines into scrollback, retaining the last line
    /// because a following table delimiter can still change its interpretation.
    pub fn drain_stable(
        &mut self,
        width: usize,
        theme: &Theme,
        mode: ColorMode,
        target: usize,
    ) -> Vec<String> {
        let mut rows = self.render(width, theme, mode).len();
        let mut count = 0;
        let mut result = Vec::new();
        while rows > target && count + 1 < self.lines.len() {
            let mut end = count + 1;
            if !self.lines[count].cells.is_empty() {
                while end < self.lines.len() && !self.lines[end].cells.is_empty() {
                    end += 1;
                }
                // Column widths depend on every row; an open table must stay mutable.
                if end == self.lines.len() && self.table_columns.is_some() {
                    break;
                }
            }
            let rendered = render_lines(&self.lines[count..end], width, theme, mode);
            rows = rows.saturating_sub(rendered.len());
            result.extend(rendered);
            count = end;
        }
        self.lines.drain(..count);
        // Long unfinished paragraphs must not be trapped in the live viewport.
        // Commit whole rows only after their inline markup is complete.
        if rows > target
            && self.lines.is_empty()
            && self.fence.is_none()
            && self.table_columns.is_none()
            && !self.pending.contains('|')
            && (self.pending_continuation || {
                let trimmed = self.pending.trim_start();
                !trimmed.starts_with(['#', '>', '`', '~']) && list_item(trimmed).is_none()
            })
        {
            let all = wrap_prose(&inline(&self.pending), width);
            let available = all.len().saturating_sub(target.max(1));
            let mut best = None;
            for (index, part) in self.pending.split_word_bound_indices() {
                if index == 0 || part.chars().all(char::is_whitespace) {
                    continue;
                }
                if !inline_complete(&self.pending[..index]) {
                    continue;
                }
                let prefix = wrap_prose(&inline(&self.pending[..index]), width);
                if prefix.len() > available {
                    break;
                }
                if prefix.len() <= available && all.starts_with(&prefix) {
                    best = Some((index, prefix));
                }
            }
            if let Some((index, prefix)) = best {
                result.extend(prefix.iter().map(|row| paint(row, theme, mode)));
                self.pending.drain(..index);
                self.pending_continuation = true;
            }
        }
        result
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
            pending_continuation: self.pending_continuation,
            ..Self::default()
        };
        // A provisional table delimiter may reclassify the last committed line.
        let count = if self.lines.last().is_some_and(|line| !line.cells.is_empty()) {
            let start = self
                .lines
                .iter()
                .rposition(|line| line.cells.is_empty())
                .map_or(0, |index| index + 1);
            preview.lines.extend_from_slice(&self.lines[start..]);
            start
        } else if let Some(line) = self
            .lines
            .last()
            .filter(|line| line.kind == Kind::Paragraph)
        {
            preview.lines.push(line.clone());
            self.lines.len().saturating_sub(1)
        } else {
            self.lines.len()
        };
        preview.parse_pending(&self.pending);
        let mut rows = render_lines(&self.lines[..count], width, theme, mode);
        rows.extend(render_lines(&preview.lines, width, theme, mode));
        rows
    }

    fn parse_pending(&mut self, text: &str) {
        if self.pending_continuation {
            self.pending_continuation = false;
            self.add(Kind::Paragraph, "", vec![span(text, Role::Text)], vec![]);
        } else {
            self.parse_line(text);
        }
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
        // Consecutive empty source lines are a single visual separator: models
        // emit runs of newlines between calls and they must not stretch the view.
        if kind == Kind::Blank
            && self
                .lines
                .last()
                .is_some_and(|line| line.kind == Kind::Blank)
        {
            return;
        }
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

pub(super) fn inline(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let emphasis = if remaining.starts_with("**") {
            Some(("**", Role::Bold))
        } else if remaining.starts_with("__") {
            Some(("__", Role::Bold))
        } else if remaining.starts_with('*') {
            Some(("*", Role::Italic))
        } else if remaining.starts_with('_') {
            Some(("_", Role::Italic))
        } else {
            None
        };
        if let Some((delimiter, role)) = emphasis {
            let start = delimiter.len();
            if let Some(end) = remaining[start..].find(delimiter).filter(|end| *end > 0) {
                if !plain.is_empty() {
                    out.push(span(&std::mem::take(&mut plain), Role::Text));
                }
                out.push(span(&remaining[start..start + end], role));
                remaining = &remaining[start + end + start..];
                continue;
            }
        }
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

// A streamed prefix cannot be finalized while a delimiter may still close.
fn inline_complete(mut text: &str) -> bool {
    while !text.is_empty() {
        let delimiter = if text.starts_with("**") {
            Some("**")
        } else if text.starts_with("__") {
            Some("__")
        } else if text.starts_with('*') {
            Some("*")
        } else if text.starts_with('_') {
            Some("_")
        } else {
            None
        };
        if let Some(delimiter) = delimiter {
            let start = delimiter.len();
            let Some(end) = text[start..].find(delimiter).filter(|end| *end > 0) else {
                return false;
            };
            text = &text[start + end + start..];
        } else if text.starts_with('`') {
            let run = text.bytes().take_while(|b| *b == b'`').count();
            let Some(end) = text[run..].find(&text[..run]) else {
                return false;
            };
            text = &text[run + end + run..];
        } else if text.starts_with('[') {
            let Some(label_end) = text.find("](") else {
                return false;
            };
            let Some(end) = text[label_end + 2..].find(')') else {
                return false;
            };
            text = &text[label_end + 3 + end..];
        } else if let Some(ch) = text.chars().next() {
            text = &text[ch.len_utf8()..];
        }
    }
    true
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

/// Sanitize and wrap text at word boundaries, splitting only oversized words.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    Sanitizer::default()
        .push(text)
        .split('\n')
        .flat_map(|line| {
            wrap_prose(&[span(line, Role::Text)], width)
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

// Prefer word boundaries for prose while preserving graphemes and span styles.
fn wrap_prose(spans: &[Span], width: usize) -> Vec<Vec<Span>> {
    let mut rows: Vec<Vec<Span>> = vec![Vec::new()];
    let mut used = 0;
    let mut parts: Vec<Vec<(&str, Role)>> = Vec::new();
    for s in spans {
        for grapheme in s.text.graphemes(true) {
            let whitespace = grapheme.chars().all(char::is_whitespace);
            if parts
                .last()
                .is_none_or(|part| part[0].0.chars().all(char::is_whitespace) != whitespace)
            {
                parts.push(Vec::new());
            }
            if let Some(part) = parts.last_mut() {
                part.push((grapheme, s.role));
            }
        }
    }
    for part in parts {
        let size = part
            .iter()
            .map(|(text, _)| UnicodeWidthStr::width(*text))
            .sum::<usize>();
        let whitespace = part[0].0.chars().all(char::is_whitespace);
        if used > 0 && !whitespace && used + size > width {
            rows.push(Vec::new());
            used = 0;
        }
        let suffix_start = part.iter().rposition(|(text, _)| {
            !text
                .chars()
                .all(|c| ".,;:!?…)]}»”’。！？、，；：".contains(c))
        });
        let tail = suffix_start.map(|index| {
            (
                index,
                part[index..]
                    .iter()
                    .map(|(text, _)| UnicodeWidthStr::width(*text))
                    .sum::<usize>(),
            )
        });
        for (index, (grapheme, role)) in part.into_iter().enumerate() {
            let mut text = grapheme;
            let mut cells = UnicodeWidthStr::width(grapheme);
            if cells > width {
                text = "�";
                cells = 1;
            }
            if tail.is_some_and(|(start, size)| {
                index == start && size <= width && used > 0 && used + size > width
            }) {
                rows.push(Vec::new());
                used = 0;
            }
            if used + cells > width {
                if whitespace {
                    continue;
                }
                rows.push(Vec::new());
                used = 0;
            }
            if let Some(row) = rows.last_mut() {
                if let Some(last) = row.last_mut().filter(|last| last.role == role) {
                    last.text.push_str(text);
                } else {
                    row.push(span(text, role));
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
        match s.role {
            Role::Text | Role::Bold => out.push_str(default_foreground(mode)),
            Role::Italic => out.push_str(&foreground(theme.rose_soft, mode)),
            Role::Heading | Role::Marker | Role::Link => {
                out.push_str(&foreground(theme.rose, mode))
            }
            Role::Code => out.push_str(&foreground(theme.ok, mode)),
            Role::Muted => out.push_str(&foreground(theme.placeholder, mode)),
        }
        if matches!(s.role, Role::Bold | Role::Heading) {
            out.push_str("\x1b[1m");
        }
        if s.role == Role::Italic {
            out.push_str("\x1b[3m");
        }
        if s.role == Role::Link {
            out.push_str("\x1b[4m");
        }
        out.push_str(&s.text);
        out.push_str("\x1b[0m");
    }
    out
}

fn render_lines(lines: &[Line], width: usize, theme: &Theme, mode: ColorMode) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        if !line.cells.is_empty() {
            let mut end = index + 1;
            while end < lines.len() && !lines[end].cells.is_empty() {
                end += 1;
            }
            let group = &lines[index..end];
            let widths = table_widths(group, width);
            for (row_index, item) in group.iter().enumerate() {
                render_table(item, width, &widths, theme, mode, &mut out);
                if item.kind == Kind::TableRow {
                    let rule = Line {
                        kind: Kind::TableRule,
                        prefix: String::new(),
                        spans: Vec::new(),
                        cells: vec![vec![]; item.cells.len()],
                    };
                    let start = out.len();
                    render_table(&rule, width, &widths, theme, mode, &mut out);
                    if row_index + 1 == group.len() {
                        for row in &mut out[start..] {
                            *row = row.replace('┼', "┴");
                        }
                    }
                }
            }
            index = end;
            continue;
        }
        index += 1;
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
        let wrapped = if matches!(
            line.kind,
            Kind::Paragraph | Kind::Heading(_) | Kind::List | Kind::Quote
        ) {
            wrap_prose(&spans, width - prefix_width)
        } else {
            wrap_spans(&spans, width - prefix_width)
        };
        for (index, row) in wrapped.iter().enumerate() {
            let marker = if index == 0 {
                prefix.clone()
            } else {
                " ".repeat(prefix_width)
            };
            let mut rendered = if marker.is_empty() {
                String::new()
            } else {
                paint(&[span(&marker, Role::Marker)], theme, mode)
            };
            rendered.push_str(&paint(row, theme, mode));
            out.push(rendered);
        }
    }
    out
}

// Keep tables compact, while giving longer columns more room and respecting the screen.
fn table_widths(lines: &[Line], width: usize) -> Vec<usize> {
    let columns = lines.first().map_or(0, |line| line.cells.len());
    let mut widths = vec![1usize; columns];
    for line in lines {
        for (index, cell) in line.cells.iter().enumerate() {
            let natural = cell
                .iter()
                .map(|span| UnicodeWidthStr::width(span.text.as_str()))
                .sum::<usize>();
            widths[index] = widths[index].max(natural.min(60));
        }
    }
    let budget = width.saturating_sub(columns.saturating_sub(1) * 3);
    while widths.iter().sum::<usize>() > budget {
        let Some((index, value)) = widths.iter().enumerate().max_by_key(|(_, value)| *value) else {
            break;
        };
        if *value <= 1 {
            break;
        }
        widths[index] -= 1;
    }
    widths
}

fn render_table(
    line: &Line,
    width: usize,
    widths: &[usize],
    theme: &Theme,
    mode: ColorMode,
    out: &mut Vec<String>,
) {
    let columns = line.cells.len();
    // Narrow terminals stack cells instead of overflowing with separators.
    if columns.saturating_mul(4).saturating_sub(1) > width {
        if line.kind == Kind::TableRule {
            out.push(paint(&[span(&"─".repeat(width), Role::Muted)], theme, mode));
            return;
        }
        for cell in &line.cells {
            for row in wrap_spans(cell, width) {
                out.push(paint(&row, theme, mode));
            }
        }
        return;
    }
    if line.kind == Kind::TableRule {
        out.push(paint(
            &[span(
                &widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─"),
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
        .zip(widths)
        .map(|(cell, w)| wrap_prose(cell, *w))
        .collect::<Vec<_>>();
    let height = cells.iter().map(Vec::len).max().unwrap_or(1);
    for row in 0..height {
        let mut spans = Vec::new();
        for (column, cell) in cells.iter().enumerate() {
            if column > 0 {
                spans.push(span(" │ ", Role::Muted));
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
