//! Embed the source revision in release version output.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed=GITCODE_COMMIT");
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output()
    {
        if output.status.success() {
            println!(
                "cargo:rerun-if-changed={}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
    }
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--git-path", "refs/heads"])
        .output()
    {
        if output.status.success() {
            println!(
                "cargo:rerun-if-changed={}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
    }
    println!("cargo:rerun-if-changed=.git/packed-refs");
    let commit = std::env::var("GITCODE_COMMIT")
        .ok()
        .or_else(|| {
            let output = Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .filter(|value| {
            !value.is_empty() && value.len() <= 40 && value.bytes().all(|c| c.is_ascii_hexdigit())
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GITCODE_COMMIT={commit}");
}
