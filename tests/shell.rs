//! M7 standalone backend coverage; exports intentionally remain unchanged.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use gitcode::perm::{Engine, Mode, Rule};
use gitcode::tools::shell;
use shell::{Session, OUTPUT_LIMIT};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
const LIMIT: Duration = Duration::from_secs(5);

struct Fixture {
    root: PathBuf,
    engine: Engine,
    session: Session,
}
impl Fixture {
    fn new(mode: Mode) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "m7-shell-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let engine = Engine::new(&root, mode).unwrap();
        let session = Session::new(&engine, &root.join("home")).unwrap();
        Self {
            root,
            engine,
            session,
        }
    }
    async fn run(&self, command: &str) -> shell::Output {
        self.session
            .execute(&self.engine, command, LIMIT)
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn splitter_is_conservative_and_quote_aware() {
    assert_eq!(
        shell::split_commands("printf 'a; b|c && d' && pwd || false; printf \"x|y\" | wc -c")
            .unwrap(),
        [
            "printf 'a; b|c && d'",
            "pwd",
            "false",
            "printf \"x|y\"",
            "wc -c"
        ]
    );
    assert_eq!(
        shell::split_commands("printf a\\;b;").unwrap(),
        ["printf a\\;b"]
    );
    assert_eq!(
        shell::split_commands(
            "ls -la /root 2>/dev/null | head -50; echo \"---\"; find / -maxdepth 4 -iname \"*dotdotagent*\" -not -path \"*/proc/*\" 2>/dev/null | head -50"
        )
        .unwrap(),
        [
            "ls -la /root 2>/dev/null",
            "head -50",
            "echo \"---\"",
            "find / -maxdepth 4 -iname \"*dotdotagent*\" -not -path \"*/proc/*\" 2>/dev/null",
            "head -50"
        ]
    );
    assert_eq!(
        shell::split_commands("ls /root 2>/dev/null | head -5; find / -maxdepth 2 2>&1 | head")
            .unwrap(),
        [
            "ls /root 2>/dev/null",
            "head -5",
            "find / -maxdepth 2 2>&1",
            "head"
        ]
    );
    for command in [
        "",
        " ",
        "pwd &&",
        "pwd |",
        "; pwd",
        "pwd;;pwd",
        "pwd & pwd",
        "pwd |& cat",
        "(pwd)",
        "echo $(pwd)",
        "echo `pwd`",
        "echo \"$HOME\"",
        "A=x pwd",
        "echo hi > file",
        "echo hi 2>/dev/null/file",
        "echo hi 2>&1 > file",
        "cat <<EOF",
        "cat <<<x",
        "echo *",
        "echo # comment",
        "if true; then pwd; fi",
        "'eval' x",
        "eval pwd",
        "source x",
        "set -e",
        "trap x EXIT",
        "exec pwd",
        "echo 'unfinished",
        "echo x\\",
        "pwd\npwd",
        "echo a\0b",
    ] {
        assert!(
            shell::split_commands(command).is_err(),
            "unexpectedly accepted: {command:?}"
        );
    }
}

#[tokio::test]
async fn permissions_fail_closed_before_any_side_effect() {
    let mut f = Fixture::new(Mode::Default);
    assert!(f
        .session
        .execute(&f.engine, "touch forbidden", LIMIT)
        .await
        .unwrap_err()
        .contains("approval"));
    f.engine.allow.push(Rule::parse("Bash(touch *)").unwrap());
    assert!(f
        .session
        .execute(&f.engine, "touch forbidden; pwd", LIMIT)
        .await
        .is_err());
    assert!(!f.root.join("forbidden").exists());
    f.engine.allow.push(Rule::parse("Bash(pwd)").unwrap());
    assert!(f
        .session
        .execute(&f.engine, "touch allowed; pwd", LIMIT)
        .await
        .is_ok());
    f.engine.ask.push(Rule::parse("Bash(touch *)").unwrap());
    assert!(f
        .session
        .execute(&f.engine, "touch asked", LIMIT)
        .await
        .unwrap_err()
        .contains("approval"));
    f.engine.mode = Mode::Plan;
    assert!(f.session.execute(&f.engine, "pwd", LIMIT).await.is_err());
    f.engine.mode = Mode::Bypass;
    f.engine.deny.push(Rule::parse("Bash(touch *)").unwrap());
    assert!(f
        .session
        .execute(&f.engine, "pwd && touch denied", LIMIT)
        .await
        .is_err());
    assert!(f
        .session
        .execute(&f.engine, "touch complex > out", LIMIT)
        .await
        .is_err());
    f.engine.deny.clear();
    f.engine.deny.push(Rule::parse("Bash(pwd; pwd)").unwrap());
    assert!(f
        .session
        .execute(&f.engine, "pwd; pwd", LIMIT)
        .await
        .is_err());
    assert!(!f.root.join("asked").exists());
    assert!(!f.root.join("denied").exists());
    f.session.close().await;
}

#[tokio::test]
async fn ordinary_bash_syntax_works_and_complex_rules_fail_closed() {
    let mut f = Fixture::new(Mode::Bypass);
    let result = f.run("name=world\nprintf '%s\\n' \"hello $(printf '%s' \"$name\")\" > result\ncat <<'EOF' >> result\nquoted 'text' and \"text\"\nEOF\ncat result").await;
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.output, "hello world\nquoted 'text' and \"text\"\n");
    let result = f.run("for f in result; do cat \"$f\"; done").await;
    assert_eq!(result.exit_code, Some(0));
    assert!(result.output.contains("hello world"));
    f.engine
        .deny
        .push(Rule::parse("Bash(touch blocked)").unwrap());
    assert!(f
        .session
        .execute(&f.engine, "echo $(touch blocked)", LIMIT)
        .await
        .is_err());
    assert!(!f.root.join("blocked").exists());
    f.engine.deny.clear();
    f.engine.mode = Mode::Default;
    assert!(f
        .session
        .execute(&f.engine, "echo $(printf test)", LIMIT)
        .await
        .unwrap_err()
        .contains("approval"));
    f.session.close().await;
}

#[tokio::test]
async fn subprocess_output_cannot_forge_completion_and_path_is_inherited() {
    let f = Fixture::new(Mode::Bypass);
    let result = f
        .run("printf '\\036M71:0\\037'; sleep 0.1; printf finished; false")
        .await;
    assert_eq!(result.exit_code, Some(1));
    assert!(result.output.contains("finished"));
    assert!(result.output.contains("M71:0"));
    let result = f.run("printf '%s' \"$PATH\"").await;
    assert_eq!(result.output, std::env::var("PATH").unwrap_or_default());
    f.session.close().await;
}

#[tokio::test]
async fn persistent_cwd_env_status_and_noninteractive_stdin() {
    let f = Fixture::new(Mode::Bypass);
    assert_eq!(
        f.run("cd sub; export M7_VALUE='persisted value'")
            .await
            .exit_code,
        Some(0)
    );
    assert_eq!(
        f.run("pwd").await.output.trim(),
        f.root.join("sub").to_str().unwrap()
    );
    let env = f.run("printenv").await.output;
    assert!(env.contains("M7_VALUE=persisted value\n"));
    assert!(env.contains(&format!("HOME={}\n", f.root.join("home").display())));
    assert!(env.contains("GIT_TERMINAL_PROMPT=0\n"));
    assert!(!env.contains("BASH_ENV="));
    f.run("unset M7_VALUE").await;
    assert_eq!(f.run("printenv M7_VALUE").await.exit_code, Some(1));
    assert_eq!(f.run("false").await.exit_code, Some(1));
    assert_eq!(
        f.run("printf 'without newline'").await.output,
        "without newline"
    );
    assert_eq!(f.run("cat").await.output, "");
    assert_eq!(f.run("printf abc | wc -c").await.output.trim(), "3");
    // Script emits stdout, stderr, stdout, and a failing status without relying on parser redirection.
    std::fs::write(
        f.root.join("merged.sh"),
        "printf first; printf second >&2; printf third; exit 7\n",
    )
    .unwrap();
    let merged = f.run("bash ../merged.sh").await;
    assert_eq!(merged.output, "firstsecondthird");
    assert_eq!(merged.exit_code, Some(7));
    f.session.close().await;
}

#[tokio::test]
async fn bounded_output_drains_and_session_survives() {
    let f = Fixture::new(Mode::Bypass);
    let output = f.run("head -c 200000 /dev/zero").await;
    assert_eq!(output.output.len(), OUTPUT_LIMIT);
    assert!(output.truncated);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(f.run("printf next").await.output, "next");
    f.session.close().await;
}

#[tokio::test]
async fn timeout_resets_session_and_validates_bounds() {
    let f = Fixture::new(Mode::Bypass);
    assert!(f
        .session
        .execute(&f.engine, "touch invalid", Duration::ZERO)
        .await
        .is_err());
    assert_eq!(
        f.session
            .execute(&f.engine, "printf long", Duration::from_secs(601))
            .await
            .unwrap()
            .output,
        "long"
    );
    assert_eq!(
        f.session
            .execute(&f.engine, "printf no-deadline", Duration::MAX)
            .await
            .unwrap()
            .output,
        "no-deadline"
    );
    assert!(!f.root.join("invalid").exists());
    f.run("cd sub; export M7_RESET=yes").await;
    let output = f
        .session
        .execute(&f.engine, "sleep 30", Duration::from_millis(80))
        .await
        .unwrap();
    assert!(output.timed_out);
    assert_eq!(output.exit_code, None);
    assert_eq!(f.run("pwd").await.output.trim(), f.root.to_str().unwrap());
    assert_eq!(f.run("printenv M7_RESET").await.exit_code, Some(1));
    f.session.close().await;
}

#[tokio::test]
async fn background_incremental_poll_wait_and_kill() {
    let f = Fixture::new(Mode::Bypass);
    let mut job = f
        .session
        .background(
            &f.engine,
            "printf 'incremental-output-before-sleep'; sleep 0.25; printf done",
            LIMIT,
        )
        .await
        .unwrap();
    let mut incremental = String::new();
    tokio::time::timeout(LIMIT, async {
        while incremental.is_empty() {
            incremental.push_str(&job.poll().unwrap());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!job.is_finished());
    assert!(job.poll().unwrap().is_empty());
    let output = job.wait().await.unwrap();
    assert_eq!(output.output, "incremental-output-before-sleepdone");
    let mut job = f
        .session
        .background(
            &f.engine,
            "printf 'kill-ready-marker-long-enough'; sleep 30; touch survived",
            LIMIT,
        )
        .await
        .unwrap();
    tokio::time::timeout(LIMIT, async {
        while job.poll().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    job.kill().await.unwrap();
    assert_eq!(f.run("printf alive").await.output, "alive");
    assert!(!f.root.join("survived").exists());
    f.session.close().await;
}

#[tokio::test]
async fn cancellation_kills_descendants_and_resets_session() {
    let f = Fixture::new(Mode::Bypass);
    std::fs::write(f.root.join("descendant.sh"), "sleep 0.6; touch escaped\n").unwrap();
    let mut job = f
        .session
        .background(
            &f.engine,
            "printf 'descendant-start-marker-long'; bash descendant.sh",
            LIMIT,
        )
        .await
        .unwrap();
    tokio::time::timeout(LIMIT, async {
        while job.poll().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Let Bash start its child before cancellation.
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(job);
    assert_eq!(f.run("printf reset").await.output, "reset");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!f.root.join("escaped").exists());
    f.session.close().await;
}

#[tokio::test]
async fn foreground_future_drop_and_independent_sessions() {
    let f = Fixture::new(Mode::Bypass);
    let other = Session::new(&f.engine, &f.root.join("home")).unwrap();
    f.run("cd sub").await;
    assert_eq!(
        other
            .execute(&f.engine, "pwd", LIMIT)
            .await
            .unwrap()
            .output
            .trim(),
        f.root.to_str().unwrap()
    );
    assert!(tokio::time::timeout(
        Duration::from_millis(80),
        f.session.execute(&f.engine, "sleep 30", LIMIT)
    )
    .await
    .is_err());
    assert_eq!(f.run("pwd").await.output.trim(), f.root.to_str().unwrap());
    let different = Engine::new(&f.root.join("sub"), Mode::Bypass).unwrap();
    assert!(other.execute(&different, "pwd", LIMIT).await.is_err());
    f.session.close().await;
    other.close().await;
}

#[tokio::test]
async fn background_wait_reports_failure_without_consuming_output() {
    let f = Fixture::new(Mode::Bypass);
    let mut tools = gitcode::tools::Tools::new(f.engine.clone());
    let started = tools.execute("Bash", &serde_json::json!({"command":"printf job-output; sh -c 'exit 7'", "run_in_background":true})).await.unwrap();
    let id = started.split_whitespace().last().unwrap();
    let output = tools
        .execute(
            "BashOutput",
            &serde_json::json!({"job_id":id, "wait_ms":5000}),
        )
        .await
        .unwrap();
    assert!(output.contains("finished=true"), "{output}");
    assert!(output.contains("exit_code=7"), "{output}");
    assert!(output.contains("job-output"), "{output}");
    tools.shutdown().await.unwrap();
}

#[tokio::test]
async fn long_foreground_command_yields_and_preserves_shell_session() {
    let f = Fixture::new(Mode::Bypass);
    let mut tools = gitcode::tools::Tools::new(f.engine.clone());
    let start = tools.execute("Bash", &serde_json::json!({"command":"export GITCODE_YIELD_TEST=works; printf early; sleep 0.2; printf late", "yield_time_ms":20})).await.unwrap();
    assert!(start.contains("background shell job job-1"), "{start}");
    let output = tools
        .execute(
            "BashOutput",
            &serde_json::json!({"job_id":"job-1", "wait_ms":5000}),
        )
        .await
        .unwrap();
    assert!(
        output.contains("finished=true")
            && output.contains("exit_code=0")
            && output.contains("late"),
        "{output}"
    );
    let env = tools
        .execute(
            "Bash",
            &serde_json::json!({"command":"printenv GITCODE_YIELD_TEST"}),
        )
        .await
        .unwrap();
    assert_eq!(env.trim(), "works");
    tools.shutdown().await.unwrap();
}
