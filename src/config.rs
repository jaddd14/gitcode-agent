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
    /// Hard request timeout in seconds.
    pub timeout_seconds: u64,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            name: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            protocol: Protocol::Anthropic,
            api_key_env: "ANTHROPIC_API_KEY".into(),
            timeout_seconds: 120,
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
    /// Hard agent iteration limit.
    pub max_turns: u32,
    /// Automatic compaction threshold as a window fraction.
    pub compaction_threshold: f64,
    /// Enable intro animation in interactive terminals.
    pub intro: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "ru".into(),
            model: "claude-opus-5-5".into(),
            provider: Provider::default(),
            max_turns: 50,
            compaction_threshold: 0.9,
            intro: true,
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
            ("max_turns", self.max_turns > 0, "must be positive"),
            (
                "compaction_threshold",
                self.compaction_threshold > 0.0 && self.compaction_threshold < 1.0,
                "must be between zero and one",
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
    for path in [
        home.join(".gitcode/config.toml"),
        project.join(".gitcode/config.toml"),
        project.join(".gitcode/config.local.toml"),
    ] {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let layer: toml::Value = text
                    .parse()
                    .map_err(|_| invalid(&path, "root", "invalid TOML syntax"))?;
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
    for (name, key) in [("GITCODE_LANGUAGE", "language"), ("GITCODE_MODEL", "model")] {
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
            while let Some(start) = rest.find("${") {
                result.push_str(&rest[..start]);
                rest = &rest[start + 2..];
                let end = rest.find('}').ok_or_else(|| {
                    invalid(
                        Path::new("environment"),
                        "interpolation",
                        "unterminated variable",
                    )
                })?;
                result.push_str(env.get(&rest[..end]).ok_or_else(|| {
                    invalid(
                        Path::new("environment"),
                        "interpolation",
                        "required variable is unset",
                    )
                })?);
                rest = &rest[end + 1..];
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
    fn defaults_and_validation() {
        let mut c = Config::default();
        assert!(c.validate(Path::new("test")).is_ok());
        c.max_turns = 0;
        assert!(c.validate(Path::new("test")).is_err());
        c.max_turns = 1;
        c.compaction_threshold = f64::NAN;
        assert!(c.validate(Path::new("test")).is_err());
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
    }
}
