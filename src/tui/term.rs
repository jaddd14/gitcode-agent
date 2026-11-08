//! RAII terminal ownership with unwind and Unix termination cleanup.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, Weak};

/// Startup control sequence; never enters an alternate screen or enables a mouse.
pub const START: &str = "\x1b[2J\x1b[3J\x1b[H\x1b[?25l\x1b[?2004h";
/// Reset synchronized output, Kitty keyboard stack, paste, style, and cursor.
pub const RESTORE: &str = "\x1b[?2026l\x1b[<u\x1b[?2004l\x1b[0m\x1b[?25h";

/// Injectable system boundary used by the guard and cleanup tests.
pub trait TerminalBackend: Send + Sync {
    /// Enable platform raw input.
    fn raw(&self) -> io::Result<()>;
    /// Restore platform cooked input.
    fn cooked(&self) -> io::Result<()>;
    /// Write and flush control bytes.
    fn write(&self, bytes: &[u8]) -> io::Result<()>;
}

struct SystemTerminal;
impl TerminalBackend for SystemTerminal {
    fn raw(&self) -> io::Result<()> {
        crossterm::terminal::enable_raw_mode()
    }
    fn cooked(&self) -> io::Result<()> {
        crossterm::terminal::disable_raw_mode()
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }
}

struct State {
    backend: Arc<dyn TerminalBackend>,
    active: AtomicBool,
}
impl State {
    fn restore(&self) {
        if self.active.swap(false, Ordering::SeqCst) {
            // Raw restoration must still run when a disconnected output rejects writes.
            let _ = self.backend.write(RESTORE.as_bytes());
            let _ = self.backend.cooked();
        }
    }
}
static STATES: OnceLock<Mutex<Vec<Weak<State>>>> = OnceLock::new();
static PANIC: Once = Once::new();
fn register(state: &Arc<State>) {
    let states = STATES.get_or_init(|| Mutex::new(Vec::new()));
    let mut states = states.lock().unwrap_or_else(|e| e.into_inner());
    states.retain(|s| s.strong_count() > 0);
    states.push(Arc::downgrade(state));
    PANIC.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(states) = STATES.get() {
                for state in states
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .filter_map(Weak::upgrade)
                {
                    state.restore();
                }
            }
            previous(info);
        }));
    });
}

/// Exclusive terminal lifetime. Keep this guard alive throughout the UI session.
pub struct TerminalGuard {
    state: Arc<State>,
    #[cfg(unix)]
    signals: Option<(signal_hook::iterator::Handle, std::thread::JoinHandle<()>)>,
}
impl TerminalGuard {
    /// Enter on a real TTY, refusing to emit escape sequences into redirected output.
    pub fn enter() -> io::Result<Self> {
        if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "interactive terminal required",
            ));
        }
        Self::with_backend(Arc::new(SystemTerminal), true)
    }

    /// Enter through an injectable backend; signal handling is opt-in for test isolation.
    pub fn with_backend(backend: Arc<dyn TerminalBackend>, signals: bool) -> io::Result<Self> {
        backend.raw()?;
        let state = Arc::new(State {
            backend,
            active: AtomicBool::new(true),
        });
        register(&state);
        let mut guard = Self {
            state,
            #[cfg(unix)]
            signals: None,
        };
        #[cfg(unix)]
        if signals {
            use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
            let mut iterator = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])?;
            let handle = iterator.handle();
            let state = Arc::clone(&guard.state);
            let worker = std::thread::Builder::new()
                .name("terminal-signals".into())
                .spawn(move || {
                    if let Some(signal) = iterator.forever().next() {
                        state.restore();
                        let _ = signal_hook::low_level::emulate_default_handler(signal);
                    }
                })?;
            guard.signals = Some((handle, worker));
        }
        #[cfg(not(unix))]
        let _ = signals;
        guard.state.backend.write(START.as_bytes())?;
        Ok(guard)
    }

    /// Restore now; repeated calls and subsequent drop are harmless.
    pub fn restore(&self) {
        self.state.restore();
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
        #[cfg(unix)]
        if let Some((handle, worker)) = self.signals.take() {
            handle.close();
            let _ = worker.join();
        }
    }
}
