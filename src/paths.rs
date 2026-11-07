//! Explicit-home path resolution avoids global environment changes in tests.

use crate::error::{io, Result};
use std::path::{Component, Path, PathBuf};

/// Resolve a path relative to the workspace, expanding a leading home marker.
pub fn resolve(path: &Path, workspace: &Path, home: &Path) -> PathBuf {
    let mut components = path.components();
    let expanded = match components.next() {
        Some(Component::Normal(first)) if first == "~" => home.join(components.as_path()),
        _ if path.is_absolute() => path.to_path_buf(),
        _ => workspace.join(path),
    };
    normalize(&expanded)
}

/// Normalize dot components without following symlinks.
pub fn normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() && !path.is_absolute() {
                    output.push(component);
                }
            }
            _ => output.push(component),
        }
    }
    output
}

/// Canonicalize an existing path and verify its filesystem boundary.
pub fn confined_existing(path: &Path, root: &Path) -> Result<bool> {
    let canonical = path
        .canonicalize()
        .map_err(|e| io("canonicalize", path, e))?;
    let root = root
        .canonicalize()
        .map_err(|e| io("canonicalize", root, e))?;
    Ok(canonical.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_and_normalization() {
        assert_eq!(
            resolve(
                Path::new("~/x/../y"),
                Path::new("/work"),
                Path::new("/home/test")
            ),
            PathBuf::from("/home/test/y")
        );
        assert_eq!(
            resolve(
                Path::new("a/../b"),
                Path::new("/work"),
                Path::new("/home/test")
            ),
            PathBuf::from("/work/b")
        );
    }
}
