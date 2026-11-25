//! Strict layered configuration with explicit environment inputs.

use crate::error::{io, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Supported provider wire protocol.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// Anthropic Messages protocol.
    #[default]
    Anthropic,
    /// OpenAI Chat Completions protocol.
    OpenaiChat,
    /// OpenAI Responses protocol.
    OpenaiResponses,
}

/// Provider endpoint without inline authentication secrets.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Provider {
    /// Display name.
    pub name: String,
    /// HTTP API root.
    pub base_url: String,
    /// Wire format.
    pub protocol: Protocol,
    /// Environment variable containing the key.
    pub api_key_env: String,
    /// Enable prompt caching on supported official endpoints.
    pub prompt_cache: bool,
    /// Hard per-attempt timeout in seconds.
    pub timeout_seconds: u64,
    /// Maximum generated tokens per request; a low value truncates long answers.
    pub max_tokens: u32,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            name: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            protocol: Protocol::Anthropic,
            api_key_env: "ANTHROPIC_API_KEY".into(),
            prompt_cache: true,
            timeout_seconds: 120,
            max_tokens: 32_000,
        }
    }
}

/// Runtime options resolved from all supported layers.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Default user-facing language, ru or en.
    pub language: String,
    /// Selected model identifier.
    pub model: String,
    /// Provider endpoint configuration.
    pub provider: Provider,
    /// Saved provider profiles offered by `/provider`, newest last.
    pub providers: Vec<Provider>,
    /// Allow a project to override provider credentials and interpolate environment variables.
    /// This opt-in is accepted only in the user's configuration.
    pub trust_project_config: bool,
    /// Hard agent iteration limit; zero disables the limit.
    pub max_turns: u32,
    /// Automatic compaction threshold as a window fraction.
    pub compaction_threshold: f64,
    /// Enable automatic compaction once the threshold is crossed; `false` keeps the full context.
    pub auto_compact: bool,
    /// Approximate input context window in tokens for automatic compaction.
    pub context_window: u64,
    /// Conversation rows moved per mouse-wheel notch.
    pub mouse_scroll_speed: u32,
    /// Animate mouse wheel scrolling; disabled by default for immediate scrolling.
    pub scroll_animation: bool,
    /// Enable intro animation in interactive terminals.
    pub intro: bool,
    /// Persisted terminal palette and white-text preference.
    pub theme: crate::theme::ThemeChoice,
    /// Shell selection: auto, bash, pwsh, powershell, or cmd.
    pub shell: String,
    /// Explicitly configured lifecycle hooks; project-defined commands require trust.
    pub hooks: Vec<crate::extensibility::HookRule>,
    /// Names of explicitly enabled local hook plugins.
    pub plugins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "ru".into(),
            model: "claude-opus-5-5".into(),
            provider: Provider::default(),
            providers: Vec::new(),
            trust_project_config: false,
            max_turns: 0,
            compaction_threshold: 0.9,
            auto_compact: true,
            context_window: 200_000,
            mouse_scroll_speed: 3,
            scroll_animation: false,
            intro: true,
            theme: crate::theme::ThemeChoice::default(),
            shell: "auto".into(),
            hooks: Vec::new(),
            plugins: Vec::new(),
        }
    }
}

fn invalid(path: &Path, key: &str, reason: &str) -> Error {
    Error::Config {
        path: path.to_owned(),
        key: key.into(),
        reason: reason.into(),
    }
}

impl Config {
    /// Validate settings without copying rejected values into errors.
    pub fn validate(&self, source: &Path) -> Result<()> {
        for (key, valid, reason) in [
            (
                "language",
                matches!(self.language.as_str(), "ru" | "en"),
                "must be ru or en",
            ),
            ("model", !self.model.trim().is_empty(), "must not be empty"),
            (
                "shell",
                matches!(
                    self.shell.as_str(),
                    "auto" | "bash" | "pwsh" | "powershell" | "cmd"
                ),
                "must be auto, bash, pwsh, powershell, or cmd",
            ),
            (
                "context_window",
                self.context_window >= 4096,
                "must be at least 4096 tokens",
            ),
            (
                "compaction_threshold",
                self.compaction_threshold > 0.0 && self.compaction_threshold < 1.0,
                "must be between zero and one",
            ),
            (
                "mouse_scroll_speed",
                (1..=50).contains(&self.mouse_scroll_speed),
                "must be between 1 and 50 rows",
            ),
            (
                "provider.max_tokens",
                self.provider.max_tokens >= 1024,
                "must be at least 1024 tokens",
            ),
            (
                "provider.timeout_seconds",
                self.provider.timeout_seconds > 0 && self.provider.timeout_seconds <= 600,
                "must be between 1 and 600",
            ),
            (
                "provider.base_url",
                self.provider.base_url.starts_with("https://")
                    || self.provider.base_url.starts_with("http://"),
                "must be an HTTP(S) URL",
            ),
        ] {
            if !valid {
                return Err(invalid(source, key, reason));
            }
        }
        if self.plugins.len() > 32
            || self
                .plugins
                .iter()
                .any(|name| !crate::plugins::valid_name(name))
        {
            return Err(invalid(
                source,
                "plugins",
                "plugin names must be unique simple names (at most 32)",
            ));
        }
        let unique: std::collections::BTreeSet<_> = self.plugins.iter().collect();
        if unique.len() != self.plugins.len() {
            return Err(invalid(source, "plugins", "plugin names must be unique"));
        }
        if self.providers.len() > 32 {
            return Err(invalid(
                source,
                "providers",
                "at most 32 saved providers are allowed",
            ));
        }
        for provider in &self.providers {
            if provider.name.trim().is_empty() || provider.name.len() > 64 {
                return Err(invalid(
                    source,
                    "providers.name",
                    "must be nonempty and at most 64 bytes",
                ));
            }
            if !(provider.base_url.starts_with("https://")
                || provider.base_url.starts_with("http://"))
            {
                return Err(invalid(
                    source,
                    "providers.base_url",
                    "must be an HTTP(S) URL",
                ));
            }
            // Saved profiles hold the lookup name only, never a secret.
            if provider.api_key_env.contains('=')
                || provider
                    .api_key_env
                    .bytes()
                    .any(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_'))
            {
                return Err(invalid(
                    source,
                    "providers.api_key_env",
                    "must be an environment variable name",
                ));
            }
        }
        let provider_names: std::collections::BTreeSet<_> = self
            .providers
            .iter()
            .map(|provider| &provider.name)
            .collect();
        if provider_names.len() != self.providers.len() {
            return Err(invalid(
                source,
                "providers",
                "provider names must be unique",
            ));
        }
        if self.hooks.len() > 32 {
            return Err(invalid(source, "hooks", "at most 32 hooks are allowed"));
        }
        for hook in &self.hooks {
            if hook.command.trim().is_empty() || hook.command.len() > 16_384 {
                return Err(invalid(
                    source,
                    "hooks.command",
                    "must be nonempty and at most 16384 bytes",
                ));
            }
            if !(1..=60_000).contains(&hook.timeout_ms) {
                return Err(invalid(
                    source,
                    "hooks.timeout_ms",
                    "must be between 1 and 60000",
                ));
            }
            // SessionStart, SessionEnd and Notification need session lifecycle
            // plumbing that is not wired yet; accepting them would silently
            // never fire.
            if !matches!(
                hook.event,
                crate::extensibility::HookEvent::PreToolUse
                    | crate::extensibility::HookEvent::PostToolUse
                    | crate::extensibility::HookEvent::UserPromptSubmit
                    | crate::extensibility::HookEvent::Stop
                    | crate::extensibility::HookEvent::SubagentStop
                    | crate::extensibility::HookEvent::PreCompact
            ) {
                return Err(invalid(
                    source,
                    "hooks.event",
                    "SessionStart, SessionEnd and Notification are not dispatched yet",
                ));
            }
            if hook
                .matcher
                .as_ref()
                .is_some_and(|value| value.len() > 1024)
            {
                return Err(invalid(
                    source,
                    "hooks.matcher",
                    "must be at most 1024 bytes",
                ));
            }
        }
        Ok(())
    }
}

/// CLI overrides are TOML-compatible typed values, applied last.
pub type Overrides = BTreeMap<String, toml::Value>;

/// Load user, project, local, environment and CLI layers, in that order.
pub fn load(
    home: &Path,
    project: &Path,
    env: &BTreeMap<String, String>,
    cli: &Overrides,
) -> Result<Config> {
    let mut value = toml::Value::try_from(Config::default())
        .map_err(|_| invalid(Path::new("defaults"), "root", "cannot encode defaults"))?;
    let user_path = home.join(".gitcode/config.toml");
    for (path, project_layer) in [
        (user_path.clone(), false),
        (project.join(".gitcode/config.toml"), true),
        (project.join(".gitcode/config.local.toml"), true),
    ] {
        // When HOME is the workspace, this is the same trusted user file.
        if project_layer && path == user_path {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut layer: toml::Value = text
                    .parse()
                    .map_err(|_| invalid(&path, "root", "invalid TOML syntax"))?;
                let trusted = value
                    .get("trust_project_config")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                if project_layer {
                    if layer.get("trust_project_config").is_some() {
                        return Err(invalid(
                            &path,
                            "trust_project_config",
                            "only the user configuration can grant project trust",
                        ));
                    }
                    if !trusted {
                        if layer.get("hooks").is_some() {
                            return Err(invalid(
                                &path,
                                "hooks",
                                "project hooks require explicit user trust",
                            ));
                        }
                        if layer.get("plugins").is_some() {
                            return Err(invalid(
                                &path,
                                "plugins",
                                "project plugins require explicit user trust",
                            ));
                        }
                        if has_environment_expansion(&layer) {
                            return Err(invalid(
                                &path,
                                "root",
                                "environment interpolation requires trusted project configuration",
                            ));
                        }
                        if let Some(provider) =
                            layer.get("provider").and_then(toml::Value::as_table)
                        {
                            for key in ["base_url", "api_key_env"] {
                                if provider.get(key).is_some_and(|candidate| {
                                    value.get("provider").and_then(|v| v.get(key))
                                        != Some(candidate)
                                }) {
                                    return Err(invalid(&path, "provider", "project cannot change the provider endpoint or credential without explicit user trust"));
                                }
                            }
                        }
                    }
                } else {
                    // Resolve trusted user settings before comparing project overrides.
                    interpolate(&mut layer, env)?;
                }
                merge(&mut value, layer);
                let _: Config = value
                    .clone()
                    .try_into()
                    .map_err(|_| invalid(&path, "root", "unknown key or invalid value type"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io("read configuration", path, e)),
        }
    }
    for (name, key) in [
        ("GITCODE_LANGUAGE", "language"),
        ("GITCODE_MODEL", "model"),
        ("GITCODE_SHELL", "shell"),
    ] {
        if let Some(v) = env.get(name) {
            set(&mut value, key, toml::Value::String(v.clone()))?;
        }
    }
    if let Some(v) = env.get("GITCODE_MAX_TURNS") {
        let n = v
            .parse::<i64>()
            .map_err(|_| invalid(Path::new("environment"), "max_turns", "must be an integer"))?;
        set(&mut value, "max_turns", toml::Value::Integer(n))?;
    }
    for (key, v) in cli {
        set(&mut value, key, v.clone())?;
    }
    interpolate(&mut value, env)?;
    let config: Config = value.try_into().map_err(|_| {
        invalid(
            Path::new("resolved configuration"),
            "root",
            "unknown key or invalid value type",
        )
    })?;
    config.validate(Path::new("resolved configuration"))?;
    Ok(config)
}

fn has_environment_expansion(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(text) => text.contains("${"),
        toml::Value::Array(items) => items.iter().any(has_environment_expansion),
        toml::Value::Table(items) => items.values().any(has_environment_expansion),
        _ => false,
    }
}

fn merge(target: &mut toml::Value, source: toml::Value) {
    match (target, source) {
        (toml::Value::Table(target), toml::Value::Table(source)) => {
            for (k, v) in source {
                match target.get_mut(&k) {
                    Some(old) => merge(old, v),
                    None => {
                        target.insert(k, v);
                    }
                }
            }
        }
        (target, source) => *target = source,
    }
}

fn set(value: &mut toml::Value, key: &str, replacement: toml::Value) -> Result<()> {
    let table = value
        .as_table_mut()
        .ok_or_else(|| invalid(Path::new("overrides"), key, "expected a table"))?;
    table.insert(key.into(), replacement);
    Ok(())
}

fn interpolate(value: &mut toml::Value, env: &BTreeMap<String, String>) -> Result<()> {
    match value {
        toml::Value::String(text) => {
            let mut result = String::new();
            let mut rest = text.as_str();
            while let Some(start) = rest.find(['$', '%']) {
                result.push_str(&rest[..start]);
                rest = &rest[start..];
                let (name, consumed) = if let Some(variable) = rest.strip_prefix("${") {
                    let end = variable.find('}').ok_or_else(|| {
                        invalid(
                            Path::new("environment"),
                            "interpolation",
                            "unterminated variable",
                        )
                    })?;
                    (&variable[..end], end + 3)
                } else if let Some(suffix) = rest.strip_prefix("$HOME").filter(|suffix| {
                    !suffix.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                }) {
                    ("HOME", rest.len() - suffix.len())
                } else if rest.starts_with("%USERPROFILE%") {
                    ("USERPROFILE", "%USERPROFILE%".len())
                } else {
                    result.push_str(&rest[..1]);
                    rest = &rest[1..];
                    continue;
                };
                result.push_str(env.get(name).ok_or_else(|| {
                    invalid(
                        Path::new("environment"),
                        "interpolation",
                        "required variable is unset",
                    )
                })?);
                rest = &rest[consumed..];
            }
            result.push_str(rest);
            *text = result;
        }
        toml::Value::Table(table) => {
            for (_, v) in table.iter_mut() {
                interpolate(v, env)?;
            }
        }
        toml::Value::Array(array) => {
            for v in array {
                interpolate(v, env)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Save only an explicitly selected theme, preserving all other user settings.
pub fn save_theme(home: &Path, choice: crate::theme::ThemeChoice) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = home.join(".gitcode");
    let path = directory.join("config.toml");
    let mut value = match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse::<toml::Value>()
            .map_err(|_| invalid(&path, "root", "invalid TOML syntax"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(Default::default())
        }
        Err(error) => return Err(io("read configuration", &path, error)),
    };
    let encoded = toml::Value::try_from(choice)
        .map_err(|_| invalid(&path, "theme", "cannot encode theme"))?;
    value
        .as_table_mut()
        .ok_or_else(|| invalid(&path, "root", "expected a table"))?
        .insert("theme".into(), encoded);
    let text = toml::to_string_pretty(&value)
        .map_err(|_| invalid(&path, "root", "cannot encode configuration"))?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| io("create configuration directory", &directory, error))?;
    let temporary = directory.join(format!(
        ".config.toml.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        if let Ok(metadata) = std::fs::metadata(&path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        #[cfg(unix)]
        std::fs::File::open(&directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| io("save theme configuration", path, error))
}

/// Persist one provider profile and make it the active `[provider]`, keeping
/// every unrelated user setting untouched.
///
/// Only the profile shape is stored; the token itself stays in the private
/// credential store under `~/.gitcode/credentials/<api_key_env>`.
pub fn save_provider(home: &Path, mut profile: Provider) -> Result<()> {
    let directory = home.join(".gitcode");
    let path = directory.join("config.toml");
    let mut value = match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse::<toml::Value>()
            .map_err(|_| invalid(&path, "root", "invalid TOML syntax"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(Default::default())
        }
        Err(error) => return Err(io("read configuration", &path, error)),
    };
    let table = value
        .as_table_mut()
        .ok_or_else(|| invalid(&path, "root", "expected a table"))?;
    // Read existing profiles before `[provider]` is replaced.
    let mut profiles: Vec<Provider> = table
        .get("providers")
        .map(|value| value.clone().try_into())
        .transpose()
        .map_err(|_| invalid(&path, "providers", "cannot read saved providers"))?
        .unwrap_or_default();
    let encoded_profile = toml::Value::try_from(profile.clone())
        .map_err(|_| invalid(&path, "provider", "cannot encode provider"))?;
    profiles.retain(|saved| saved.name != profile.name);
    profiles.push(std::mem::take(&mut profile));
    let encoded_profiles = toml::Value::try_from(profiles)
        .map_err(|_| invalid(&path, "providers", "cannot encode providers"))?;
    table.insert("provider".into(), encoded_profile);
    table.insert("providers".into(), encoded_profiles);
    let text = toml::to_string_pretty(&value)
        .map_err(|_| invalid(&path, "root", "cannot encode configuration"))?;
    write_private(&directory, &path, &text).map_err(|error| io("save provider", &path, error))
}

/// Atomically replace a private configuration file owned by the operator.
fn write_private(directory: &Path, path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(
        ".config.toml.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Standard configuration file paths for diagnostics.
pub fn sources(home: &Path, project: &Path) -> [PathBuf; 3] {
    [
        home.join(".gitcode/config.toml"),
        project.join(".gitcode/config.toml"),
        project.join(".gitcode/config.local.toml"),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn explicit_theme_save_preserves_user_configuration() {
        let root = std::env::temp_dir().join(format!("gitcode-theme-save-{}", std::process::id()));
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let empty = Overrides::new();
        let config = load(&home, &project, &BTreeMap::new(), &empty).unwrap();
        assert_eq!(config.theme, crate::theme::ThemeChoice::default());
        assert!(!home.join(".gitcode").exists());
        std::fs::create_dir_all(home.join(".gitcode")).unwrap();
        let path = home.join(".gitcode/config.toml");
        std::fs::write(&path, "model = 'custom-model'\nintro = false\n[provider]\nname = 'custom'\nbase_url = '${ENDPOINT}'\n").unwrap();
        let choice = crate::theme::ThemeChoice {
            preset: crate::theme::RosePreset::WineRose,
            white: crate::theme::WhiteMode::Pure,
        };
        save_theme(&home, choice).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: toml::Value = contents.parse().unwrap();
        assert_eq!(value["model"].as_str(), Some("custom-model"));
        assert_eq!(value["intro"].as_bool(), Some(false));
        assert_eq!(value["provider"]["name"].as_str(), Some("custom"));
        assert_eq!(value["provider"]["base_url"].as_str(), Some("${ENDPOINT}"));
        let env = BTreeMap::from([("ENDPOINT".into(), "https://example.com".into())]);
        assert_eq!(load(&home, &project, &env, &empty).unwrap().theme, choice);
        assert_eq!(std::fs::read_dir(home.join(".gitcode")).unwrap().count(), 1);
        std::fs::write(&path, "broken = [").unwrap();
        assert!(save_theme(&home, choice).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken = [");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn theme_persistence_preserves_user_settings_and_does_not_write_on_load() {
        use crate::theme::{RosePreset, ThemeChoice, WhiteMode};
        let home = std::env::temp_dir().join(format!("gitcode-theme-{}", std::process::id()));
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let path = home.join(".gitcode/config.toml");
        assert!(!path.exists());
        let loaded = load(&home, &project, &BTreeMap::new(), &Overrides::new()).unwrap();
        assert_eq!(loaded.theme, ThemeChoice::default());
        assert!(!path.exists());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "model = 'custom-model'\nintro = false\n[provider]\nname = 'custom'\nbase_url = '${BASE}/v1'\n").unwrap();
        let choice = ThemeChoice {
            preset: RosePreset::WineRose,
            white: WhiteMode::Pure,
        };
        save_theme(&home, choice).unwrap();
        let stored: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(stored["model"].as_str(), Some("custom-model"));
        assert_eq!(stored["intro"].as_bool(), Some(false));
        assert_eq!(stored["provider"]["base_url"].as_str(), Some("${BASE}/v1"));
        let loaded = load(
            &home,
            &project,
            &BTreeMap::from([("BASE".into(), "https://example.com".into())]),
            &Overrides::new(),
        )
        .unwrap();
        assert_eq!(loaded.theme, choice);
        assert_eq!(loaded.provider.name, "custom");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::write(&path, "invalid = [").unwrap();
        assert!(save_theme(&home, ThemeChoice::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid = [");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn defaults_and_validation() {
        let mut c = Config::default();
        assert!(c.validate(Path::new("test")).is_ok());
        assert_eq!(c.max_turns, 0);
        assert!(c.auto_compact);
        assert_eq!(c.mouse_scroll_speed, 3);
        c.mouse_scroll_speed = 0;
        assert!(c.validate(Path::new("test")).is_err());
        c.mouse_scroll_speed = 3;
        c.max_turns = 1;
        c.compaction_threshold = f64::NAN;
        assert!(c.validate(Path::new("test")).is_err());
    }

    #[test]
    fn save_provider_updates_active_and_preserves_settings() {
        let home =
            std::env::temp_dir().join(format!("gitcode-provider-save-{}", std::process::id()));
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let config_dir = home.join(".gitcode");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "model = 'custom-model'\nauto_compact = false\nmouse_scroll_speed = 6\n",
        )
        .unwrap();
        let profile = Provider {
            name: "api.openai.com".into(),
            base_url: "https://api.openai.com/v1".into(),
            protocol: Protocol::OpenaiChat,
            api_key_env: "API_OPENAI_COM_API_KEY".into(),
            prompt_cache: false,
            ..Provider::default()
        };
        save_provider(&home, profile.clone()).unwrap();
        let loaded = load(&home, &project, &BTreeMap::new(), &Overrides::new()).unwrap();
        assert_eq!(loaded.model, "custom-model");
        assert!(!loaded.auto_compact);
        assert_eq!(loaded.mouse_scroll_speed, 6);
        assert_eq!(loaded.provider.name, "api.openai.com");
        assert_eq!(loaded.providers.len(), 1);
        assert_eq!(loaded.providers[0].base_url, "https://api.openai.com/v1");
        // Saving again replaces the same profile instead of duplicating it.
        save_provider(&home, profile).unwrap();
        let reloaded = load(&home, &project, &BTreeMap::new(), &Overrides::new()).unwrap();
        assert_eq!(reloaded.providers.len(), 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn interpolation_is_strict() {
        let mut v = toml::Value::String("${BASE}/v1".into());
        assert!(interpolate(&mut v, &BTreeMap::new()).is_err());
        interpolate(
            &mut v,
            &BTreeMap::from([("BASE".into(), "https://example.com".into())]),
        )
        .unwrap();
        assert_eq!(v.as_str(), Some("https://example.com/v1"));
    }

    #[test]
    fn interpolation_supports_home_and_userprofile_in_nested_strings() {
        let env = BTreeMap::from([
            ("HOME".into(), "/home/example".into()),
            ("USERPROFILE".into(), r"C:\Users\Example User".into()),
            ("BASE".into(), "https://example.com".into()),
        ]);
        let mut value: toml::Value = r#"
            path = '$HOME/.gitcode'
            nested = ['%USERPROFILE%\.gitcode', '${BASE}/$HOME/%USERPROFILE%']
            count = 7
        "#
        .parse()
        .unwrap();
        interpolate(&mut value, &env).unwrap();
        assert_eq!(value["path"].as_str(), Some("/home/example/.gitcode"));
        assert_eq!(
            value["nested"][0].as_str(),
            Some(r"C:\Users\Example User\.gitcode")
        );
        assert_eq!(
            value["nested"][1].as_str(),
            Some(r"https://example.com//home/example/C:\Users\Example User")
        );
        assert_eq!(value["count"].as_integer(), Some(7));
    }

    #[test]
    fn interpolation_preserves_longer_names_and_other_shell_syntax() {
        let input = "$HOMEPATH $HOME_DIR $HOME1 $HOMEpath $OTHER %USERPROFILE_DIR% %OTHER% %USERPROFILE $(echo test) `echo test` — $HOME/данные";
        let mut value = toml::Value::String(input.into());
        interpolate(
            &mut value,
            &BTreeMap::from([("HOME".into(), "/explicit".into())]),
        )
        .unwrap();
        assert_eq!(
            value.as_str(),
            Some("$HOMEPATH $HOME_DIR $HOME1 $HOMEpath $OTHER %USERPROFILE_DIR% %OTHER% %USERPROFILE $(echo test) `echo test` — /explicit/данные")
        );
    }

    #[test]
    fn interpolation_does_not_expand_inserted_values() {
        let env = BTreeMap::from([
            ("HOME".into(), "${MISSING} %USERPROFILE% $HOME".into()),
            ("USERPROFILE".into(), "$HOME ${UNTERMINATED".into()),
            ("BASE".into(), "%USERPROFILE% $HOME ${MISSING}".into()),
        ]);
        let mut value = toml::Value::String("$HOME|%USERPROFILE%|${BASE}".into());
        interpolate(&mut value, &env).unwrap();
        assert_eq!(
            value.as_str(),
            Some("${MISSING} %USERPROFILE% $HOME|$HOME ${UNTERMINATED|%USERPROFILE% $HOME ${MISSING}")
        );
    }

    #[test]
    fn interpolation_requires_explicit_variables_and_preserves_errors() {
        for (input, expected_reason) in [
            ("$HOME", "required variable is unset"),
            ("%USERPROFILE%", "required variable is unset"),
            ("${HOME}", "required variable is unset"),
            ("${}", "required variable is unset"),
            ("${HOME", "unterminated variable"),
            ("prefix $OTHER ${", "unterminated variable"),
        ] {
            let mut value = toml::Value::String(input.into());
            match interpolate(&mut value, &BTreeMap::new()).unwrap_err() {
                Error::Config { path, key, reason } => {
                    assert_eq!(path, Path::new("environment"));
                    assert_eq!(key, "interpolation");
                    assert_eq!(reason, expected_reason);
                }
                other => panic!("unexpected error: {other:?}"),
            }
            assert_eq!(value.as_str(), Some(input));
        }
        let mut value = toml::Value::String("$HOME%USERPROFILE%${EMPTY}".into());
        interpolate(
            &mut value,
            &BTreeMap::from([
                ("HOME".into(), String::new()),
                ("USERPROFILE".into(), String::new()),
                ("EMPTY".into(), String::new()),
            ]),
        )
        .unwrap();
        assert_eq!(value.as_str(), Some(""));
    }

    #[test]
    fn nested_merge_preserves_defaults() {
        let mut v = toml::Value::try_from(Config::default()).unwrap();
        merge(
            &mut v,
            "[provider]\nname = 'custom'"
                .parse::<toml::Value>()
                .unwrap(),
        );
        let c: Config = v.try_into().unwrap();
        assert_eq!(c.provider.name, "custom");
        assert_eq!(c.provider.timeout_seconds, 120);
        assert_eq!(c.provider.max_tokens, 32_000);
    }
}
