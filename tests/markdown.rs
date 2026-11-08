//! Streaming Markdown contract and terminal-safety regression tests.

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
    let rows = plain(&parser, 11);
    assert_eq!(rows[0], "Name │Value");
    assert_eq!(rows[1], "─────┼─────");
    assert_eq!(rows[2], "alpha│世界 ");
    assert_eq!(rows[3], "     │世界 ");
    assert_eq!(rows[4], "after");
    let parser = parse("| `a|b` | c\\|d |\n| --- | --- |\n| z | y |");
    assert_eq!(parser.lines()[0].cells.len(), 2);
    assert_eq!(parser.lines()[0].cells[0][0].text, "a|b");
    assert_eq!(parser.lines()[0].cells[1][0].text, "c|d");
    let not_table = parse("a | b\n-- | ---\n");
    assert!(not_table.lines().iter().all(|l| l.kind == Kind::Paragraph));
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
