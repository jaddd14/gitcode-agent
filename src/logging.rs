//! Private append-only diagnostic logs, never written to terminal streams.

use crate::{
    credentials::{redact, Secret},
    error::{io, Result},
};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// File-only logger that redacts credentials before encoding structured entries.
pub struct Logger {
    file: File,
    path: PathBuf,
    secrets: Vec<Secret>,
}

#[derive(Serialize)]
struct Entry<'a> {
    timestamp_ms: u128,
    level: &'a str,
    message: String,
}

impl Logger {
    /// Open a private append-only log; callers supply a private parent directory.
    pub fn open(path: &Path, secrets: Vec<Secret>) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(|e| io("open log", path, e))?;
        Ok(Self {
            file,
            path: path.to_owned(),
            secrets,
        })
    }

    /// Append a redacted JSON line without printing on stdout or stderr.
    pub fn record(&mut self, level: &str, message: &str) -> Result<()> {
        let entry = Entry {
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            level,
            message: redact(message, &self.secrets),
        };
        let mut bytes = serde_json::to_vec(&entry)
            .map_err(|e| io("encode log", &self.path, std::io::Error::other(e)))?;
        bytes.push(b'\n');
        self.file
            .write_all(&bytes)
            .map_err(|e| io("append log", &self.path, e))
    }
}
