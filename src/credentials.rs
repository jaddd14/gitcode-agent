//! Private credential storage with deliberately redacted formatting.

use crate::error::{io, Result};
use std::{collections::BTreeMap, fmt, fs::OpenOptions, io::Write, path::Path};

/// Resolve an environment credential first, then its private file under the user home.
/// Missing files mean unauthenticated access; unsafe or unreadable files fail closed.
pub fn resolve(
    home: &Path,
    key_env: &str,
    env: &BTreeMap<String, String>,
) -> Result<Option<Secret>> {
    if let Some(value) = env.get(key_env).filter(|value| !value.is_empty()) {
        return Ok(Some(Secret::new(value.clone())));
    }
    // Environment names are also filenames; never let configuration escape the store.
    if key_env.is_empty()
        || !key_env
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(io(
            "resolve credential",
            home.join(".gitcode/credentials"),
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid credential identifier",
            ),
        ));
    }
    let directory = home.join(".gitcode/credentials");
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io("inspect credential store", directory, error)),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(io(
                "inspect credential store",
                directory,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "requires a regular directory",
                ),
            ));
        }
    }
    let path = directory.join(key_env);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io("inspect credential", path, error)),
        Ok(_) => read(&path).map(|secret| (!secret.expose().is_empty()).then_some(secret)),
    }
}

/// Sensitive bytes that never appear through Debug or Display.
pub struct Secret(String);

impl Secret {
    /// Wrap a credential obtained from a trusted source.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Expose only at the authentication boundary.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Persist a new credential without overwriting an existing path or symlink.
pub fn create(path: &Path, secret: &Secret) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| io("create credential", path, e))?;
    #[cfg(windows)]
    {
        // Best effort only: a failed ACL operation is not reported as secure storage.
        if !crate::platform::restrict_credentials(path) {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(io(
                "restrict credential ACL",
                path,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "cannot restrict credential ACL",
                ),
            ));
        }
    }
    file.write_all(secret.expose().as_bytes())
        .map_err(|e| io("write credential", path, e))?;
    file.sync_all().map_err(|e| io("sync credential", path, e))
}

/// Read a private, regular credential file, rejecting permissive Unix modes.
pub fn read(path: &Path) -> Result<Secret> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| io("inspect credential", path, e))?;
    let mut allowed = metadata.is_file() && !metadata.file_type().is_symlink();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        allowed &= metadata.permissions().mode() & 0o077 == 0;
    }
    if !allowed {
        return Err(io(
            "read private credential",
            path,
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "requires a private regular file",
            ),
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| io("read credential", path, e))?;
    Ok(Secret::new(text.trim_end().to_owned()))
}

/// Remove known credentials from output before it reaches persistent storage.
pub fn redact(text: &str, secrets: &[Secret]) -> String {
    let mut output = text.to_owned();
    let mut values: Vec<_> = secrets
        .iter()
        .map(Secret::expose)
        .filter(|s| !s.is_empty())
        .collect();
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for value in values {
        output = output.replace(value, "[REDACTED]");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formatting_and_redaction() {
        let secret = Secret::new("example-sensitive-value".into());
        assert_eq!(format!("{secret:?} {secret}"), "[REDACTED] [REDACTED]");
        assert_eq!(
            redact("key=example-sensitive-value", &[secret]),
            "key=[REDACTED]"
        );
    }
}
