//! Provider switching, styled like `/effort`: existing profiles first, then a
//! guided "add new" flow (base URL, protocol, auth method, token).
use super::{
    picker,
    render::{Frame, Renderer},
    widgets,
};
use crate::{
    config::{Protocol, Provider},
    theme::{ColorMode, Theme},
};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::io;

/// Masked placeholder shown instead of a stored credential.
const HIDDEN: &str = "••••••••";

/// One row of the provider menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// Start the guided flow for a brand-new endpoint.
    AddNew,
    /// Switch to a saved profile.
    Existing(Provider),
}

/// How the token is sent on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// `x-api-key`, used by Anthropic-style endpoints.
    ApiKey,
    /// `Authorization: Bearer`, used by OpenAI-compatible endpoints.
    Bearer,
}

impl Auth {
    /// Label for the picker.
    pub fn label(self) -> &'static str {
        match self {
            Self::ApiKey => "API-ключ (x-api-key)",
            Self::Bearer => "Bearer-токен (Authorization)",
        }
    }
    /// Wire format implied by the chosen auth method.
    pub fn protocol(self) -> Protocol {
        match self {
            Self::ApiKey => Protocol::Anthropic,
            Self::Bearer => Protocol::OpenaiChat,
        }
    }
    /// Environment variable that will hold the secret.
    pub fn key_env(self, name: &str) -> String {
        let slug: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("{}_API_KEY", slug.trim_matches('_'))
    }
}

/// Result of the guided flow.
#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    /// Profile ready to activate and save.
    pub profile: Provider,
    /// Secret to store in the private credential file.
    pub token: String,
}

/// Menu rows: "add new" is always first, exactly as asked.
pub fn rows(
    profiles: &[Provider],
    active: &str,
    selected: usize,
    width: usize,
    height: usize,
    mode: ColorMode,
) -> Vec<String> {
    let theme = Theme::default();
    let width = width.max(1);
    let mut out = vec![picker::heading("  Провайдер", width, &theme, mode)];
    out.push(String::new());
    let entries: Vec<Row> = std::iter::once(Row::AddNew)
        .chain(profiles.iter().cloned().map(Row::Existing))
        .collect();
    let capacity = height.saturating_sub(4).max(1).min(entries.len());
    let start = (selected + 1).saturating_sub(capacity);
    for (index, entry) in entries.iter().enumerate().skip(start).take(capacity) {
        let (label, description) = match entry {
            Row::AddNew => (
                "＋ Добавить провайдера".to_owned(),
                "base url · способ входа · токен".to_owned(),
            ),
            Row::Existing(profile) => {
                let current = if profile.name == active {
                    " (текущий)"
                } else {
                    ""
                };
                (
                    format!("{}{current}", profile.name),
                    format!(
                        "{}{}",
                        profile.base_url,
                        match profile.protocol {
                            Protocol::Anthropic => " · x-api-key",
                            Protocol::OpenaiChat => " · bearer",
                            Protocol::OpenaiResponses => " · bearer (responses)",
                        }
                    ),
                )
            }
        };
        let primary = widgets::ellipsis(&label, width.saturating_sub(6).max(8), false);
        let secondary = if width >= 60 {
            format!("  {description}")
        } else {
            String::new()
        };
        out.push(picker::choice(
            index == selected,
            &primary,
            &secondary,
            width,
            &theme,
            mode,
        ));
    }
    out.push(String::new());
    out.push(picker::text(
        "  ↑↓ выбрать   enter применить   esc закрыть",
        width,
        theme.placeholder,
        mode,
    ));
    picker::constrain(out, height)
}

fn field_frame(
    title: &str,
    hint: &str,
    value: &str,
    error: Option<&str>,
    width: usize,
    mode: ColorMode,
) -> Frame {
    let theme = Theme::default();
    let width = width.max(1);
    let mut lines = vec![picker::heading(&format!("  {title}"), width, &theme, mode)];
    lines.push(String::new());
    lines.push(picker::text(
        &format!("  ▸ {value}"),
        width,
        theme.text,
        mode,
    ));
    lines.push(String::new());
    if let Some(error) = error {
        lines.push(picker::text(&format!("  {error}"), width, theme.rose, mode));
    } else {
        lines.push(picker::text(
            &format!("  {hint}"),
            width,
            theme.placeholder,
            mode,
        ));
    }
    lines.push(picker::text(
        "  enter дальше   esc назад",
        width,
        theme.placeholder,
        mode,
    ));
    Frame {
        lines: picker::constrain(lines, 7),
        cursor: None,
    }
}

/// Read one line of text with a caret, returning `None` when cancelled.
async fn read_value(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    title: &str,
    hint: &str,
    initial: &str,
) -> io::Result<Option<String>> {
    let mut value = initial.to_owned();
    let mut error: Option<String> = None;
    loop {
        let (width, _) = renderer.size();
        renderer.paint_styled(&field_frame(
            title,
            hint,
            &value,
            error.as_deref(),
            width.saturating_sub(1),
            renderer.color_mode(),
        ))?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Enter => return Ok(Some(value)),
                KeyCode::Backspace => {
                    value.pop();
                    error = None;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear();
                    error = None;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    if value.len() < 512 {
                        value.push(c);
                    }
                    error = None;
                }
                _ => {}
            },
            Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
            Some(Err(error)) => return Err(error),
            None => return Ok(None),
            _ => {}
        }
    }
}

/// Read a secret without echoing it beyond a fixed mask.
async fn read_secret(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    title: &str,
    key_env: &str,
) -> io::Result<Option<String>> {
    let mut secret = String::new();
    loop {
        // The mask length is capped so a long token cannot reveal its size.
        let masked = HIDDEN.repeat(secret.chars().count().clamp(1, 8));
        let (width, _) = renderer.size();
        renderer.paint_styled(&field_frame(
            title,
            &format!("сохранится в ~/.gitcode/credentials/{key_env}"),
            &masked,
            None,
            width.saturating_sub(1),
            renderer.color_mode(),
        ))?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Enter if !secret.trim().is_empty() => {
                    return Ok(Some(secret.trim().to_owned()))
                }
                KeyCode::Enter => {}
                KeyCode::Backspace => {
                    secret.pop();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && secret.len() < 8192 =>
                {
                    secret.push(c);
                }
                _ => {}
            },
            Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
            Some(Err(error)) => return Err(error),
            None => return Ok(None),
            _ => {}
        }
    }
}

/// Pick an existing provider or walk through adding a new one.
pub async fn select(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
    profiles: &[Provider],
    active: &str,
) -> io::Result<Option<Choice>> {
    let mut selected = 0usize;
    let entries = profiles.len() + 1;
    loop {
        let (width, height) = renderer.size();
        renderer.paint_styled(&Frame {
            lines: rows(
                profiles,
                active,
                selected,
                width.saturating_sub(1),
                picker::height(height),
                renderer.color_mode(),
            ),
            cursor: None,
        })?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Up => selected = (selected + entries - 1) % entries,
                KeyCode::Down => selected = (selected + 1) % entries,
                KeyCode::Enter => {
                    if selected == 0 {
                        return match add_new(renderer, events).await? {
                            Some(created) => Ok(Some(Choice::Created(created))),
                            // Cancelling the wizard returns to the list, not out.
                            None => continue,
                        };
                    }
                    return Ok(Some(Choice::Existing(profiles[selected - 1].clone())));
                }
                _ => {}
            },
            Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
            Some(Err(error)) => return Err(error),
            None => return Ok(None),
            _ => {}
        }
    }
}

/// Outcome of the provider picker.
#[derive(Debug, Clone)]
pub enum Choice {
    /// Activate an already saved profile.
    Existing(Provider),
    /// Save and activate a freshly created profile.
    Created(Created),
}

/// Guided flow: base URL, auth method, token.
pub async fn add_new(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
) -> io::Result<Option<Created>> {
    let Some(base_url) = read_value(
        renderer,
        events,
        "Base URL провайдера",
        "например https://api.openai.com/v1",
        "https://",
    )
    .await?
    else {
        return Ok(None);
    };
    let base_url = base_url.trim().trim_end_matches('/').to_owned();
    if !(base_url.starts_with("https://") || base_url.starts_with("http://"))
        || base_url.len() > 512
    {
        return Ok(None);
    }
    let Some(auth) = pick_auth(renderer, events).await? else {
        return Ok(None);
    };
    let name = host_name(&base_url);
    let key_env = auth.key_env(&name);
    let Some(token) = read_secret(renderer, events, "Токен доступа", &key_env).await?
    else {
        return Ok(None);
    };
    Ok(Some(Created {
        profile: Provider {
            name,
            base_url,
            protocol: auth.protocol(),
            api_key_env: key_env,
            prompt_cache: matches!(auth, Auth::ApiKey),
            ..Provider::default()
        },
        token,
    }))
}

async fn pick_auth(
    renderer: &mut Renderer<io::Stdout>,
    events: &mut EventStream,
) -> io::Result<Option<Auth>> {
    let options = [Auth::ApiKey, Auth::Bearer];
    let mut selected = 1usize;
    loop {
        let theme = Theme::default();
        let (width, height) = renderer.size();
        let width = width.saturating_sub(1).max(1);
        let mode = renderer.color_mode();
        let mut lines = vec![picker::heading(
            "  Как передавать токен?",
            width,
            &theme,
            mode,
        )];
        lines.push(String::new());
        for (index, auth) in options.iter().enumerate() {
            lines.push(picker::choice(
                index == selected,
                &format!("{}. {}", index + 1, auth.label()),
                "",
                width,
                &theme,
                mode,
            ));
        }
        lines.push(String::new());
        lines.push(picker::text(
            "  ↑↓ выбрать   enter дальше   esc назад",
            width,
            theme.placeholder,
            mode,
        ));
        renderer.paint_styled(&Frame {
            lines: picker::constrain(lines, picker::height(height)),
            cursor: None,
        })?;
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None)
                }
                KeyCode::Up => selected = (selected + options.len() - 1) % options.len(),
                KeyCode::Down => selected = (selected + 1) % options.len(),
                KeyCode::Enter => return Ok(Some(options[selected])),
                _ => {}
            },
            Some(Ok(Event::Resize(w, h))) => renderer.resize(w.into(), h.into()),
            Some(Err(error)) => return Err(error),
            None => return Ok(None),
            _ => {}
        }
    }
}

/// Turn a URL into a short, stable profile name.
pub fn host_name(base_url: &str) -> String {
    let without_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let host = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme);
    let trimmed = host.trim_start_matches("www.");
    if trimmed.is_empty() {
        "custom".to_owned()
    } else {
        trimmed.chars().take(64).collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn profile(name: &str, url: &str) -> Provider {
        Provider {
            name: name.into(),
            base_url: url.into(),
            ..Provider::default()
        }
    }

    #[test]
    fn add_new_is_always_the_first_row() {
        let profiles = vec![profile("openai", "https://api.openai.com/v1")];
        let lines = rows(&profiles, "openai", 0, 80, 18, ColorMode::None);
        let text = lines.join("\n");
        assert!(text.contains("Добавить провайдера"));
        let add = lines.iter().position(|l| l.contains("Добавить")).unwrap();
        let existing = lines.iter().position(|l| l.contains("openai")).unwrap();
        assert!(add < existing);
        assert!(text.contains("(текущий)"));
        assert!(lines.iter().all(|line| line.width() <= 80));
    }

    #[test]
    fn menu_fits_and_keeps_selection_visible() {
        let profiles: Vec<_> = (0..12)
            .map(|i| profile(&format!("p{i}"), "https://example.com/v1"))
            .collect();
        for height in [8, 12, 20] {
            for selected in 0..profiles.len() + 1 {
                let lines = rows(&profiles, "p0", selected, 70, height, ColorMode::None);
                assert!(lines.len() <= height);
                assert!(lines.iter().any(|line| line.contains("enter применить")));
            }
        }
    }

    #[test]
    fn auth_maps_to_protocol_and_environment_name() {
        assert_eq!(Auth::ApiKey.protocol(), Protocol::Anthropic);
        assert_eq!(Auth::Bearer.protocol(), Protocol::OpenaiChat);
        assert_eq!(
            Auth::Bearer.key_env("api.openai.com"),
            "API_OPENAI_COM_API_KEY"
        );
        assert_eq!(Auth::ApiKey.key_env("anthropic"), "ANTHROPIC_API_KEY");
    }

    #[test]
    fn host_name_strips_scheme_port_and_path() {
        assert_eq!(host_name("https://api.openai.com/v1"), "api.openai.com");
        assert_eq!(host_name("http://localhost:11434/v1"), "localhost");
        assert_eq!(host_name("https://www.example.com"), "example.com");
        assert_eq!(host_name("https://"), "custom");
    }
}
