//! Deterministic terminal capability and privacy-safe feedback coverage.

use gitcode::{
    theme::ColorMode,
    tui::{
        capabilities::Capabilities,
        render::{Frame, Renderer},
    },
};

fn detect(windows: bool, vars: &[(&str, &str)]) -> Capabilities {
    Capabilities::detect_with(true, windows, |name| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    })
}

#[test]
fn conservative_host_policy_is_independent_of_process_environment() {
    for vars in [
        vec![],
        vec![("TERM", "xterm-256color")],
        vec![("WT_SESSION", "present")],
        vec![("TERM", "dumb"), ("TERM_PROGRAM", "kitty")],
        vec![("TERM", "screen-256color"), ("TERM_PROGRAM", "kitty")],
        vec![("TERM", "xterm-kitty"), ("TMUX", "present")],
        vec![("TERM", "foot"), ("STY", "present")],
    ] {
        assert_eq!(detect(false, &vars), Capabilities::default());
        assert_eq!(detect(true, &vars), Capabilities::default());
    }
    assert_eq!(
        Capabilities::detect_with(false, false, |_| Some("kitty".into())),
        Capabilities::default()
    );
    for term in ["xterm-kitty", "foot", "foot-extra"] {
        let caps = detect(false, &[("TERM", term)]);
        assert!(caps.kitty_keyboard);
        assert!(caps.synchronized_output);
        assert!(!caps.kitty_graphics);
        assert_eq!(detect(true, &[("TERM", term)]), Capabilities::default());
    }
    for windows in [true, false] {
        let caps = detect(windows, &[("TERM_PROGRAM", "WezTerm")]);
        assert!(caps.synchronized_output);
        assert!(!caps.kitty_keyboard);
        assert!(!caps.kitty_graphics);
    }
}

fn render(capabilities: Capabilities, interactive: bool) -> std::io::Result<Vec<u8>> {
    let mut renderer = Renderer::with_capabilities(
        Vec::new(),
        80,
        24,
        ColorMode::None,
        interactive,
        capabilities,
    );
    let frame = Frame {
        lines: vec!["intro frame".into()],
        cursor: None,
    };
    renderer.paint(&frame)?;
    renderer.paint_styled(&frame)?;
    renderer.finalize_styled(&["header".into()])?;
    renderer.finalize("answer")?;
    Ok(renderer.into_inner())
}

#[test]
fn sync_is_opt_in_without_changing_frame_contents() -> Result<(), Box<dyn std::error::Error>> {
    let plain = render(Capabilities::default(), true)?;
    let synchronized = render(
        Capabilities {
            synchronized_output: true,
            ..Capabilities::default()
        },
        true,
    )?;
    let synchronized = String::from_utf8(synchronized)?;
    // Painting the identical frame a second time is deliberately suppressed.
    assert_eq!(synchronized.matches("\x1b[?2026h").count(), 3);
    assert_eq!(synchronized.matches("\x1b[?2026l").count(), 3);
    assert_eq!(
        synchronized
            .replace("\x1b[?2026h", "")
            .replace("\x1b[?2026l", "")
            .as_bytes(),
        plain
    );
    assert!(!String::from_utf8(plain)?.contains("\x1b_G"));
    Ok(())
}

#[test]
fn noninteractive_never_emits_extensions_even_if_injected() -> std::io::Result<()> {
    assert_eq!(
        render(
            Capabilities {
                kitty_keyboard: true,
                kitty_graphics: true,
                synchronized_output: true,
            },
            false,
        )?,
        b"header\nanswer\n"
    );
    Ok(())
}

#[test]
fn keyboard_push_uses_crossterm_disambiguation_only() -> std::io::Result<()> {
    let mut bytes = Vec::new();
    Capabilities::default().enable_keyboard(&mut bytes)?;
    assert!(bytes.is_empty());
    Capabilities {
        kitty_keyboard: true,
        ..Capabilities::default()
    }
    .enable_keyboard(&mut bytes)?;
    #[cfg(not(windows))]
    assert_eq!(bytes, b"\x1b[>1u");
    #[cfg(windows)]
    assert!(bytes.is_empty());
    Ok(())
}

#[test]
fn feedback_is_static_and_only_emitted_for_documented_hosts() -> std::io::Result<()> {
    use gitcode::tui::capabilities::TerminalFeedback;
    let output = |interactive, vars: &[(&str, &str)]| -> std::io::Result<Vec<u8>> {
        let feedback = TerminalFeedback::detect_with(interactive, |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        });
        let mut bytes = Vec::new();
        feedback.set_title(&mut bytes)?;
        feedback.notify_completed_turn(&mut bytes)?;
        Ok(bytes)
    };
    assert_eq!(
        output(true, &[("TERM_PROGRAM", "WezTerm")])?,
        "\x1b]2;gitcode\x1b\\\x1b]9;gitcode: Ход завершён\x1b\\".as_bytes()
    );
    assert_eq!(
        output(true, &[("WT_SESSION", "known-session")])?,
        b"\x1b]2;gitcode\x1b\\"
    );
    for vars in [
        vec![],
        vec![("WT_SESSION", "")],
        vec![("TERM_PROGRAM", "unverified-host")],
        vec![("TERM_PROGRAM", "WezTerm"), ("TERM", "dumb")],
        vec![("TERM_PROGRAM", "WezTerm"), ("TERM", "screen")],
        vec![("TERM_PROGRAM", "WezTerm"), ("TMUX", "present")],
        vec![("TERM_PROGRAM", "WezTerm"), ("STY", "present")],
    ] {
        assert!(output(true, &vars)?.is_empty());
    }
    assert!(output(false, &[("TERM_PROGRAM", "WezTerm")])?.is_empty());
    Ok(())
}
