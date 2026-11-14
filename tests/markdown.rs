//! Streaming Markdown contract and terminal-safety regression tests.
#![allow(clippy::unwrap_used)]

// Keep this feature isolated until the application exports the M5 module.
#[path = "../src/tui/markdown.rs"]
mod markdown;
use gitcode::theme;
use markdown::{Kind, Markdown};
use theme::{ColorMode, Theme};
use unicode_width::UnicodeWidthStr;

fn plain(parser: &Markdown, width: usize) -> Vec<String> {
    parser.render(width, &Theme::default(), ColorMode::None)
}

fn parse(text: &str) -> Markdown {
    let mut parser = Markdown::new();
    parser.push(text);
    parser.finish();
    parser
}

#[test]
fn chunks_commit_only_complete_lines_and_preview_tail() {
    let mut parser = Markdown::new();
    parser.push("# He");
    assert!(parser.lines().is_empty());
    assert_eq!(plain(&parser, 80), ["He"]);
    parser.push("ading\n- wor");
    assert_eq!(parser.lines().len(), 1);
    assert_eq!(parser.lines()[0].kind, Kind::Heading(1));
    assert_eq!(plain(&parser, 80), ["Heading", "• wor"]);
    parser.push("ld\n");
    parser.finish();
    let snapshot = plain(&parser, 80);
    parser.finish();
    assert_eq!(plain(&parser, 80), snapshot);
    assert_eq!(snapshot, ["Heading", "• world"]);
}

#[test]
fn recognizes_blocks_inline_code_and_visible_links() {
    let parser =
        parse("###### Title\n\n- one\n12. two\n> a `code` [site](https://example.org)\nnormal\n");
    assert_eq!(
        parser
            .lines()
            .iter()
            .map(|line| line.kind.clone())
            .collect::<Vec<_>>(),
        [
            Kind::Heading(6),
            Kind::Blank,
            Kind::List,
            Kind::List,
            Kind::Quote,
            Kind::Paragraph,
        ]
    );
    assert_eq!(
        plain(&parser, 80),
        [
            "Title",
            "",
            "• one",
            "12. two",
            "│ a code site (https://example.org)",
            "normal"
        ]
    );
    assert!(parser.lines()[4]
        .spans
        .iter()
        .any(|s| s.role == markdown::Role::Code));
    assert!(parser.lines()[4]
        .spans
        .iter()
        .any(|s| s.role == markdown::Role::Link));
    assert_eq!(
        plain(&parse("text ``a`b`` end and `unfinished"), 80),
        ["text a`b end and `unfinished"]
    );
    assert_eq!(
        plain(&parse("####### not a heading\n#nospace\n-plain"), 80),
        ["####### not a heading", "#nospace", "-plain"]
    );
}

#[test]
fn fences_are_literal_and_require_matching_marker_and_length() {
    let parser =
        parse("````rust\n# literal [x](y)\n```\n~~~~\n````\n~~~text\n- still code\n~~~\nend");
    assert_eq!(
        parser
            .lines()
            .iter()
            .map(|l| l.kind.clone())
            .collect::<Vec<_>>(),
        [
            Kind::Fence,
            Kind::Code,
            Kind::Code,
            Kind::Code,
            Kind::Fence,
            Kind::Fence,
            Kind::Code,
            Kind::Fence,
            Kind::Paragraph,
        ]
    );
    assert_eq!(
        plain(&parser, 80),
        [
            "rust",
            "# literal [x](y)",
            "```",
            "~~~~",
            "",
            "text",
            "- still code",
            "",
            "end"
        ]
    );
    assert_eq!(plain(&parse("```\n  x\t y"), 80), ["", "  x     y"]);
}

#[test]
fn tables_reclassify_headers_wrap_cells_and_keep_following_prose() {
    let parser = parse("| Name | Value |\n| :--- | ---: |\n| alpha | 世界世界 |\nafter");
    assert_eq!(parser.lines()[0].kind, Kind::TableHeader);
    assert_eq!(parser.lines()[1].kind, Kind::TableRule);
    assert_eq!(parser.lines()[2].kind, Kind::TableRow);
    assert_eq!(parser.lines()[3].kind, Kind::Paragraph);
    let rows = plain(&parser, 15);
    assert_eq!(
        rows,
        [
            "┌──────┬──────┐",
            "│ Name │ Valu │",
            "│      │ e    │",
            "├──────┼──────┤",
            "│ alph │ 世界 │",
            "│ a    │ 世界 │",
            "└──────┴──────┘",
            "after",
        ]
    );
    let parser = parse("| `a|b` | c\\|d |\n| --- | --- |\n| z | y |");
    assert_eq!(parser.lines()[0].cells.len(), 2);
    assert_eq!(parser.lines()[0].cells[0][0].text, "a|b");
    assert_eq!(parser.lines()[0].cells[1][0].text, "c|d");
    let not_table = parse("a | b\n-- | ---\n");
    assert!(not_table.lines().iter().all(|l| l.kind == Kind::Paragraph));
}

#[test]
fn tables_close_borders_with_no_body_and_when_columns_must_stack() {
    let header = parse("| A | B |\n| --- | --- |\n");
    assert_eq!(plain(&header, 30), ["┌───┬───┐", "│ A │ B │", "└───┴───┘"]);
    let parser = parse("| A | B |\n| --- | --- |\n| 世界 | long |\n");
    for width in 3..9 {
        let rows = plain(&parser, width);
        assert!(rows.first().unwrap().starts_with('┌'));
        assert!(rows.last().unwrap().starts_with('└'));
        assert!(rows
            .iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) <= width));
        assert!(rows
            .iter()
            .all(|row| matches!(row.chars().next(), Some('┌' | '├' | '└' | '│'))));
        assert!(rows
            .iter()
            .all(|row| matches!(row.chars().last(), Some('┐' | '┤' | '┘' | '│'))));
    }
}

#[test]
fn emphasis_is_styled_and_plain_text_has_no_markers() {
    let parser =
        parse("# Заголовок\nОбычный **жирный** и *курсив*, `код` и [ссылка](https://example.org).");
    let rows = parser.render(100, &Theme::default(), ColorMode::TrueColor);
    assert!(rows[0].contains("\x1b[1mЗаголовок"));
    assert!(rows[1].contains("\x1b[1mжирный"));
    assert!(rows[1].contains("\x1b[3mкурсив"));
    assert!(rows[1].contains("\x1b[4mссылка"));
    assert!(!plain(&parser, 100).join("\n").contains("**"));
}

#[test]
fn streaming_drains_only_stable_lines_without_losing_table_headers() {
    let mut parser = Markdown::new();
    parser.push("first\nsecond\n| Header | Value |\n");
    let drained = parser.drain_stable(30, &Theme::default(), ColorMode::None, 1);
    assert_eq!(drained, ["first", "second"]);
    parser.push("| --- | --- |\n| x | y |\n");
    assert_eq!(parser.lines()[0].kind, Kind::TableHeader);
    let mut all = drained;
    all.extend(plain(&parser, 30));
    assert!(all.join("\n").contains("Header"));
    assert_eq!(
        all.iter().filter(|line| line.as_str() == "first").count(),
        1
    );
}

#[test]
fn long_unfinished_paragraph_streams_without_duplication() {
    let text = "один два три четыре пять шесть семь восемь девять десять ".repeat(8);
    let expected = plain(&parse(&text), 16);
    let mut parser = Markdown::new();
    let mut actual = Vec::new();
    for chunk in text.split_inclusive(' ') {
        parser.push(chunk);
        actual.extend(parser.drain_stable(16, &Theme::default(), ColorMode::None, 4));
    }
    parser.finish();
    actual.extend(plain(&parser, 16));
    assert_eq!(actual, expected);
}

#[test]
fn long_unfinished_markdown_preserves_inline_styles_while_streaming() {
    let text = "один **жирный текст** и `код` слова ".repeat(12);
    let mode = ColorMode::TrueColor;
    let expected = parse(&text).render(24, &Theme::default(), mode);
    let mut parser = Markdown::new();
    let mut actual = Vec::new();
    for chunk in text.split_inclusive(' ') {
        parser.push(chunk);
        actual.extend(parser.drain_stable(24, &Theme::default(), mode, 4));
    }
    parser.finish();
    actual.extend(parser.render(24, &Theme::default(), mode));
    assert_eq!(actual, expected);
}

#[test]
fn pending_delimiter_preview_matches_committed_table() {
    let mut parser = Markdown::new();
    parser.push("a | b\n--- | ---");
    let preview = plain(&parser, 15);
    assert_eq!(parser.lines()[0].kind, Kind::Paragraph);
    parser.push("\n");
    assert_eq!(plain(&parser, 15), preview);
    assert_eq!(parser.lines()[0].kind, Kind::TableHeader);
}

#[test]
fn unicode_wrap_preserves_clusters_and_cell_bounds() {
    assert_eq!(markdown::wrap("a界e\u{301}🙂", 3), ["a界", "e\u{301}🙂"]);
    assert_eq!(markdown::wrap("👨‍👩‍👧‍👦x", 2), ["👨‍👩‍👧‍👦", "x"]);
    assert_eq!(markdown::wrap("界🙂", 1), ["�", "�"]);
    assert_eq!(markdown::wrap("a\n\n", 4), ["a", "", ""]);
    assert!(markdown::wrap("abc", 0).is_empty());
    assert!(plain(&parse("hi"), 0).is_empty());
    assert_eq!(plain(&parse("- abcdef"), 5), ["• abc", "  def"]);
}

#[test]
fn prose_wraps_words_without_splitting_them_unless_too_long() {
    assert_eq!(markdown::wrap_words("привет мир", 8), ["привет ", "мир"]);
    assert_eq!(
        markdown::wrap_words("один длинноеслово", 8),
        ["один ", "длинноес", "лово"]
    );
    assert_eq!(plain(&parse("привет мир"), 8), ["привет ", "мир"]);
}

#[test]
fn punctuation_stays_with_words_across_styles_and_in_tables() {
    for punctuation in [".", ",", "?", "!", ":", ";", "…", "?!", "».", ")"] {
        let text = format!("Это слово{punctuation}");
        let rows = markdown::wrap_words(&text, 9);
        assert_eq!(rows, ["Это ".to_owned(), format!("слово{punctuation}")]);
        let styled = format!("Это **слово**{punctuation}");
        assert_eq!(plain(&parse(&styled), 9), rows);
    }
    assert_eq!(
        markdown::wrap_words("драматургии.", 11),
        ["драматурги", "и."]
    );
    let parser =
        parse("| Область | Значение |\n| --- | --- |\n| Жанры | Поэзия и драматургии. |\n");
    let rows = plain(&parser, 23);
    assert!(rows.last().unwrap().contains('┴'));
    for row in rows.iter().filter(|row| row.contains('│')) {
        let cell = row.split('│').nth(1).unwrap().trim();
        assert_ne!(cell, ".");
        assert!(UnicodeWidthStr::width(row.as_str()) <= 23);
    }
}

#[test]
fn untrusted_ansi_is_removed_including_payloads_and_chunked_controls() {
    let source = "ok\x1b[2J\x1b[31mred\x1b[0m\x1b]52;c;secret\x07\x1bPpayload\x1b\\\u{009b}2J\u{009d}title\u{009c}\x1b(B!\r\x08\x00\n\tend";
    let mut parser = Markdown::new();
    for c in source.chars() {
        parser.push(&c.to_string());
    }
    parser.finish();
    assert_eq!(plain(&parser, 80), ["okred!", "    end"]);
    assert_eq!(markdown::wrap(source, 80), ["okred!", "    end"]);
    assert_eq!(
        plain(&parse("safe\x1b]unterminated malicious\nmore"), 80),
        ["safe"]
    );
    assert_eq!(
        plain(&parse("[x](\x1b]8;;evil\x1b\\https://safe)"), 80),
        ["x (https://safe)"]
    );
}

#[test]
fn streaming_is_invariant_for_every_utf8_chunk_boundary() {
    let source = "# 标题\r\n> `é` [link](https://example.org)\n| a | b |\n| --- | --- |\n| 世界 | 🙂 |\n```rs\nlet x = 1;\x1b[2J\n```\ntail";
    let expected = parse(source);
    for (boundary, _) in source
        .char_indices()
        .chain(std::iter::once((source.len(), ' ')))
    {
        let mut parser = Markdown::new();
        parser.push(&source[..boundary]);
        // Rendering previews must not mutate parser state.
        let _ = plain(&parser, 9);
        parser.push(&source[boundary..]);
        parser.finish();
        assert_eq!(parser.lines(), expected.lines(), "boundary {boundary}");
        assert_eq!(
            plain(&parser, 13),
            plain(&expected, 13),
            "boundary {boundary}"
        );
    }
}

#[test]
fn all_widths_and_color_modes_remain_bounded_and_only_generate_sgr(
) -> Result<(), Box<dyn std::error::Error>> {
    let regex = regex::Regex::new("\x1b\\[[0-9;]*m")?;
    let parser = parse("# heading\n- 世界🙂 `code`\n> quoted text\n| a | b | c |\n| --- | --- | --- |\n| 👨‍👩‍👧‍👦 | longword | 界 |\n```\n    literal界\n```");
    for width in 1..=30 {
        let expected = plain(&parser, width);
        for row in &expected {
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= width,
                "{width}: {row:?}"
            );
        }
        for mode in [
            ColorMode::TrueColor,
            ColorMode::Indexed256,
            ColorMode::Ansi16,
        ] {
            let styled = parser.render(width, &Theme::default(), mode);
            let stripped = styled
                .iter()
                .map(|row| {
                    let clean = regex.replace_all(row, "").into_owned();
                    assert!(!clean.contains('\x1b'));
                    clean
                })
                .collect::<Vec<_>>();
            assert_eq!(stripped, expected);
        }
    }
    Ok(())
}

#[test]
fn supports_trusted_renderer_output_and_resize_without_reparse() -> std::io::Result<()> {
    let parser = parse("# Title\n- stream `content`");
    let before = parser.lines().to_vec();
    let narrow = parser.render(7, &Theme::default(), ColorMode::TrueColor);
    let wide = parser.render(40, &Theme::default(), ColorMode::TrueColor);
    assert!(narrow.len() > wide.len());
    assert_eq!(parser.lines(), before);
    let mut sink =
        gitcode::tui::render::Renderer::new(Vec::new(), 40, 20, ColorMode::TrueColor, false);
    sink.finalize_styled(&wide)?;
    assert!(!sink.into_inner().contains(&0x1b));
    Ok(())
}

#[test]
fn table_rows_have_horizontal_separators_in_stream_and_final_output() {
    let text = "| Год | Событие |\n| --- | --- |\n| 1799 | Рождение |\n| 1837 | Дуэль |\n";
    let expected = plain(&parse(text), 40);
    assert_eq!(expected.iter().filter(|row| row.contains('┼')).count(), 2);
    assert!(expected.last().unwrap().contains('┴'));
    assert!(!expected.last().unwrap().contains('┼'));
    let mut stream = Markdown::new();
    let mut rendered = Vec::new();
    for line in text.split_inclusive('\n') {
        stream.push(line);
        rendered.extend(stream.drain_stable(40, &Theme::default(), ColorMode::None, 1));
    }
    stream.finish();
    rendered.extend(plain(&stream, 40));
    assert_eq!(rendered, expected);
}

#[test]
fn tables_fit_content_and_keep_columns_aligned_across_streaming_and_resize() {
    let text = "| Год | Событие |\n| --- | --- |\n| 1799 | Рождение |\n| 1837 | Очень длинное историческое событие |\n";
    let parser = parse(text);
    let wide = plain(&parser, 150);
    assert!(wide
        .iter()
        .all(|row| UnicodeWidthStr::width(row.as_str()) < 50));
    let separators: Vec<usize> = wide
        .iter()
        .filter_map(|row| row.find('│').map(|i| UnicodeWidthStr::width(&row[..i])))
        .collect();
    assert!(separators.len() >= 3 && separators.iter().all(|col| *col == separators[0]));
    let narrow = plain(&parser, 24);
    assert!(narrow
        .iter()
        .all(|row| UnicodeWidthStr::width(row.as_str()) <= 24));
    assert!(narrow.len() > wide.len());
    let mut stream = Markdown::new();
    let mut output = Vec::new();
    for chunk in text.chars() {
        stream.push(&chunk.to_string());
        output.extend(stream.drain_stable(150, &Theme::default(), ColorMode::None, 1));
    }
    stream.finish();
    output.extend(plain(&stream, 150));
    assert_eq!(output, wide);

    let followed = format!("{text}\nПосле таблицы.\n");
    let mut stream = Markdown::new();
    let mut output = Vec::new();
    for line in followed.split_inclusive('\n') {
        stream.push(line);
        output.extend(stream.drain_stable(150, &Theme::default(), ColorMode::None, 1));
    }
    stream.finish();
    output.extend(plain(&stream, 150));
    assert_eq!(output, plain(&parse(&followed), 150));
}

#[test]
fn prose_uses_terminal_foreground_while_accent_colors_are_preserved() {
    let parser = parse("обычный **жирный** [ссылка](https://example.org)");
    let theme = Theme::default();
    for mode in [
        ColorMode::TrueColor,
        ColorMode::Indexed256,
        ColorMode::Ansi16,
    ] {
        let row = parser.render(100, &theme, mode).join("\n");
        assert!(row.contains("\x1b[39mобычный"));
        assert!(row.contains(&format!(
            "{}\x1b[1mжирный",
            theme::foreground(theme.rose, mode)
        )));
        assert!(row.contains(&theme::foreground(theme.rose, mode)));
        assert!(!row.contains(&theme::foreground(theme.text, mode)));
    }
    let plain = parser.render(100, &theme, ColorMode::None).join("\n");
    assert!(!plain.contains('\x1b'));
}

#[test]
fn runs_of_blank_source_lines_render_as_one_separator() {
    let rows = plain(&parse("Первый шаг.\n\n\n\nВторой шаг.\n\n\n"), 40);
    assert_eq!(rows.first().map(String::as_str), Some("Первый шаг."));
    assert!(rows.iter().any(|row| row == "Второй шаг."));
    assert!(
        !rows
            .windows(2)
            .any(|pair| pair[0].is_empty() && pair[1].is_empty()),
        "{rows:?}"
    );
}
