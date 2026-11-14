//! RAII terminal ownership with unwind and platform termination cleanup.

use super::capabilities::Capabilities;

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, Weak};

/// Startup control sequence; never enters an alternate screen or enables a mouse.
pub const START: &str = "\x1b[2J\x1b[3J\x1b[H\x1b[?25l\x1b[?2004h";
/// Clear the visible terminal and its scrollback, then home the cursor.
pub const CLEAR_SCREEN: &str = "\x1b[2J\x1b[3J\x1b[H";
/// Reset synchronized output, Kitty keyboard stack, paste, style, and cursor.
pub const RESTORE: &str =
    "\x1b[?1002l\x1b[?1006l\x1b[?2026l\x1b[<u\x1b[?2004l\x1b[0m\x1b[?25h\x1b[2J\x1b[3J\x1b[H";

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
        crate::platform::prepare_terminal()?;
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
    synchronized_output: bool,
    keyboard_pushed: AtomicBool,
}
impl State {
    fn restore(&self) {
        if self.active.swap(false, Ordering::SeqCst) {
            // Raw restoration must still run when a disconnected output rejects writes.
            // The viewport enables mouse reporting independently of keyboard
            // capability negotiation, so always restore it on exit.
            let mut restore = String::from("\x1b[?1002l\x1b[?1006l");
            if self.synchronized_output {
                restore.push_str("\x1b[?2026l");
            }
            if self.keyboard_pushed.load(Ordering::SeqCst) {
                restore.push_str("\x1b[<u");
            }
            restore.push_str("\x1b[?2004l\x1b[0m\x1b[?25h\x1b[2J\x1b[3J\x1b[H");
            let _ = self.backend.write(restore.as_bytes());
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
    #[cfg(windows)]
    console_events: Option<crate::platform::ConsoleEvents>,
}
impl TerminalGuard {
    /// Enter on a real TTY, refusing to emit escape sequences into redirected output.
    pub fn enter() -> io::Result<Self> {
        Self::enter_with_capabilities(Capabilities::detect(true))
    }

    /// Enter with the same detected extension policy used by the renderer.
    pub fn enter_with_capabilities(capabilities: Capabilities) -> io::Result<Self> {
        if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "interactive terminal required",
            ));
        }
        Self::with_backend_and_capabilities(Arc::new(SystemTerminal), true, capabilities)
    }

    /// Legacy full-state injected fixture; signal handling is opt-in for isolation.
    /// Assumes an existing keyboard push, preserving deterministic restore-byte tests.
    /// Runtime callers should use `enter_with_capabilities` instead.
    pub fn with_backend(backend: Arc<dyn TerminalBackend>, signals: bool) -> io::Result<Self> {
        Self::with_backend_policy(backend, signals, true, true)
    }

    /// Injectable policy-aware entry; pop only after a successful keyboard push.
    pub fn with_backend_and_capabilities(
        backend: Arc<dyn TerminalBackend>,
        signals: bool,
        capabilities: Capabilities,
    ) -> io::Result<Self> {
        let guard =
            Self::with_backend_policy(backend, signals, capabilities.synchronized_output, false)?;
        struct BackendWriter<'a>(&'a dyn TerminalBackend);
        impl Write for BackendWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.write(bytes)?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        capabilities.enable_keyboard(&mut BackendWriter(guard.state.backend.as_ref()))?;
        guard.state.keyboard_pushed.store(
            capabilities.kitty_keyboard && !cfg!(windows),
            Ordering::SeqCst,
        );
        Ok(guard)
    }

    fn with_backend_policy(
        backend: Arc<dyn TerminalBackend>,
        signals: bool,
        synchronized_output: bool,
        keyboard_pushed: bool,
    ) -> io::Result<Self> {
        backend.raw()?;
        let state = Arc::new(State {
            backend,
            active: AtomicBool::new(true),
            synchronized_output,
            keyboard_pushed: AtomicBool::new(keyboard_pushed),
        });
        register(&state);
        #[allow(unused_mut)]
        let mut guard = Self {
            state,
            #[cfg(unix)]
            signals: None,
            #[cfg(windows)]
            console_events: None,
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
        #[cfg(windows)]
        if signals {
            let state = Arc::clone(&guard.state);
            guard.console_events = Some(crate::platform::ConsoleEvents::listen(move || {
                state.restore();
            })?);
        }
        #[cfg(not(any(unix, windows)))]
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
