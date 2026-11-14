//! Persist the model selected for each provider endpoint.
use crate::config::Provider;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io, path::Path};

fn key(provider: &Provider) -> String {
    let mut hash = Sha256::new();
    for part in [
        &provider.name,
        &provider.base_url,
        &format!("{:?}", provider.protocol),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn read(home: &Path) -> io::Result<BTreeMap<String, String>> {
    match std::fs::read(home.join(".gitcode/models.json")) {
        Ok(data) => serde_json::from_slice(&data).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error),
    }
}

/// Load the model explicitly selected for this provider, if any.
pub fn get(home: &Path, provider: &Provider) -> io::Result<Option<String>> {
    Ok(read(home)?
        .get(&key(provider))
        .filter(|model| !model.trim().is_empty())
        .cloned())
}

/// Atomically save a model without modifying other providers or configuration.
pub fn set(home: &Path, provider: &Provider, model: &str) -> io::Result<()> {
    use std::io::Write;
    if model.trim().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty model"));
    }
    let mut values = read(home)?;
    values.insert(key(provider), model.to_owned());
    let directory = home.join(".gitcode");
    std::fs::create_dir_all(&directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer_pretty(&mut temporary, &values).map_err(io::Error::other)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(directory.join("models.json"))
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn selected_models_persist_and_are_scoped_to_provider_and_protocol() {
        let home = tempfile::tempdir().unwrap();
        let first = Provider::default();
        let mut second = first.clone();
        second.base_url = "https://other.example/v1".into();
        assert_eq!(get(home.path(), &first).unwrap(), None);
        set(home.path(), &first, "model-a").unwrap();
        set(home.path(), &second, "model-b").unwrap();
        assert_eq!(
            get(home.path(), &first).unwrap().as_deref(),
            Some("model-a")
        );
        assert_eq!(
            get(home.path(), &second).unwrap().as_deref(),
            Some("model-b")
        );
        second.protocol = crate::config::Protocol::OpenaiChat;
        assert_eq!(get(home.path(), &second).unwrap(), None);
        assert!(set(home.path(), &first, " ").is_err());
    }
}
