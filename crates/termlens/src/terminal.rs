//! [`Terminal`]: spawn a program in a real PTY, type into it, wait on its
//! rendered screen, resize it, and reap it.
//!
//! Architecture (see `docs/DESIGN.md`): a background reader thread drains
//! the PTY master into the emulator continuously, under a lock shared with
//! the test thread. Screens are immutable snapshots taken under that lock,
//! so no output is ever lost between two waits.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::emu::{Emulator, Vt100Emulator};
use crate::error::{Error, Result};
use crate::keys::Key;
use crate::screen::Screen;
use crate::wait::{next_backoff, Expired, Monitor, INITIAL_BACKOFF, POLL_CAP};

/// How long `wait_exit` keeps draining PTY output after the child has been
/// reaped, so the final screen is complete. Best effort: a grandchild
/// holding the PTY open must not stall the wait.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Serializes every PTY *lifecycle edge* (open+spawn on one side, kill+reap+
/// master-close on the other) across all `Terminal`s in this process.
///
/// Why: macOS tears PTYs down with `revoke()`, and PTY device numbers are
/// recycled immediately. With concurrent terminals, one thread's teardown
/// can race another thread's `openpty()` **on the same recycled device**,
/// and the late revoke hangs up the brand-new session — the fresh child
/// dies at birth (observed under stress as SIGHUP-style deaths, instant
/// EOF, and EIO on the first write, at roughly 1 in 800 spawns on loaded
/// macOS runners; Linux, whose teardown is not revoke-based, ran the same
/// suite 100/100). Holding this lock during both edges means the kernel
/// never sees the two windows overlap. Steady-state I/O is unaffected.
static PTY_LIFECYCLE: Mutex<()> = Mutex::new(());

fn pty_lifecycle_guard() -> std::sync::MutexGuard<'static, ()> {
    PTY_LIFECYCLE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Exit status of the child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    code: u32,
    success: bool,
    signal: Option<Box<str>>,
}

impl ExitStatus {
    fn from_pty(status: &portable_pty::ExitStatus) -> Self {
        Self {
            code: status.exit_code(),
            success: status.success(),
            signal: status.signal().map(Into::into),
        }
    }

    /// True if the child exited successfully (code 0, no fatal signal).
    #[must_use]
    pub fn success(&self) -> bool {
        self.success
    }

    /// The raw exit code as reported by the OS. Note that a signal-killed
    /// child has no real exit code — the OS reports a placeholder (1);
    /// check [`signal`](Self::signal) to tell the two cases apart.
    #[must_use]
    pub fn code(&self) -> u32 {
        self.code
    }

    /// The name of the signal that terminated the child, if it died from a
    /// signal (e.g. `"Hangup"`, `"Killed: 9"`). `None` for a normal exit.
    ///
    /// Distinguishing "the app exited 1" from "something killed the app" is
    /// the difference between a failing test and a failing test *harness* —
    /// always assert with the full status in the message, e.g.
    /// `assert_eq!(status.code(), 7, "status: {status}")`.
    #[must_use]
    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.signal {
            Some(signal) => write!(f, "killed by signal: {signal} (code {})", self.code),
            None => write!(f, "exit code {}", self.code),
        }
    }
}

/// Shared between the test thread and the PTY reader thread.
struct EmuState {
    emu: Box<dyn Emulator>,
    /// When the last byte arrived (or the terminal was spawned/resized).
    last_activity: Instant,
    /// Set once the PTY read side reaches EOF; nothing more can arrive.
    eof: bool,
    /// Bumped on every state change (bytes, EOF, resize). Lets waiters skip
    /// re-evaluation on spurious wakes, and keys the snapshot cache.
    generation: u64,
    /// The last snapshot built, valid while `generation` is unchanged.
    /// `Screen` is Arc-backed, so serving the cache is a cheap clone.
    snapshot_cache: Option<(u64, Screen)>,
    /// Completed synchronized updates (DEC 2026) observed so far.
    frames_seen: u64,
    /// The screen exactly as of the most recent completed frame.
    last_frame: Option<Screen>,
}

impl EmuState {
    fn new(emu: Box<dyn Emulator>) -> Self {
        Self {
            emu,
            last_activity: Instant::now(),
            eof: false,
            generation: 0,
            snapshot_cache: None,
            frames_seen: 0,
            last_frame: None,
        }
    }

    /// Record a state change: waiters must re-evaluate, snapshots rebuild.
    fn touch(&mut self) {
        self.last_activity = Instant::now();
        self.generation += 1;
    }

    /// Snapshot the screen and remember it, rebuilding only if the state
    /// changed since the last stored snapshot. An unchanged terminal costs
    /// one Arc clone per call instead of a full grid conversion.
    fn snapshot(&mut self) -> Screen {
        if let Some((generation, screen)) = &self.snapshot_cache {
            if *generation == self.generation {
                return screen.clone();
            }
        }
        let screen = self.emu.snapshot();
        self.snapshot_cache = Some((self.generation, screen.clone()));
        screen
    }

    /// Cache-aware read that never writes: serve the stored snapshot when
    /// current, else build a fresh one *without* storing it. The wait loop
    /// uses this — during a chatty stream every chunk advances the
    /// generation, so storing there would pay clone-and-evict costs on
    /// every chunk for a cache the next chunk invalidates (measured ~3% on
    /// a full-throughput stream).
    fn peek_snapshot(&self) -> Screen {
        if let Some((generation, screen)) = &self.snapshot_cache {
            if *generation == self.generation {
                return screen.clone();
            }
        }
        self.emu.snapshot()
    }
}

/// Configures and spawns a [`Terminal`].
///
/// ```
/// use std::time::Duration;
/// use termlens::Terminal;
///
/// # fn main() -> termlens::Result<()> {
/// let mut t = Terminal::builder()
///     .size(80, 24)
///     .timeout(Duration::from_secs(10))
///     .args(["-c", "echo builder-doc; read quit"])
///     .spawn("sh")?;
/// t.wait_until(|s| s.contains("builder-doc"))?;
/// # t.send(termlens::Key::Enter); // release `read quit`
/// # t.wait_exit()?; Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct TerminalBuilder {
    cols: u16,
    rows: u16,
    timeout: Duration,
    args: Vec<OsString>,
    env_clear: bool,
    envs: Vec<(OsString, OsString)>,
}

impl Default for TerminalBuilder {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            timeout: Duration::from_secs(5),
            args: Vec::new(),
            env_clear: false,
            envs: Vec::new(),
        }
    }
}

impl TerminalBuilder {
    /// Terminal size as columns × rows. Defaults to 80×24.
    #[must_use]
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Default deadline applied to **every** `wait_*` call. Defaults to 5s.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Append one argument for the spawned program.
    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Append several arguments for the spawned program.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// Set an environment variable for the child.
    ///
    /// Variables set here always reach the child, regardless of call order
    /// relative to [`env_clear`](Self::env_clear).
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// Don't inherit the parent process environment: the child sees only
    /// variables set via [`env`](Self::env) (plus the default `TERM`, see
    /// [`spawn`](Self::spawn)). Strict control keeps tests hermetic — a
    /// developer's exotic `LS_COLORS` should never change a snapshot.
    ///
    /// Note this differs from `std::process::Command::env_clear`, which also
    /// discards explicitly set variables; here `env()` entries survive.
    #[must_use]
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    /// Spawn `program` inside a fresh PTY and start draining its output.
    ///
    /// Unless a `TERM` variable was set explicitly, the child gets
    /// `TERM=xterm-256color` — matching the escape sequences the emulator
    /// speaks, and deterministic regardless of the host environment.
    ///
    /// # Errors
    ///
    /// [`Error::Pty`] when the PTY cannot be opened and [`Error::Spawn`]
    /// when the program cannot be executed.
    pub fn spawn(self, program: impl AsRef<OsStr>) -> Result<Terminal> {
        let program = program.as_ref();
        let command_desc = std::iter::once(program)
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");

        // Hold the lifecycle lock across openpty → spawn → slave close, so
        // no concurrent Terminal teardown can revoke our fresh PTY device.
        let lifecycle = pty_lifecycle_guard();

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: self.rows,
                cols: self.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Pty(format!("openpty failed: {e}")))?;

        let mut cmd = CommandBuilder::new(program);
        cmd.args(&self.args);
        if self.env_clear {
            cmd.env_clear();
        }
        if !self.envs.iter().any(|(k, _)| k == "TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        for (key, value) in &self.envs {
            cmd.env(key, value);
        }

        // Attach the reader thread BEFORE spawning the child: a program that
        // writes and exits within its first millisecond must find a drain
        // already running. (macOS's PTY layer can discard output still
        // buffered at teardown — see docs/DESIGN.md §2 — so the window
        // between child start and first read must be as close to zero as
        // userspace can make it.)
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Pty(format!("cloning PTY reader failed: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::Pty(format!("taking PTY writer failed: {e}")))?;

        let shared = Arc::new(Monitor::new(EmuState::new(Box::new(Vt100Emulator::new(
            self.rows, self.cols,
        )))));

        let reader_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("termlens-pty-reader".into())
            .spawn(move || reader_loop(reader, &reader_shared))
            .map_err(Error::Io)?;

        let child = pair.slave.spawn_command(cmd).map_err(|e| Error::Spawn {
            command: command_desc.clone(),
            reason: e.to_string(),
        })?;
        // Close the parent's slave handle so the master sees EOF once the
        // child (and its descendants) release the terminal.
        drop(pair.slave);
        drop(lifecycle);

        Ok(Terminal {
            child,
            master: Some(pair.master),
            writer: Some(writer),
            shared,
            default_timeout: self.timeout,
            exit_status: None,
            command_desc,
        })
    }
}

/// Drain the PTY into the emulator until EOF. Runs on a dedicated thread.
fn reader_loop(mut reader: Box<dyn Read + Send>, shared: &Monitor<EmuState>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => shared.mutate(|state| {
                // The emulator stops at each DEC 2026 frame end, so the
                // snapshot taken there is exactly the completed frame —
                // even when the same chunk already carries the next
                // frame's opening bytes.
                let mut offset = 0;
                while offset < n {
                    let processed = state.emu.process(&buf[offset..n]);
                    offset += processed.consumed;
                    if processed.frame_complete {
                        state.frames_seen += 1;
                        state.last_frame = Some(state.emu.snapshot());
                    }
                }
                state.touch();
            }),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // Linux reports EIO on the master once the child side is gone;
            // treat any hard error as end-of-stream.
            Err(_) => break,
        }
    }
    shared.mutate(|state| {
        state.eof = true;
        state.touch();
    });
}

/// A program running inside a real PTY, observed through an emulated screen.
///
/// See the crate-level docs for a full example. Dropping a `Terminal` kills
/// and reaps the child — tests never leak zombies, even on panic.
pub struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // Option only so Drop can close them under the PTY lifecycle lock;
    // both are Some for the entire life of the value outside Drop.
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    shared: Arc<Monitor<EmuState>>,
    default_timeout: Duration,
    exit_status: Option<ExitStatus>,
    command_desc: String,
}

impl Terminal {
    /// Start configuring a terminal. See [`TerminalBuilder`].
    #[must_use]
    pub fn builder() -> TerminalBuilder {
        TerminalBuilder::default()
    }

    /// Snapshot the current screen.
    ///
    /// Taken under the reader lock: the snapshot is a consistent view of
    /// everything the child had written up to this instant.
    #[must_use]
    pub fn screen(&self) -> Screen {
        self.shared.lock().snapshot()
    }

    /// Send one key press. See [`Key`] for the encodings.
    ///
    /// # Panics
    ///
    /// Panics if the bytes cannot be written to the PTY (e.g. the child
    /// exited and the OS tore the terminal down); the panic message includes
    /// the current screen. A test that types into a dead program is broken —
    /// failing loudly beats a silent no-op.
    pub fn send(&mut self, key: Key) {
        self.write_or_panic(&key.encode(), &format!("{key:?}"));
    }

    /// Send a string literally (UTF-8 bytes, no key mapping, no newline).
    ///
    /// # Panics
    ///
    /// Same contract as [`send`](Self::send).
    pub fn send_str(&mut self, s: &str) {
        self.write_or_panic(s.as_bytes(), "literal text");
    }

    fn write_or_panic(&mut self, bytes: &[u8], what: &str) {
        let writer = self.writer.as_mut().expect("writer lives until drop");
        if let Err(e) = writer.write_all(bytes).and_then(|()| writer.flush()) {
            panic!(
                "termlens: failed to send {what} to `{}` ({e})\n--- screen ---\n{}",
                self.command_desc,
                self.screen()
            );
        }
    }

    /// Block until `predicate` holds on the screen.
    ///
    /// The predicate is re-evaluated whenever new output arrives. Fails with
    /// [`Error::Timeout`] at the deadline (builder `timeout`), or
    /// [`Error::Eof`] as soon as the PTY closes with the predicate still
    /// false — both embed the screen for debugging.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] / [`Error::Eof`], each carrying the screen.
    pub fn wait_until(&mut self, mut predicate: impl FnMut(&Screen) -> bool) -> Result<()> {
        const WHAT: &str = "the screen predicate to hold";
        let deadline = Instant::now() + self.default_timeout;
        let mut seen_generation = None;
        let outcome = self.shared.wait_until(deadline, |state| {
            // Spurious wake (poll-cap tick, unrelated notify): the state is
            // unchanged, so the predicate's verdict is too.
            if seen_generation == Some(state.generation) {
                return None;
            }
            seen_generation = Some(state.generation);

            let screen = state.peek_snapshot();
            if predicate(&screen) {
                return Some(Ok(()));
            }
            if state.eof {
                return Some(Err(Error::Eof {
                    waiting_for: WHAT.into(),
                    screen,
                }));
            }
            None
        });
        match outcome {
            Ok(inner) => inner,
            Err(Expired) => Err(Error::Timeout {
                waiting_for: WHAT.into(),
                timeout: self.default_timeout,
                screen: self.screen(),
            }),
        }
    }

    /// Block until a **complete frame** satisfies `predicate`.
    ///
    /// For applications that bracket repaints in DEC 2026 synchronized
    /// updates (`BeginSynchronizedUpdate` / `EndSynchronizedUpdate` in
    /// crossterm), the predicate is evaluated only on screens exactly as
    /// they stood when an update ended — never on a torn, half-painted
    /// frame. This removes the discipline [`wait_until`](Self::wait_until)
    /// demands (single predicate, wait on the last-painted region; see
    /// `docs/DESIGN.md` §2).
    ///
    /// The frame completed most recently *before* the call is evaluated
    /// first, so a fast application cannot slip a frame past you. Each
    /// frame is evaluated at most once; if several frames complete within
    /// one read burst, only the newest is seen — `wait_frame` guarantees
    /// frame-consistent screens, not observation of every transient frame.
    ///
    /// ```
    /// # fn main() -> termlens::Result<()> {
    /// let mut t = termlens::Terminal::builder()
    ///     .timeout(std::time::Duration::from_secs(10))
    ///     .args(["-c", r"printf '\033[?2026hFrame ready\033[?2026l'; read quit"])
    ///     .spawn("sh")?;
    /// t.wait_frame(|screen| screen.contains("Frame ready"))?;
    /// # t.send(termlens::Key::Enter); t.wait_exit()?; Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] at the deadline — with a pointed message when the
    /// application never emitted a single synchronized update, since
    /// `wait_frame` can then never succeed; use `wait_until` for such apps.
    /// [`Error::Eof`] as soon as the PTY closes with no matching frame.
    pub fn wait_frame(&mut self, mut predicate: impl FnMut(&Screen) -> bool) -> Result<()> {
        const WHAT: &str = "a complete frame matching the predicate";
        let deadline = Instant::now() + self.default_timeout;
        let mut seen_frame = None;
        let outcome = self.shared.wait_until(deadline, |state| {
            if state.frames_seen > 0 && seen_frame != Some(state.frames_seen) {
                seen_frame = Some(state.frames_seen);
                let frame = state
                    .last_frame
                    .clone()
                    .expect("frames_seen > 0 implies a stored frame");
                if predicate(&frame) {
                    return Some(Ok(()));
                }
            }
            if state.eof {
                return Some(Err(Error::Eof {
                    waiting_for: WHAT.into(),
                    screen: state.peek_snapshot(),
                }));
            }
            None
        });
        match outcome {
            Ok(inner) => inner,
            Err(Expired) => {
                let (frames, screen) = {
                    let mut guard = self.shared.lock();
                    let screen = guard.last_frame.clone().unwrap_or_else(|| guard.snapshot());
                    (guard.frames_seen, screen)
                };
                let waiting_for = if frames == 0 {
                    "a complete frame — but the application never emitted a \
                     DEC 2026 synchronized update. wait_frame needs repaints \
                     bracketed in BeginSynchronizedUpdate/EndSynchronizedUpdate; \
                     for other apps use wait_until (docs/DESIGN.md §2)"
                        .to_owned()
                } else {
                    format!("{WHAT} ({frames} complete frames observed)")
                };
                Err(Error::Timeout {
                    waiting_for,
                    timeout: self.default_timeout,
                    screen,
                })
            }
        }
    }

    /// Block until the terminal has been quiet — no bytes for `quiet` and
    /// the stream not ending mid-escape-sequence. EOF counts as idle
    /// (nothing more can arrive).
    ///
    /// This is a heuristic: "no output for N ms" is evidence, not proof,
    /// that the application finished rendering. Prefer
    /// [`wait_until`](Self::wait_until) on visible content where possible.
    /// `docs/DESIGN.md` discusses the trade-off and the planned frame-sync
    /// alternative.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] when the overall deadline (builder `timeout`)
    /// expires first — e.g. when `quiet` exceeds the timeout, or the child
    /// keeps chattering.
    pub fn wait_idle(&mut self, quiet: Duration) -> Result<()> {
        let deadline = Instant::now() + self.default_timeout;
        let mut guard = self.shared.lock();
        loop {
            if guard.eof {
                return Ok(());
            }
            let elapsed = guard.last_activity.elapsed();
            if elapsed >= quiet && !guard.emu.mid_sequence() && !guard.emu.in_sync_update() {
                return Ok(());
            }

            let now = Instant::now();
            if now >= deadline {
                let screen = guard.peek_snapshot();
                drop(guard);
                return Err(Error::Timeout {
                    waiting_for: format!("{quiet:?} of output silence"),
                    timeout: self.default_timeout,
                    screen,
                });
            }
            // Sleep until the quiet period could complete, the deadline
            // hits, or new bytes arrive (notification) — whichever first.
            // When we're only waiting out a mid-sequence stall, poll-cap.
            let sleep = if elapsed < quiet {
                quiet - elapsed
            } else {
                POLL_CAP
            }
            .min(deadline - now)
            .max(Duration::from_millis(1));
            guard = self.shared.wait_timeout(guard, sleep);
        }
    }

    /// Block until the child exits, then return its status. Idempotent:
    /// after the first success the cached status is returned.
    ///
    /// After reaping, briefly (≤500ms) waits for the PTY to reach EOF so the
    /// final screen is complete — best effort, in case descendants keep the
    /// terminal open.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] (with screen) if the child is still running at the
    /// deadline; [`Error::Io`] if the OS wait itself fails.
    pub fn wait_exit(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.exit_status.clone() {
            return Ok(status);
        }
        let deadline = Instant::now() + self.default_timeout;
        let mut backoff = INITIAL_BACKOFF;
        loop {
            if let Some(status) = self.child.try_wait().map_err(Error::Io)? {
                let status = ExitStatus::from_pty(&status);
                self.exit_status = Some(status.clone());
                let _ = self
                    .shared
                    .wait_until(Instant::now() + DRAIN_GRACE, |state| {
                        state.eof.then_some(())
                    });
                return Ok(status);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Timeout {
                    waiting_for: format!("`{}` to exit", self.command_desc),
                    timeout: self.default_timeout,
                    screen: self.screen(),
                });
            }
            thread::sleep(backoff.min(deadline - now));
            backoff = next_backoff(backoff);
        }
    }

    /// Resize the PTY (TIOCSWINSZ — the kernel delivers SIGWINCH to the
    /// child) and the emulated grid, atomically from the observer's side.
    ///
    /// # Errors
    ///
    /// [`Error::Pty`] if the ioctl fails.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .as_ref()
            .expect("master lives until drop")
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Pty(format!("resize failed: {e}")))?;
        self.shared.mutate(|state| {
            state.emu.set_size(rows, cols);
            state.touch();
        });
        Ok(())
    }
}

impl fmt::Debug for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Terminal")
            .field("command", &self.command_desc)
            .field("default_timeout", &self.default_timeout)
            .field("exit_status", &self.exit_status)
            .finish_non_exhaustive()
    }
}

impl Drop for Terminal {
    /// Kill and reap the child. No zombies, even when a test panics before
    /// `wait_exit`. The reader thread ends on its own at EOF and is never
    /// joined here — a grandchild holding the PTY open must not hang Drop.
    ///
    /// The whole teardown (including closing the master/writer fds) runs
    /// under the process-wide PTY lifecycle lock: on macOS, letting a
    /// master close overlap a concurrent `openpty()` can revoke the *other*
    /// terminal's freshly recycled PTY device (see `PTY_LIFECYCLE` in this module).
    fn drop(&mut self) {
        let _lifecycle = pty_lifecycle_guard();
        if self.exit_status.is_none() {
            let already_exited = matches!(self.child.try_wait(), Ok(Some(_)));
            if !already_exited {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        // Close the PTY fds while still holding the lock.
        drop(self.writer.take());
        drop(self.master.take());
    }
}
