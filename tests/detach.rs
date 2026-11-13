//! A disconnected terminal must not cancel the live agent turn.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::process::Command;

#[test]
fn terminal_disconnect_keeps_turn_running_and_resume_reattaches() {
    if Command::new("tmux").arg("-V").output().is_err()
        || Command::new("python3").arg("--version").output().is_err()
    {
        eprintln!("skipping disconnect test: tmux and python3 are required");
        return;
    }
    for script in ["verify-detach.py", "verify-existing-tmux.py"] {
        let status = Command::new("python3")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(script),
            )
            .env("GITCODE_TEST_BINARY", env!("CARGO_BIN_EXE_gitcode"))
            .status()
            .unwrap();
        assert!(status.success(), "{script} failed");
    }
}
