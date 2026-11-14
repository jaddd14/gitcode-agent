//! Explicit persistent Bash sessions, not an OS sandbox. Authorized executables can access
//! the host and escape process groups. No filesystem confinement is claimed.
//! Commands in a session are serialized (including background jobs). `cd`, `export`, and
//! `unset` persist; cancellation, timeout, or protocol failure resets the interpreter.
//! Unix only: cancellation uses an isolated process group and best-effort `/bin/kill`.

use crate::perm::{Decision, Engine, Mode, Rule};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::oneshot,
};

/// Maximum retained merged stdout/stderr bytes per command.
pub const OUTPUT_LIMIT: usize = 100_000;

/// Split simple shell lists into independently permission-checked command strings.
/// Quotes and escapes protect literal separators. Pipelines and &&/||/; are supported.
/// Expansions, substitutions, subshells, output-destroying redirections other than
/// `2>/dev/null` and `2>&1` (including heredocs), background operators, assignments
/// before executables, globs, comments and control syntax fail closed, even in bypass
/// mode. This is deliberately not a general shell parser.
pub fn split_commands(command: &str) -> Result<Vec<String>, String> {
    if command.len() > 32_768 {
        return Err("command exceeds 32 KiB".into());
    }
    let mut parts = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    let mut chars = command.char_indices().peekable();
    let mut trailing = false;
    while let Some((i, ch)) = chars.next() {
        if ch.is_control() {
            return Err("control characters are unsupported".into());
        }
        if escaped {
            escaped = false;
            continue;
        }
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            }
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if quote == Some('"') {
            if matches!(ch, '$' | '`') {
                return Err("shell expansion is unsupported".into());
            }
            if ch == '"' {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '&' if chars.peek().is_some_and(|(_, next)| *next == '>') => {
                let Some(length) = discard_redirect(&command[i..]) else {
                    return Err("complex shell syntax is unsupported".into());
                };
                while chars.peek().is_some_and(|(index, _)| *index < i + length) {
                    chars.next();
                }
            }
            '>' => {
                let Some(length) = discard_redirect(&command[i..]) else {
                    return Err("complex shell syntax is unsupported".into());
                };
                while chars.peek().is_some_and(|(index, _)| *index < i + length) {
                    chars.next();
                }
            }
            '$' | '`' | '(' | ')' | '<' | '{' | '}' | '*' | '?' | '[' | ']' | '~' | '#' => {
                return Err("complex shell syntax is unsupported".into())
            }
            '&' | '|' | ';' => {
                let part = command[start..i].trim();
                validate_simple(part)?;
                parts.push(part.to_owned());
                trailing = ch == ';';
                if chars.peek().is_some_and(|(_, next)| *next == ch) {
                    if ch == ';' {
                        return Err("control syntax is unsupported".into());
                    }
                    chars.next();
                } else if ch == '&' {
                    return Err("use the background job API".into());
                }
                start = chars.peek().map_or(command.len(), |(i, _)| *i);
            }
            _ => {}
        }
    }
    if quote.is_some() || escaped {
        return Err("unterminated quote or escape".into());
    }
    let last = command[start..].trim();
    if !last.is_empty() {
        validate_simple(last)?;
        parts.push(last.into());
    } else if !trailing {
        return Err("empty command or trailing operator".into());
    }
    if parts.is_empty() {
        return Err("empty command".into());
    }
    Ok(parts)
}

/// Byte length of a redirect that only discards output, e.g. `2>/dev/null` or `2>&1`.
///
/// Silencing a stream touches no file and runs nothing, so it passes the
/// conservative splitter; every other redirect still fails closed.
fn discard_redirect(rest: &str) -> Option<usize> {
    let mut index = usize::from(rest.starts_with('&'));
    index += rest[index..]
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    index += if rest[index..].starts_with(">>") {
        2
    } else if rest[index..].starts_with('>') {
        1
    } else {
        return None;
    };
    index += rest[index..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let target = &rest[index..];
    if let Some(after) = target.strip_prefix("/dev/null") {
        // The word must end here: `/dev/null/file` writes a real file.
        let boundary = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || "_./-".contains(c)));
        return boundary.then_some(index + "/dev/null".len());
    }
    let descriptor = target.strip_prefix('&')?;
    let digits = descriptor.chars().take_while(char::is_ascii_digit).count();
    (digits > 0).then_some(index + 1 + digits)
}

fn validate_simple(part: &str) -> Result<(), String> {
    let first = part.split_whitespace().next().unwrap_or("");
    // Require a literal executable/builtin name: quoting must not hide control syntax.
    if first.is_empty()
        || !first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
    {
        return Err("executable must be a literal name; prefix assignments are unsupported".into());
    }
    if matches!(
        first,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "function"
            | "select"
            | "in"
            | "!"
            | "time"
            | "coproc"
            | "exit"
            | "logout"
            | "exec"
            | "eval"
            | "source"
            | "."
            | "trap"
            | "set"
            | "shopt"
            | "enable"
            | "builtin"
            | "command"
            | "return"
            | "break"
            | "continue"
    ) {
        return Err("shell control builtins are unsupported".into());
    }
    Ok(())
}

/// Conservatively split native dialects without treating Windows path slashes as escapes.
pub fn split_native_commands(
    command: &str,
    kind: crate::platform::ShellKind,
) -> Result<Vec<String>, String> {
    use crate::platform::ShellKind;
    if kind == ShellKind::Bash {
        return split_commands(command);
    }
    if command.is_empty() || command.len() > 32_768 {
        return Err("empty or oversized command".into());
    }
    let powershell = matches!(kind, ShellKind::Pwsh | ShellKind::PowerShell);
    let mut quote = None;
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, ch) in command.char_indices() {
        if ch.is_control() {
            return Err("control characters are unsupported".into());
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            if active == '"'
                && ((powershell && matches!(ch, '$' | '`'))
                    || (!powershell && matches!(ch, '%' | '!')))
            {
                return Err("quoted expansion is unsupported".into());
            }
            continue;
        }
        if ch == '"' || powershell && ch == '\'' {
            quote = Some(ch);
            continue;
        }
        if ch == ';' && powershell {
            parts.push(command[start..index].trim().to_owned());
            start = index + 1;
            continue;
        }
        if matches!(
            ch,
            '&' | '|' | '<' | '>' | '(' | ')' | '{' | '}' | '`' | '^' | '*' | '?' | '#' | '%' | '!'
        ) {
            return Err("complex native shell syntax is unsupported".into());
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    parts.push(command[start..].trim().to_owned());
    for part in &parts {
        let first = part.split_whitespace().next().unwrap_or("");
        let env_assignment = powershell && first.starts_with("$env:") && part.contains('=');
        if first.is_empty()
            || (!env_assignment
                && !first
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_./\\:-".contains(c)))
        {
            return Err("executable must be a literal name".into());
        }
        if matches!(
            first.to_ascii_lowercase().as_str(),
            "exit"
                | "return"
                | "throw"
                | "trap"
                | "function"
                | "if"
                | "for"
                | "foreach"
                | "while"
                | "do"
                | "invoke-expression"
                | "iex"
                | "set-alias"
                | "new-alias"
                | "remove-item"
        ) {
            // These can alter interpreter framing or bypass per-command classification.
            return Err("native control syntax is unsupported".into());
        }
    }
    Ok(parts)
}

/// Inspect simple commands component by component. Complex Bash is authorized as
/// a whole and is never covered by remembered simple-command prefixes.
pub fn permission_decision(engine: &Engine, command: &str) -> Result<Decision, String> {
    let kind = crate::platform::detect_shell("auto")?.kind;
    permission_decision_for(engine, command, kind)
}

/// Check a command using the dialect of its configured interpreter.
pub fn permission_decision_for(
    engine: &Engine,
    command: &str,
    kind: crate::platform::ShellKind,
) -> Result<Decision, String> {
    if command.trim().is_empty() || command.contains('\0') {
        return Err("empty command or NUL byte".into());
    }
    let parsed = if kind == crate::platform::ShellKind::Bash {
        split_commands(command)
    } else {
        split_native_commands(command, kind)
    };
    let parts = match parsed {
        Ok(parts) => parts,
        Err(_) => {
            // We cannot safely infer every executable from arbitrary shell syntax.
            // Scoped deny/ask rules must therefore not be silently bypassed.
            let whole = engine.decide("Bash", command, true);
            if whole == Decision::Deny
                || engine
                    .deny
                    .iter()
                    .any(|rule| rule.tool == "Bash" || rule.tool == "*")
            {
                return Ok(Decision::Deny);
            }
            if engine.mode != Mode::Bypass
                && engine
                    .ask
                    .iter()
                    .any(|rule| rule.tool == "Bash" || rule.tool == "*")
            {
                return Ok(Decision::Ask);
            }
            return Ok(whole);
        }
    };
    let mut whole = engine.clone();
    if whole.mode != Mode::Bypass {
        whole.allow = vec![Rule::parse("Bash(**)")?];
    }
    let mut result = Decision::Allow;
    for decision in std::iter::once(whole.decide("Bash", command, true))
        .chain(parts.iter().map(|part| engine.decide("Bash", part, true)))
    {
        match decision {
            Decision::Deny => return Ok(Decision::Deny),
            Decision::Ask => result = Decision::Ask,
            Decision::Allow => {}
        }
    }
    Ok(result)
}

/// A conservative, visible scope for similar single commands: executable and first argument.
pub fn similar_prefix(command: &str) -> Option<String> {
    let parts = split_commands(command).ok()?;
    if parts.len() != 1 || command.trim_end().ends_with(';') {
        return None;
    }
    let mut words = parts[0].split_whitespace();
    let executable = words.next()?;
    let argument = words.next()?;
    if !argument
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
    {
        return None;
    }
    Some(format!("{executable} {argument}"))
}

fn authorize(engine: &Engine, command: &str, shell: &crate::platform::Shell) -> Result<(), String> {
    match permission_decision_for(engine, command, shell.kind)? {
        Decision::Allow => Ok(()),
        Decision::Ask => Err("permission requires explicit approval".into()),
        Decision::Deny => Err("permission denied by policy".into()),
    }
}

/// Completed command with bounded, lossy UTF-8 merged output; nonzero exits are not errors.
#[derive(Debug, Clone)]
pub struct Output {
    /// Merged stdout/stderr in pipe arrival order.
    pub output: String,
    /// Shell exit status; absent on timeout.
    pub exit_code: Option<i32>,
    /// Whether the deadline expired and the session was reset.
    pub timed_out: bool,
    /// Whether bytes beyond OUTPUT_LIMIT were discarded (pipes are still drained).
    pub truncated: bool,
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    cursor: usize,
    truncated: bool,
}
impl Capture {
    fn append(&mut self, bytes: &[u8]) {
        let count = bytes.len().min(OUTPUT_LIMIT - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        self.truncated |= count < bytes.len();
    }
    fn output(&self, code: Option<i32>, timed_out: bool) -> Output {
        Output {
            output: String::from_utf8_lossy(&self.bytes).into_owned(),
            exit_code: code,
            timed_out,
            truncated: self.truncated,
        }
    }
}

/// An explicitly owned shell interpreter. Clones share the same serialized session.
/// The supplied existing HOME is passed only to children; the host environment is untouched.
#[derive(Clone)]
pub struct Session {
    root: PathBuf,
    home: PathBuf,
    process: Arc<tokio::sync::Mutex<Option<Process>>>,
    shell: crate::platform::Shell,
}
impl Session {
    /// Create a lazy session rooted at the engine workspace, with an explicit child HOME.
    pub fn new(engine: &Engine, home: &Path) -> Result<Self, String> {
        Self::with_shell(engine, home, "auto")
    }

    /// Create a session using auto, bash, pwsh, powershell, or cmd selection.
    pub fn with_shell(engine: &Engine, home: &Path, selection: &str) -> Result<Self, String> {
        let shell = crate::platform::detect_shell(selection)?;
        let home = home
            .canonicalize()
            .map_err(|_| "cannot resolve shell HOME")?;
        if !home.is_dir() {
            return Err("shell HOME must be a directory".into());
        }
        Ok(Self {
            root: engine.root.clone(),
            home,
            process: Arc::new(tokio::sync::Mutex::new(None)),
            shell,
        })
    }

    /// Execute with current policy; Ask is an error, never implicit approval.
    /// The timeout must be nonzero. `Duration::MAX` means no deadline. Dropping this future
    /// cancels the active process group and resets state. No auto-read-only heuristic.
    pub async fn execute(
        &self,
        engine: &Engine,
        command: &str,
        timeout: Duration,
    ) -> Result<Output, String> {
        self.run(
            engine,
            command,
            timeout,
            Arc::new(Mutex::new(Capture::default())),
        )
        .await
    }

    /// Start a supervised job. Poll for incremental output, then wait or kill it.
    /// A job occupies this interpreter until completion; use another Session for parallelism.
    pub async fn background(
        &self,
        engine: &Engine,
        command: &str,
        timeout: Duration,
    ) -> Result<Job, String> {
        self.check(engine, command, timeout)?;
        let capture = Arc::new(Mutex::new(Capture::default()));
        let (cancel, cancelled) = oneshot::channel();
        let (session, engine, command, shared) = (
            self.clone(),
            engine.clone(),
            command.to_owned(),
            capture.clone(),
        );
        let task = tokio::spawn(async move {
            tokio::select! {
                result = session.run(&engine, &command, timeout, shared) => result,
                _ = cancelled => Err("shell job cancelled; active session reset".into()),
            }
        });
        Ok(Job {
            capture,
            cancel: Some(cancel),
            task: Some(task),
            result: None,
        })
    }

    fn check(&self, engine: &Engine, command: &str, timeout: Duration) -> Result<(), String> {
        if timeout.is_zero() {
            return Err("timeout must be positive".into());
        }
        if engine.root != self.root {
            return Err("session workspace does not match permission engine".into());
        }
        authorize(engine, command, &self.shell)
    }

    async fn run(
        &self,
        engine: &Engine,
        command: &str,
        timeout: Duration,
        capture: Arc<Mutex<Capture>>,
    ) -> Result<Output, String> {
        self.check(engine, command, timeout)?;
        let work = async {
            let mut slot = self.process.lock().await;
            let mut process = match slot.take() {
                Some(process) => process,
                None => Process::spawn(&self.root, &self.home, &self.shell)?,
            };
            // Command output is untrusted. A fresh OS-random marker prevents a
            // file or subprocess from accidentally or deliberately ending a job.
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|_| "cannot create shell completion marker")?;
            let id = nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let marker = format!("\x1eGC{id}:").into_bytes();
            let script = self.shell.script_with_marker(command, &format!("GC{id}"));
            process
                .input
                .write_all(script.as_bytes())
                .await
                .map_err(|_| "cannot write shell command")?;
            let mut pending = Vec::new();
            let code = loop {
                let mut chunk = [0; 4096];
                let count = process
                    .output
                    .read(&mut chunk)
                    .await
                    .map_err(|_| "cannot read shell output")?;
                if count == 0 {
                    return Err("shell exited before completion marker".into());
                }
                pending.extend_from_slice(&chunk[..count]);
                if let Some(index) = pending.windows(marker.len()).position(|w| w == marker) {
                    capture
                        .lock()
                        .map_err(|_| "output lock poisoned")?
                        .append(&pending[..index]);
                    pending.drain(..index);
                    if let Some(end) = pending.iter().position(|b| *b == 0x1f) {
                        let code = std::str::from_utf8(&pending[marker.len()..end])
                            .ok()
                            .and_then(|s| s.parse::<i32>().ok())
                            .ok_or("invalid shell completion marker")?;
                        break code;
                    }
                    if pending.len() > 128 {
                        return Err("invalid shell completion marker".into());
                    }
                } else {
                    let keep = (1..marker.len())
                        .rev()
                        .find(|&n| pending.ends_with(&marker[..n]))
                        .unwrap_or(0);
                    let ready = pending.len() - keep;
                    capture
                        .lock()
                        .map_err(|_| "output lock poisoned")?
                        .append(&pending[..ready]);
                    pending.drain(..ready);
                }
            };
            *slot = Some(process);
            Ok::<i32, String>(code)
        };
        let result = if tokio::time::Instant::now().checked_add(timeout).is_none() {
            Ok(work.await)
        } else {
            tokio::time::timeout(timeout, work).await
        };
        match result {
            Ok(result) => Ok(capture
                .lock()
                .map_err(|_| "output lock poisoned")?
                .output(Some(result?), false)),
            Err(_) => Ok(capture
                .lock()
                .map_err(|_| "output lock poisoned")?
                .output(None, true)),
        }
    }

    /// Kill/reset an idle session. Waits for any active job; use Job::kill for active work.
    pub async fn close(&self) {
        let process = self.process.lock().await.take();
        if let Some(mut process) = process {
            let _ = process.child.start_kill();
            let _ = process.child.wait().await;
            // Drop also closes the job/process-group guard and descendants.
            drop(process);
        }
    }
}

/// Supervised job; dropping it requests cancellation, while kill waits for cleanup.
pub struct Job {
    capture: Arc<Mutex<Capture>>,
    cancel: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<Result<Output, String>>>,
    result: Option<Result<Output, String>>,
}
impl Job {
    /// Read bytes not returned by earlier polls (lossy UTF-8, possibly split across polls).
    /// Output beyond the per-command cap is discarded rather than buffered indefinitely.
    pub fn poll(&mut self) -> Result<String, String> {
        let mut capture = self.capture.lock().map_err(|_| "output lock poisoned")?;
        let output = String::from_utf8_lossy(&capture.bytes[capture.cursor..]).into_owned();
        capture.cursor = capture.bytes.len();
        Ok(output)
    }
    /// Whether the worker finished, including errors and timeout.
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(|task| task.is_finished())
    }
    /// A handle for observing completion without consuming the job.
    pub fn completion_handle(&self) -> Option<tokio::task::AbortHandle> {
        self.task
            .as_ref()
            .map(tokio::task::JoinHandle::abort_handle)
    }
    /// Wait up to the specified duration without consuming the job or its output.
    pub async fn wait_ready(&mut self, duration: Duration) {
        if let Some(task) = self.task.as_mut() {
            if let Ok(result) = tokio::time::timeout(duration, task).await {
                self.result = Some(result.unwrap_or_else(|_| Err("shell worker failed".into())));
                self.task.take();
                self.cancel.take();
            }
        }
    }
    /// Describe the final outcome after completion has been collected.
    pub fn completion_status(&self) -> Option<String> {
        self.result.as_ref().map(|result| match result {
            Ok(output) if output.timed_out => "timed out".to_string(),
            Ok(output) => format!(
                "exit_code={}",
                output
                    .exit_code
                    .map_or_else(|| "unknown".to_string(), |code| code.to_string())
            ),
            Err(error) => format!("error: {error}"),
        })
    }
    /// Wait for completion and receive the full bounded output, including previously polled bytes.
    pub async fn wait(mut self) -> Result<Output, String> {
        if let Some(result) = self.result.take() {
            return result;
        }
        let result = self
            .task
            .as_mut()
            .ok_or("job already consumed")?
            .await
            .map_err(|_| "shell worker failed")?;
        self.task.take();
        self.cancel.take();
        result
    }
    /// Request cancellation and wait until the worker has dropped its process-group guard.
    pub async fn kill(mut self) -> Result<(), String> {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.map_err(|_| "shell worker failed")?.err();
        }
        Ok(())
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

struct Process {
    child: Child,
    input: ChildStdin,
    #[cfg(unix)]
    output: tokio::net::UnixStream,
    #[cfg(windows)]
    output: tokio::io::DuplexStream,
    #[cfg(unix)]
    group: u32,
    #[cfg(windows)]
    _tree: crate::platform::ProcessTree,
}
impl Process {
    #[cfg(unix)]
    fn spawn(root: &Path, home: &Path, shell: &crate::platform::Shell) -> Result<Self, String> {
        let (reader, writer) =
            std::os::unix::net::UnixStream::pair().map_err(|_| "cannot create output socket")?;
        reader
            .set_nonblocking(true)
            .map_err(|_| "cannot configure output socket")?;
        let stderr = writer
            .try_clone()
            .map_err(|_| "cannot merge shell output")?;
        let mut command = Command::new(&shell.executable);
        command
            .args(["--noprofile", "--norc"])
            .current_dir(root)
            .env("HOME", home)
            .env("TERM", "dumb")
            .env("CI", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(std::os::fd::OwnedFd::from(writer)))
            .stderr(Stdio::from(std::os::fd::OwnedFd::from(stderr)))
            .process_group(0)
            .kill_on_drop(true);
        let output = tokio::net::UnixStream::from_std(reader)
            .map_err(|_| "cannot register output socket")?;
        let mut child = command.spawn().map_err(|_| "cannot start Bash")?;
        let group = child.id().ok_or("shell has no process id")?;
        let input = child.stdin.take().ok_or("shell stdin unavailable")?;
        Ok(Self {
            child,
            input,
            output,
            group,
        })
    }
}
#[cfg(windows)]
impl Process {
    fn spawn(root: &Path, home: &Path, shell: &crate::platform::Shell) -> Result<Self, String> {
        use crate::platform::ShellKind;
        use std::io::Read;
        let (mut reader, writer) = os_pipe::pipe().map_err(|_| "cannot create shell pipe")?;
        let stderr = writer
            .try_clone()
            .map_err(|_| "cannot merge shell output")?;
        let mut command = Command::new(&shell.executable);
        match shell.kind {
            ShellKind::Bash => {
                command.args(["--noprofile", "--norc"]);
            }
            ShellKind::Pwsh | ShellKind::PowerShell => {
                command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "-"]);
            }
            ShellKind::Cmd => {
                command.args(["/D", "/Q", "/K"]);
            }
        }
        command
            .current_dir(root)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("TERM", "dumb")
            .env("CI", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(writer))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| "cannot start native shell")?;
        drop(command);
        let tree = crate::platform::ProcessTree::attach(&child)?;
        let input = child.stdin.take().ok_or("shell stdin unavailable")?;
        let (output, mut sink) = tokio::io::duplex(65536);
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let mut chunk = [0; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if runtime.block_on(sink.write_all(&chunk[..count])).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            input,
            output,
            _tree: tree,
        })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // group comes only from our own unreaped child created with process_group(0).
        #[cfg(unix)]
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", self.group)])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.start_kill();
    }
}
