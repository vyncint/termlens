//! [`Terminal`]: spawn a program in a real PTY, type into it, wait on its
//! rendered screen, resize it, and reap it.
//!
//! Architecture (see `docs/DESIGN.md`): a background reader thread drains
//! the PTY master into the emulator continuously, under a lock shared with
//! the test thread. Screens are immutable snapshots taken under that lock,
//! so no output is ever lost between two waits.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::emu::{Emulator, InputModes, MouseEncoding, Query, Stop, Vt100Emulator};
use crate::error::{Error, Result};
use crate::keys::Input;
use crate::keys::{mouse_legacy, mouse_sgr, mouse_utf8};
use crate::screen::{MouseMode, Screen};
use crate::wait::{next_backoff, Expired, Monitor, INITIAL_BACKOFF, POLL_CAP};

/// How long `wait_exit` keeps draining PTY output after the child has been
/// reaped, so the final screen is complete. Best effort: a grandchild
/// holding the PTY open must not stall the wait.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// How many distinct unanswered query shapes a terminal remembers for its
/// diagnostics. The set is filled by the application under test — every
/// distinct `CSI … n` is its own shape — so it is bounded; anything
/// beyond this is counted rather than kept.
const MAX_UNANSWERED: usize = 8;

/// How many completed frames a terminal retains for [`wait_frame`].
///
/// Several frames can complete inside a single read, and only retained
/// frames are observable — so the bound is the bound on how much of a
/// burst a test can assert on. Eight covers realistic bursts (a repaint
/// is rarely under a hundred bytes, and a read is 8 KiB) while keeping
/// the worst case small: a retained frame is a full grid snapshot, about
/// 75 KB at 80×24 and 470 KB at 200×60, so eight of them cost roughly
/// 0.6 MB and 3.8 MB respectively.
const FRAME_HISTORY: usize = 8;

/// Writes that may be queued for the writer thread before the drain
/// starts discarding query replies. Reached only when the application
/// has stopped reading its input entirely, in which case it cannot be
/// waiting on those bytes.
const REPLY_QUEUE_DEPTH: usize = 64;

/// One write handed to the writer thread.
///
/// Query replies are fire-and-forget; typed input carries an
/// acknowledgement channel so the calling thread can apply a deadline
/// and fail loudly instead of blocking forever inside `write(2)`.
struct WriteRequest {
    bytes: Vec<u8>,
    ack: Option<mpsc::SyncSender<io::Result<()>>>,
}

/// How long `Drop` will wait for a killed child to be reaped before
/// giving up. Teardown must terminate: a stuck child is a bad outcome, a
/// test binary that never exits is a worse one.
const DROP_REAP_GRACE: Duration = Duration::from_secs(2);

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

/// The PTY writer, shared between `Terminal` (typed input) and the reader
/// thread (query replies). `None` after teardown. Locked briefly per write;
/// never while the emulator state lock is held.
type SharedWriter = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// Open a second writer onto the same PTY master, for the responder
/// thread. `take_writer` may only be called once, so duplicate the
/// descriptor instead: both refer to the same open file description,
/// which is what we want — same terminal, independent blocking.
#[cfg(unix)]
fn dup_writer(master: &dyn portable_pty::MasterPty) -> Option<std::fs::File> {
    use std::os::unix::io::FromRawFd;

    let fd = master.as_raw_fd()?;
    // SAFETY: `fd` is the live master descriptor (the caller still owns
    // the master). dup(2) returns a fresh descriptor we take sole
    // ownership of; `File` closes it exactly once on drop.
    #[allow(unsafe_code)]
    let duped = unsafe { libc::dup(fd) };
    if duped < 0 {
        return None;
    }
    #[allow(unsafe_code)]
    Some(unsafe { std::fs::File::from_raw_fd(duped) })
}

#[cfg(not(unix))]
fn dup_writer(_master: &dyn portable_pty::MasterPty) -> Option<std::fs::File> {
    None
}

/// Scroll-wheel direction for [`Terminal::scroll`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Wheel up (away from the user).
    Up,
    /// Wheel down (toward the user).
    Down,
    /// Horizontal wheel left (or a trackpad swipe).
    Left,
    /// Horizontal wheel right.
    Right,
}

/// A mouse button, for [`Terminal::click_with`] and [`Terminal::drag`].
///
/// Add modifiers the same way [`Key`](crate::Key) does:
/// `MouseButton::Left.ctrl()`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// The primary button — what [`Terminal::click`] sends.
    Left,
    /// The middle button (wheel click).
    Middle,
    /// The secondary button, conventionally a context menu.
    Right,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }

    /// This button held with `Ctrl` — the usual multi-select idiom.
    #[must_use]
    pub fn ctrl(self) -> MouseChord {
        MouseChord::from(self).ctrl()
    }

    /// This button held with `Alt`.
    #[must_use]
    pub fn alt(self) -> MouseChord {
        MouseChord::from(self).alt()
    }

    /// This button held with `Shift`.
    #[must_use]
    pub fn shift(self) -> MouseChord {
        MouseChord::from(self).shift()
    }
}

/// A mouse button plus modifier keys — `MouseButton::Left.ctrl()`.
///
/// Mirrors the [`Chord`](crate::Chord) builder for keys; anything taking
/// `impl Into<MouseChord>` accepts a bare [`MouseButton`] too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseChord {
    button: MouseButton,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl From<MouseButton> for MouseChord {
    fn from(button: MouseButton) -> Self {
        Self {
            button,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
}

impl MouseChord {
    /// Add `Ctrl`.
    #[must_use]
    pub fn ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    /// Add `Alt`.
    #[must_use]
    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }

    /// Add `Shift`.
    #[must_use]
    pub fn shift(mut self) -> Self {
        self.shift = true;
        self
    }

    /// The xterm button code: the button, plus 4 for shift, 8 for alt
    /// and 16 for control.
    fn code(self) -> u8 {
        self.button.code()
            + 4 * u8::from(self.shift)
            + 8 * u8::from(self.alt)
            + 16 * u8::from(self.ctrl)
    }
}

/// The error every mouse action shares: bytes the application never
/// asked for would be misparsed as keys.
fn no_mouse_tracking() -> Error {
    Error::Input(
        "the application has not enabled mouse tracking \
         (no CSI ?9/?1000/?1002/?1003 h was seen)"
            .into(),
    )
}

/// A POSIX signal for [`Terminal::signal`]: the graceful-shutdown set.
///
/// Note the difference from typing: `send(Key::Ctrl('c'))` writes the
/// `0x03` byte *through the PTY* (an app in raw mode reads it; in cooked
/// mode the line discipline turns it into `SIGINT`), while
/// `signal(Signal::Int)` delivers the signal directly via `kill(2)`,
/// bypassing the terminal entirely.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    /// `SIGINT` — interactive interrupt (what Ctrl-C means).
    Int,
    /// `SIGTERM` — the polite termination request.
    Term,
    /// `SIGHUP` — the controlling terminal hung up.
    Hup,
    /// `SIGQUIT` — quit (often with a core dump).
    Quit,
    /// `SIGUSR1` — user-defined; commonly "reload" or "toggle".
    Usr1,
    /// `SIGUSR2` — user-defined.
    Usr2,
    /// `SIGKILL` — uncatchable. Prefer letting `Drop` clean up; send this
    /// only to test how your supervisor reacts to a hard kill.
    Kill,
}

#[cfg(unix)]
impl Signal {
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Int => libc::SIGINT,
            Signal::Term => libc::SIGTERM,
            Signal::Hup => libc::SIGHUP,
            Signal::Quit => libc::SIGQUIT,
            Signal::Usr1 => libc::SIGUSR1,
            Signal::Usr2 => libc::SIGUSR2,
            Signal::Kill => libc::SIGKILL,
        }
    }
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
    /// The most recent completed frames, oldest first, capped at
    /// [`FRAME_HISTORY`]. Several frames can complete inside one read, so
    /// keeping only the newest made rapid intermediate states — a
    /// progress counter ticking 1, 2, 3 in one write — unobservable.
    frames: VecDeque<Screen>,
    /// Whether to answer recognized terminal queries (builder-configured).
    respond: bool,
    /// Background color reported to OSC 11 queries.
    background: (u8, u8, u8),
    foreground: (u8, u8, u8),
    /// Distinct queries that went unanswered, in first-seen order, each
    /// with the read it most recently arrived in — timeout errors surface
    /// these so a blocked probe is diagnosable.
    unanswered: Vec<(String, u64)>,
    /// Distinct unanswered shapes beyond `MAX_UNANSWERED`, counted only.
    unanswered_overflow: usize,
    /// Query replies the reader could not deliver because the child was
    /// not reading its input — evidence for the diagnosis, not an error.
    replies_dropped: usize,
    /// Reads delivered by the PTY so far. A query last seen in an earlier
    /// read than this cannot be what the application is blocked on: it
    /// produced output after asking.
    reads: u64,
}

impl EmuState {
    fn new(
        emu: Box<dyn Emulator>,
        respond: bool,
        background: (u8, u8, u8),
        foreground: (u8, u8, u8),
    ) -> Self {
        Self {
            emu,
            last_activity: Instant::now(),
            eof: false,
            generation: 0,
            snapshot_cache: None,
            frames_seen: 0,
            frames: VecDeque::with_capacity(FRAME_HISTORY),
            respond,
            background,
            foreground,
            unanswered: Vec::new(),
            unanswered_overflow: 0,
            replies_dropped: 0,
            reads: 0,
        }
    }

    /// Record a query we did not answer, keyed on its printable shape.
    ///
    /// Bounded: the set is application-controlled (every distinct
    /// `CSI … n` is its own shape), so a probing application must not be
    /// able to grow it without limit — the same reasoning as
    /// `OSC_CAPTURE_MAX` in `emu/seq.rs`.
    fn note_unanswered(&mut self, shape: String) {
        let reads = self.reads;
        if let Some(entry) = self.unanswered.iter_mut().find(|(seen, _)| *seen == shape) {
            // Asked again: judge it by the most recent occurrence.
            entry.1 = reads;
        } else if self.unanswered.len() < MAX_UNANSWERED {
            self.unanswered.push((shape, reads));
        } else {
            self.unanswered_overflow += 1;
        }
    }

    /// Build the reply for a query, or record it as unanswered. Pure
    /// computation under the state lock; the caller does the writing.
    fn answer(&mut self, query: &Query) -> Option<Vec<u8>> {
        fn osc_color(code: u8, (r, g, b): (u8, u8, u8), st: bool) -> Vec<u8> {
            let widen = |v: u8| u16::from(v) << 8 | u16::from(v);
            let terminator = if st { "\x1b\\" } else { "\x07" };
            format!(
                "\x1b]{code};rgb:{:04x}/{:04x}/{:04x}{terminator}",
                widen(r),
                widen(g),
                widen(b)
            )
            .into_bytes()
        }

        if !self.respond {
            self.note_unanswered(query_shape(query));
            return None;
        }
        let reply = match query {
            Query::CursorPosition { private } => {
                // Report exactly the cursor as of the query byte: the
                // emulator stopped there, so this snapshot cannot include
                // later output. 1-based on the wire.
                let (row, col, _) = self.emu.snapshot().cursor();
                let prefix = if *private { "?" } else { "" };
                format!("\x1b[{prefix}{};{}R", row + 1, col + 1).into_bytes()
            }
            Query::OperatingStatus => b"\x1b[0n".to_vec(),
            // VT220 with ANSI color: honest — nothing claimed (sixel,
            // kitty, …) that the emulator cannot render.
            Query::PrimaryDa => b"\x1b[?62;22c".to_vec(),
            Query::SecondaryDa => b"\x1b[>1;10;0c".to_vec(),
            Query::TextAreaSize => {
                let screen = self.emu.snapshot();
                format!("\x1b[8;{};{}t", screen.rows(), screen.cols()).into_bytes()
            }
            Query::OscColor {
                code: 11,
                st_terminated,
            } => osc_color(11, self.background, *st_terminated),
            Query::OscColor {
                code,
                st_terminated,
            } => osc_color(*code, self.foreground, *st_terminated),
            Query::RequestMode(mode) => {
                // DECRPM. Reporting 0 ("not recognized") for anything we
                // do not track exactly is deliberate — see
                // `Emulator::mode_state`.
                let value = self.emu.mode_state(*mode).report_value();
                format!("\x1b[?{mode};{value}$y").into_bytes()
            }
            Query::Unanswerable(shape) => {
                self.note_unanswered(shape.clone());
                return None;
            }
        };
        Some(reply)
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

    /// One-line diagnosis when a query went unanswered, appended to every
    /// wait's error — a probing app blocked on a reply is otherwise
    /// indistinguishable from a hung one.
    ///
    /// Causation is claimed only for queries the application has *not*
    /// visibly moved past. If output arrived in a later read than the
    /// query, the application asked and carried on, so the query is
    /// reported as context instead. Counting reads rather than bytes is
    /// deliberate: the emulator stops at the query byte and processes the
    /// rest of the same chunk, so output the application batched into the
    /// same `write` as its probe is not evidence of progress.
    fn query_note(&self) -> String {
        let backlog = if self.replies_dropped > 0 {
            format!(
                " — note: the application is not reading its input \
                 ({} terminal replies could not be delivered)",
                self.replies_dropped
            )
        } else {
            String::new()
        };
        if self.unanswered.is_empty() {
            return backlog;
        }
        let mut blocking: Vec<&str> = Vec::new();
        let mut moved_past: Vec<&str> = Vec::new();
        for (shape, seen_at) in &self.unanswered {
            if *seen_at == self.reads {
                blocking.push(shape);
            } else {
                moved_past.push(shape);
            }
        }
        let more = if self.unanswered_overflow > 0 {
            format!(", and {} more", self.unanswered_overflow)
        } else {
            String::new()
        };
        if blocking.is_empty() {
            format!(
                "{backlog} — note: the application queried the terminal \
                 ({}{more}) and received no answer, but produced output \
                 afterwards, so that is probably not why this wait failed",
                moved_past.join(", ")
            )
        } else {
            format!(
                "{backlog} — note: the application queried the terminal \
                 ({}{more}) and received no answer; if it is blocked waiting \
                 for that reply, this is the cause",
                blocking.join(", ")
            )
        }
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
    cwd: Option<PathBuf>,
    answer_queries: bool,
    background: (u8, u8, u8),
    foreground: (u8, u8, u8),
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
            cwd: None,
            answer_queries: true,
            background: (0, 0, 0),
            foreground: (0xff, 0xff, 0xff),
        }
    }
}

impl TerminalBuilder {
    /// Terminal size as columns × rows. Defaults to 80×24.
    ///
    /// Both dimensions must be non-zero; [`spawn`](Self::spawn) rejects a
    /// zero with [`Error::Input`] rather than letting the emulator meet a
    /// size no terminal can have.
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

    /// Run the program with `dir` as its working directory instead of
    /// inheriting the test runner's. Directory-sensitive programs no
    /// longer need a `cd … && …` through a shell.
    ///
    /// [`spawn`](Self::spawn) fails with [`Error::Spawn`] if `dir` is not
    /// an existing directory — running somewhere else instead would make
    /// a directory-sensitive test pass against the wrong tree.
    #[must_use]
    pub fn current_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Answer terminal queries from the application (on by default).
    ///
    /// Real terminals answer questions like `CSI 6 n` (cursor position),
    /// `CSI c` (device attributes) and `OSC 11 ; ?` (background color) —
    /// so does termlens, because an application blocked on a probe reply
    /// would otherwise hang until the test times out. Disable only to
    /// test how your app behaves against a mute terminal; unanswered
    /// queries are then named inside wait-timeout errors.
    #[must_use]
    pub fn answer_queries(mut self, answer: bool) -> Self {
        self.answer_queries = answer;
        self
    }

    /// The background color reported to `OSC 11` queries (light/dark
    /// detection). Defaults to black.
    #[must_use]
    pub fn background_rgb(mut self, r: u8, g: u8, b: u8) -> Self {
        self.background = (r, g, b);
        self
    }

    /// The foreground color reported to `OSC 10` queries. Defaults to
    /// white.
    ///
    /// Applications that choose a theme by comparing foreground and
    /// background luminance need both ends configurable; with only
    /// [`background_rgb`](Self::background_rgb) they always saw
    /// white-on-*your-background*, so one branch of that logic could
    /// never be exercised.
    #[must_use]
    pub fn foreground_rgb(mut self, r: u8, g: u8, b: u8) -> Self {
        self.foreground = (r, g, b);
        self
    }

    /// Reject configurations that cannot produce a working terminal.
    ///
    /// These are all programming errors in the test, and each has a
    /// failure mode elsewhere that is far harder to read than a typed
    /// error here: an empty program name becomes a page of PATH search
    /// output from the PTY layer, and a missing working directory becomes
    /// no error at all — the child just runs somewhere else.
    fn validate(&self, command_desc: &str, program: &OsStr) -> Result<()> {
        let spawn_err = |reason: String| {
            Err(Error::Spawn {
                command: command_desc.to_owned(),
                reason,
            })
        };
        if program.is_empty() {
            return spawn_err("no program name given (the program argument was empty)".into());
        }
        check_size(self.cols, self.rows)?;
        // portable-pty filters the cwd through is_dir() and silently falls
        // back to the home directory. Sensible for a terminal emulator,
        // which must always open somewhere; wrong for a test harness,
        // where "run it there" is part of the assertion.
        if let Some(dir) = &self.cwd {
            if !dir.is_dir() {
                return spawn_err(format!(
                    "current_dir({}) is not an existing directory",
                    dir.display()
                ));
            }
        }
        Ok(())
    }

    /// Spawn `program` inside a fresh PTY and start draining its output.
    ///
    /// Unless a `TERM` variable was set explicitly, the child gets
    /// `TERM=xterm-256color` — matching the escape sequences the emulator
    /// speaks, and deterministic regardless of the host environment.
    ///
    /// # Errors
    ///
    /// [`Error::Spawn`] when the configuration cannot produce a runnable
    /// command (empty program name, missing
    /// [`current_dir`](Self::current_dir)) or the program cannot be
    /// executed; [`Error::Input`] when the configured
    /// [`size`](Self::size) has a zero dimension; [`Error::Pty`] when the
    /// PTY cannot be opened.
    pub fn spawn(self, program: impl AsRef<OsStr>) -> Result<Terminal> {
        let program = program.as_ref();
        let command_desc = std::iter::once(program)
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");

        // Validate the configuration before opening anything: a bad
        // builder should fail on its own terms, not as a PTY-layer
        // diagnostic about something else.
        self.validate(&command_desc, program)?;

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
        if let Some(dir) = &self.cwd {
            cmd.cwd(dir);
        }
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

        let shared = Arc::new(Monitor::new(EmuState::new(
            Box::new(Vt100Emulator::new(self.rows, self.cols)),
            self.answer_queries,
            self.background,
            self.foreground,
        )));
        let writer: SharedWriter = Arc::new(Mutex::new(Some(writer)));

        // Everything written *to* the child goes through a thread of its
        // own: query replies from the drain, and typed input from the
        // test. A write into the PTY blocks whenever the application has
        // stopped reading, and neither caller can afford that — the drain
        // would deadlock the harness (the child then blocks writing into
        // a full output buffer), and the test thread would hang with no
        // deadline and no diagnosis.
        let write_tx = match dup_writer(pair.master.as_ref()) {
            Some(mut pty_writer) => {
                let (tx, rx) = mpsc::sync_channel::<WriteRequest>(REPLY_QUEUE_DEPTH);
                thread::Builder::new()
                    .name("termlens-pty-writer".into())
                    .spawn(move || {
                        while let Ok(request) = rx.recv() {
                            let result = pty_writer
                                .write_all(&request.bytes)
                                .and_then(|()| pty_writer.flush());
                            let failed = result.is_err();
                            if let Some(ack) = request.ack {
                                // The caller may have given up already.
                                let _ = ack.send(result);
                            }
                            if failed {
                                break; // terminal gone; nothing left to write
                            }
                        }
                    })
                    .map_err(Error::Io)?;
                Some(tx)
            }
            // No descriptor to duplicate: fall back to writing inline,
            // as before.
            None => None,
        };

        let reader_shared = Arc::clone(&shared);
        let reader_writer = Arc::clone(&writer);
        let reader_tx = write_tx.clone();
        thread::Builder::new()
            .name("termlens-pty-reader".into())
            .spawn(move || reader_loop(reader, &reader_shared, &reader_writer, reader_tx.as_ref()))
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
            writer,
            write_tx,
            shared,
            default_timeout: self.timeout,
            exit_status: None,
            command_desc,
        })
    }
}

/// Remove bracketed-paste markers from pasted text.
///
/// Repeated to a fixed point, because a single pass is trivially
/// defeated by a marker that reassembles from the remains of another
/// (`ESC[2ESC[201~01~`) — the same rule kitty applies.
fn strip_paste_markers(text: &str) -> String {
    let mut out = text.to_owned();
    loop {
        let stripped = out.replace("\x1b[200~", "").replace("\x1b[201~", "");
        if stripped == out {
            return out;
        }
        out = stripped;
    }
}

/// Reject a zero terminal dimension.
///
/// A terminal has at least one row and one column. Letting a zero reach
/// the emulator is not a graceful degradation: in debug builds vt100
/// panics on an overflowing subtraction, and in release builds (the
/// stress workflow runs `--release`) the arithmetic wraps, `spawn` and
/// `resize` return `Ok`, and vt100 panics on the first printable byte —
/// on the reader thread, where nothing propagates it. The drain dies
/// silently, every later snapshot is blank, and a careless test goes
/// green. Zeroes arrive from ordinary arithmetic (`resize(cols - 1, …)`
/// in a loop), so this must be a typed error, not a panic in a
/// dependency.
fn check_size(cols: u16, rows: u16) -> Result<()> {
    if cols == 0 || rows == 0 {
        return Err(Error::Input(format!(
            "a terminal needs at least one column and one row, got {cols}x{rows} \
             (columns x rows)"
        )));
    }
    Ok(())
}

/// Canonical printable shape of a known query (for diagnostics when the
/// responder is disabled).
fn query_shape(query: &Query) -> String {
    match query {
        Query::CursorPosition { private: false } => "^[[6n".into(),
        Query::CursorPosition { private: true } => "^[[?6n".into(),
        Query::OperatingStatus => "^[[5n".into(),
        Query::PrimaryDa => "^[[c".into(),
        Query::SecondaryDa => "^[[>c".into(),
        Query::TextAreaSize => "^[[18t".into(),
        Query::OscColor { code, .. } => format!("^[]{code};?"),
        Query::RequestMode(mode) => format!("^[[?{mode}$p"),
        Query::Unanswerable(shape) => shape.clone(),
    }
}

/// Drain the PTY into the emulator until EOF. Runs on a dedicated thread.
fn reader_loop(
    mut reader: Box<dyn Read + Send>,
    shared: &Monitor<EmuState>,
    writer: &SharedWriter,
    replies_to: Option<&mpsc::SyncSender<WriteRequest>>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // The emulator stops at each DEC 2026 frame end and at
                // each query, so state read there (screen, cursor) is
                // exact — even when the same chunk already carries the
                // following bytes. Replies are BUILT under the state
                // lock but WRITTEN after it is released; the state lock
                // and the writer lock are never held together.
                let replies = shared.mutate(|state| {
                    // Counted before the chunk is processed, so a query
                    // inside it is recorded against *this* read: output
                    // batched into the same write as the probe must not
                    // read as the application having moved past it.
                    state.reads += 1;
                    let mut replies: Vec<Vec<u8>> = Vec::new();
                    let mut offset = 0;
                    while offset < n {
                        let processed = state.emu.process(&buf[offset..n]);
                        offset += processed.consumed;
                        match processed.stop {
                            Some(Stop::FrameComplete) => {
                                state.frames_seen += 1;
                                let frame = state.emu.snapshot();
                                if state.frames.len() == FRAME_HISTORY {
                                    state.frames.pop_front();
                                }
                                state.frames.push_back(frame);
                            }
                            Some(Stop::Query(query)) => {
                                if let Some(reply) = state.answer(&query) {
                                    replies.push(reply);
                                }
                            }
                            None => {}
                        }
                    }
                    state.touch();
                    replies
                });
                for reply in replies {
                    match replies_to {
                        // Hand off without waiting. A full queue means the
                        // application has stopped reading its input
                        // entirely, so it cannot be waiting on these bytes
                        // — and the drain must keep running regardless.
                        Some(tx) => {
                            let request = WriteRequest {
                                bytes: reply,
                                ack: None, // fire and forget: never wait here
                            };
                            if let Err(mpsc::TrySendError::Full(_)) = tx.try_send(request) {
                                shared.mutate(|state| state.replies_dropped += 1);
                            }
                        }
                        // No responder thread (no descriptor to duplicate):
                        // write inline, as before.
                        None => {
                            let mut writer = writer.lock().unwrap_or_else(PoisonError::into_inner);
                            if let Some(writer) = writer.as_mut() {
                                let _ = writer.write_all(&reply).and_then(|()| writer.flush());
                            }
                        }
                    }
                }
            }
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
    // Option only so Drop can close it under the PTY lifecycle lock;
    // Some for the entire life of the value outside Drop.
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    /// Shared with the reader thread, which writes query replies.
    writer: SharedWriter,
    /// Queue to the writer thread. `None` only when no descriptor could
    /// be duplicated, in which case writes go through `writer` directly.
    write_tx: Option<mpsc::SyncSender<WriteRequest>>,
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

    /// Send one key press or modifier [`Chord`](crate::Chord). See
    /// [`Key`](crate::Key) for the encodings.
    ///
    /// # Panics
    ///
    /// Panics if the bytes cannot be written to the PTY — the child
    /// exited and the OS tore the terminal down, or the application
    /// stopped reading its input and the PTY buffer filled, in which
    /// case the write gives up at the terminal's deadline rather than
    /// blocking forever. Either way the panic message includes the
    /// current screen. A test that types into a program which cannot
    /// receive it is broken; failing loudly beats a silent no-op, and
    /// beats a hang by more.
    ///
    /// [`click`](Self::click), [`scroll`](Self::scroll),
    /// [`paste`](Self::paste) and [`send_str`](Self::send_str) share this
    /// contract.
    pub fn send(&mut self, key: impl Input + fmt::Debug) {
        let application_cursor = self.input_modes().application_cursor;
        self.write_or_panic(&key.encode_modal(application_cursor), &format!("{key:?}"));
    }

    /// Send a string literally (UTF-8 bytes, no key mapping, no newline).
    ///
    /// # Panics
    ///
    /// Same contract as [`send`](Self::send).
    pub fn send_str(&mut self, s: &str) {
        self.write_or_panic(s.as_bytes(), "literal text");
    }

    /// Paste text, the way a terminal pastes.
    ///
    /// When the application has enabled bracketed paste (mode 2004 —
    /// crossterm's `EnableBracketedPaste`), the text arrives wrapped in
    /// `ESC[200~ … ESC[201~` and the application sees **one paste
    /// event**, not a burst of key presses. When it hasn't, the bytes
    /// arrive unwrapped — exactly like a real terminal.
    ///
    /// Two transformations make the paste behave like a real one, both
    /// applied to the text itself:
    ///
    /// - **Line breaks become `\r`**, the byte the Enter key produces.
    ///   Applications in raw mode (which clears `ICRNL`) never see `\n`
    ///   from a terminal, so a pasted `"a\nb"` would otherwise be a line
    ///   break the application does not recognize.
    /// - **Paste markers inside the text are removed** while bracketed
    ///   paste is active, repeatedly until none remain. Otherwise an
    ///   embedded `ESC[201~` would end the paste early and the remainder
    ///   would arrive as ordinary key presses — the classic paste
    ///   injection, and a silent way for a test to exercise something
    ///   other than what it wrote.
    ///
    /// To send bytes with no transformation at all, use
    /// [`send_str`](Self::send_str) and write the brackets yourself.
    ///
    /// # Panics
    ///
    /// Same contract as [`send`](Self::send).
    pub fn paste(&mut self, text: &str) {
        // Real terminals send CR for a pasted line break; \r\n collapses
        // to a single CR rather than two line breaks.
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.input_modes().bracketed_paste {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(strip_paste_markers(&text).as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.write_or_panic(&bytes, "a bracketed paste");
        } else {
            self.write_or_panic(text.as_bytes(), "a paste");
        }
    }

    /// Click the primary button at `(col, row)` (0-based, like
    /// [`Screen::cell`]). Sends a press — and, when the application's
    /// tracking mode reports them, a release — encoded exactly as the
    /// tracking mode and encoding **the application enabled** (SGR 1006
    /// or the legacy byte form).
    ///
    /// # Errors
    ///
    /// [`Error::Input`] when the application has not enabled mouse
    /// tracking (feeding it mouse bytes anyway would be misparsed as
    /// garbage keys), or when the position is unrepresentable in the
    /// legacy encoding (columns/rows beyond 222).
    pub fn click(&mut self, col: u16, row: u16) -> Result<()> {
        self.click_with(MouseButton::Left, col, row)
    }

    /// Click a specific button, optionally with modifiers, at
    /// `(col, row)`.
    ///
    /// Takes a [`MouseButton`] or a [`MouseChord`], the same way
    /// [`send`](Self::send) takes a [`Key`](crate::Key) or a
    /// [`Chord`](crate::Chord):
    ///
    /// ```no_run
    /// # use termlens::MouseButton;
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder().spawn("true")?;
    /// t.click_with(MouseButton::Right, 10, 4)?;          // context menu
    /// t.click_with(MouseButton::Left.ctrl(), 10, 4)?;    // multi-select
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Same conditions as [`click`](Self::click).
    pub fn click_with(&mut self, button: impl Into<MouseChord>, col: u16, row: u16) -> Result<()> {
        let chord = button.into();
        let modes = self.input_modes();
        let press_only = match modes.mouse {
            MouseMode::None => return Err(no_mouse_tracking()),
            MouseMode::Press => true,
            MouseMode::PressRelease | MouseMode::ButtonMotion | MouseMode::AnyMotion => false,
        };
        let mut bytes = self.mouse_report(&modes, chord.code(), col, row, true)?;
        if !press_only {
            bytes.extend(self.mouse_report(&modes, chord.code(), col, row, false)?);
        }
        self.write_or_panic(&bytes, "a mouse click");
        Ok(())
    }

    /// Drag from one cell to another: press, motion, release.
    ///
    /// What actually reaches the application depends on what it asked
    /// for, as always. Under button-event or any-event tracking
    /// (`?1002`/`?1003`) the motion report is included; under plain
    /// `?1000` the application asked not to hear about motion, so it
    /// receives the press and release alone — the endpoints, which is
    /// what selection handling usually needs. Under X10 (`?9`) there is
    /// no release at all, so a drag cannot be expressed and this is a
    /// typed error rather than a misleading half-gesture.
    ///
    /// # Errors
    ///
    /// [`Error::Input`] when no mouse tracking is enabled, when the
    /// tracking mode is X10, or when a position is unrepresentable in
    /// the encoding the application selected.
    pub fn drag(
        &mut self,
        button: impl Into<MouseChord>,
        from: (u16, u16),
        to: (u16, u16),
    ) -> Result<()> {
        let chord = button.into();
        let modes = self.input_modes();
        let report_motion = match modes.mouse {
            MouseMode::None => return Err(no_mouse_tracking()),
            MouseMode::Press => {
                return Err(Error::Input(
                    "the application enabled X10 mouse tracking (CSI ?9 h), which \
                     reports presses only — a drag has no release to report"
                        .into(),
                ))
            }
            MouseMode::PressRelease => false,
            MouseMode::ButtonMotion | MouseMode::AnyMotion => true,
        };
        let (from_col, from_row) = from;
        let (to_col, to_row) = to;

        let mut bytes = self.mouse_report(&modes, chord.code(), from_col, from_row, true)?;
        if report_motion {
            // A motion report is the button code plus 32.
            bytes.extend(self.mouse_report(&modes, chord.code() + 32, to_col, to_row, true)?);
        }
        bytes.extend(self.mouse_report(&modes, chord.code(), to_col, to_row, false)?);
        self.write_or_panic(&bytes, "a mouse drag");
        Ok(())
    }

    /// Scroll the wheel one notch at `(col, row)` (0-based).
    ///
    /// # Errors
    ///
    /// Same conditions as [`click`](Self::click).
    pub fn scroll(&mut self, col: u16, row: u16, direction: Scroll) -> Result<()> {
        let modes = self.input_modes();
        if modes.mouse == MouseMode::None {
            return Err(no_mouse_tracking());
        }
        let button = match direction {
            Scroll::Up => 64,
            Scroll::Down => 65,
            Scroll::Left => 66,
            Scroll::Right => 67,
        };
        // Wheel events are presses only; there is no release.
        let bytes = self.mouse_report(&modes, button, col, row, true)?;
        self.write_or_panic(&bytes, "a mouse scroll");
        Ok(())
    }

    fn input_modes(&self) -> InputModes {
        self.shared.lock().emu.input_modes()
    }

    fn mouse_report(
        &self,
        modes: &InputModes,
        button: u8,
        col: u16,
        row: u16,
        press: bool,
    ) -> Result<Vec<u8>> {
        if modes.mouse_encoding == MouseEncoding::Sgr {
            return Ok(mouse_sgr(button, col, row, press));
        }
        if col > 222 || row > 222 {
            return Err(Error::Input(format!(
                "({col}, {row}) is unrepresentable in the legacy mouse \
                 encoding the application selected (max 222)"
            )));
        }
        // Both remaining forms are `ESC [ M Cb Cx Cy`; they differ only in
        // how a coordinate byte above 127 is written.
        let button = if press { button } else { 3 }; // legacy: release is 3
        Ok(match modes.mouse_encoding {
            MouseEncoding::Utf8 => mouse_utf8(button, col, row),
            MouseEncoding::Legacy | MouseEncoding::Sgr => mouse_legacy(button, col, row),
        })
    }

    fn write_or_panic(&mut self, bytes: &[u8], what: &str) {
        // Build the panic message (which takes the state lock for the
        // screen) strictly after the write attempt finishes, so no two
        // locks are ever held together.
        if let Err(reason) = self.write_within_deadline(bytes) {
            panic!(
                "termlens: failed to send {what} to `{}` ({reason})\n--- screen ---\n{}",
                self.command_desc,
                self.screen()
            );
        }
    }

    /// Write to the child, bounded by the default deadline.
    ///
    /// Writes into a PTY block once the application stops reading its
    /// input, and there is no portable way to ask whether the next write
    /// would block — `POLLOUT` on a macOS master reports writable and
    /// then blocks anyway. So the write happens on the writer thread and
    /// this one waits for an acknowledgement with a deadline: the test
    /// gets a screen-carrying failure naming the real cause instead of
    /// the six-hour CI job `docs/DESIGN.md` §2 exists to prevent.
    fn write_within_deadline(&mut self, bytes: &[u8]) -> std::result::Result<(), String> {
        let Some(tx) = &self.write_tx else {
            // No writer thread (no descriptor to duplicate): the original
            // synchronous path, which can block — better than not being
            // able to type at all.
            let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
            return match writer.as_mut() {
                Some(writer) => writer
                    .write_all(bytes)
                    .and_then(|()| writer.flush())
                    .map_err(|e| e.to_string()),
                None => Err("the terminal is closed".to_owned()),
            };
        };

        let not_reading = || {
            format!(
                "the application is not reading its input, and the PTY buffer is \
                 full — no progress in {:?}",
                self.default_timeout
            )
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let request = WriteRequest {
            bytes: bytes.to_vec(),
            ack: Some(ack_tx),
        };
        // A full queue means the writer thread is already stuck on an
        // earlier write, which is the same diagnosis. (`send_timeout` is
        // still unstable, so this is a bounded retry on `try_send`.)
        let deadline = Instant::now() + self.default_timeout;
        let mut pending = request;
        loop {
            match tx.try_send(pending) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(not_reading());
                    }
                    pending = returned;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err("the terminal is closed".to_owned())
                }
            }
        }
        match ack_rx.recv_timeout(self.default_timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(not_reading()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("the terminal is closed".to_owned()),
        }
    }

    /// Block until `predicate` holds on the screen.
    ///
    /// The predicate is re-evaluated whenever new output arrives. Fails with
    /// [`Error::Timeout`] at the deadline (builder `timeout`), or
    /// [`Error::Eof`] as soon as the PTY closes with the predicate still
    /// false — both embed the screen for debugging.
    ///
    /// # Race-free waiting
    ///
    /// The guarantee is precise: every byte up to and including the ones
    /// that made the predicate true has been processed — and nothing more.
    /// No byte marks where a repaint ends, so a predicate can fire on a
    /// half-painted screen, including half a row. Three rules (with the
    /// field stories behind them: `docs/DESIGN.md` §2):
    ///
    /// 1. **Put everything you assert into this one predicate.** A
    ///    [`Screen`] is one consistent instant; `wait_until(a)` followed by
    ///    `assert!(screen().b)` is a race between two instants.
    /// 2. **Wait on the last thing your app paints** (the rightmost text
    ///    of the bottom row, the cursor's resting position) before
    ///    snapshotting a whole screen — not on a line drawn midway.
    /// 3. **Settle before whole-screen snapshots**: a snapshot asserts on
    ///    cells no predicate named, so [`wait_idle`](Self::wait_idle)
    ///    first.
    ///
    /// Applications that emit DEC 2026 synchronized updates need none of
    /// this — [`wait_frame`](Self::wait_frame) sees only complete frames.
    /// After a [`resize`](Self::resize), also see the stale-frame trap
    /// documented there.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] / [`Error::Eof`], each carrying the screen.
    pub fn wait_until(&mut self, predicate: impl FnMut(&Screen) -> bool) -> Result<()> {
        self.wait_until_deadline(predicate, self.default_timeout)
    }

    /// [`wait_until`](Self::wait_until) with a per-call timeout — for the
    /// one known-slow moment (a first compile, a large fixture load) that
    /// shouldn't force every other wait in the suite to the slow value.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] / [`Error::Eof`], each carrying the screen.
    pub fn wait_until_for(
        &mut self,
        predicate: impl FnMut(&Screen) -> bool,
        timeout: Duration,
    ) -> Result<()> {
        self.wait_until_deadline(predicate, timeout)
    }

    fn wait_until_deadline(
        &mut self,
        mut predicate: impl FnMut(&Screen) -> bool,
        timeout: Duration,
    ) -> Result<()> {
        const WHAT: &str = "the screen predicate to hold";
        let deadline = Instant::now() + timeout;
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
                    waiting_for: format!("{WHAT}{}", state.query_note()),
                    screen,
                }));
            }
            None
        });
        match outcome {
            Ok(inner) => inner,
            Err(Expired) => Err(Error::Timeout {
                waiting_for: format!("{WHAT}{}", self.shared.lock().query_note()),
                timeout,
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
    /// Frames completed *before* the call are evaluated too, so a fast
    /// application cannot slip one past you — including when several
    /// complete inside a single read. The terminal retains the last
    /// **8** completed frames and each call scans them oldest first, so
    /// a burst like a progress counter ticking `1`, `2`, `3` in one
    /// write is observable step by step by three successive calls.
    ///
    /// A frame is one *completed* synchronized update: an
    /// `EndSynchronizedUpdate` that closes a Begin this terminal actually
    /// saw. An unmatched End publishes nothing — the `?2026l` inside the
    /// mode-reset string applications emit defensively at startup and on
    /// crash is not a repaint, and treating it as one would both invent a
    /// frame and hide the "never emitted a synchronized update" diagnosis
    /// below. A Begin/End pair that changed no cell *does* publish: the
    /// count is of repaints, not of changes.
    ///
    /// Two honest limits follow from the retention bound: a burst longer
    /// than 8 frames drops its oldest, and because retained frames stay
    /// matchable, a predicate that was true of an earlier frame resolves
    /// immediately rather than waiting for a new one. Where that matters,
    /// assert on something only the newer frame can show.
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
    pub fn wait_frame(&mut self, predicate: impl FnMut(&Screen) -> bool) -> Result<()> {
        self.wait_frame_deadline(predicate, self.default_timeout)
    }

    /// [`wait_frame`](Self::wait_frame) with a per-call timeout, for the
    /// one known-slow repaint that shouldn't drag every other wait in the
    /// suite up to its deadline.
    ///
    /// # Errors
    ///
    /// Same as [`wait_frame`](Self::wait_frame).
    pub fn wait_frame_for(
        &mut self,
        predicate: impl FnMut(&Screen) -> bool,
        timeout: Duration,
    ) -> Result<()> {
        self.wait_frame_deadline(predicate, timeout)
    }

    fn wait_frame_deadline(
        &mut self,
        mut predicate: impl FnMut(&Screen) -> bool,
        timeout: Duration,
    ) -> Result<()> {
        const WHAT: &str = "a complete frame matching the predicate";
        let deadline = Instant::now() + timeout;
        let mut seen_frame = None;
        let outcome = self.shared.wait_until(deadline, |state| {
            if state.frames_seen > 0 && seen_frame != Some(state.frames_seen) {
                seen_frame = Some(state.frames_seen);
                // Oldest retained frame first: several frames can
                // complete inside one read, and a test asserting on a
                // sequence must be able to see them in the order the
                // application drew them.
                if state.frames.iter().any(&mut predicate) {
                    return Some(Ok(()));
                }
            }
            if state.eof {
                return Some(Err(Error::Eof {
                    waiting_for: format!("{WHAT}{}", state.query_note()),
                    screen: state.peek_snapshot(),
                }));
            }
            None
        });
        match outcome {
            Ok(inner) => inner,
            Err(Expired) => {
                // The live screen, like every other wait: the error's
                // header says "screen at timeout" and a CI log is often
                // the only evidence there is. The last completed frame
                // can be arbitrarily old — it is named in `waiting_for`
                // instead, where it reads as a count rather than a
                // picture that claims to be current.
                let (frames, screen, note) = {
                    let mut guard = self.shared.lock();
                    let screen = guard.snapshot();
                    let note = guard.query_note();
                    (guard.frames_seen, screen, note)
                };
                let waiting_for = if frames == 0 {
                    // An application blocked on an unanswered probe never
                    // reaches its first repaint, so this is exactly where
                    // the query note earns its place: without it the
                    // message blames the app for not emitting frames.
                    format!(
                        "a complete frame — but the application never emitted a \
                         DEC 2026 synchronized update. wait_frame needs repaints \
                         bracketed in BeginSynchronizedUpdate/EndSynchronizedUpdate; \
                         for other apps use wait_until (docs/DESIGN.md §2){note}"
                    )
                } else {
                    format!("{WHAT} ({frames} complete frames observed){note}")
                };
                Err(Error::Timeout {
                    waiting_for,
                    timeout,
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
    /// [`wait_until`](Self::wait_until) on visible content where possible,
    /// or [`wait_frame`](Self::wait_frame) where the application emits
    /// DEC 2026 synchronized updates. `docs/DESIGN.md` §2 discusses the
    /// trade-off.
    ///
    /// # Errors
    ///
    /// [`Error::Timeout`] when the overall deadline (builder `timeout`)
    /// expires first — e.g. when `quiet` exceeds the timeout, or the child
    /// keeps chattering.
    pub fn wait_idle(&mut self, quiet: Duration) -> Result<()> {
        self.wait_idle_deadline(quiet, self.default_timeout)
    }

    /// [`wait_idle`](Self::wait_idle) with a per-call timeout.
    ///
    /// Both arguments are durations and the order matters: `quiet` is the
    /// silence being waited *for*, `timeout` is how long to wait for it.
    /// `quiet` must be the smaller of the two, or the wait can only ever
    /// time out.
    ///
    /// ```no_run
    /// # use std::time::Duration;
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder().spawn("true")?;
    /// // 100ms of silence, waited for up to 30s.
    /// t.wait_idle_for(Duration::from_millis(100), Duration::from_secs(30))?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Same as [`wait_idle`](Self::wait_idle), against `timeout`.
    pub fn wait_idle_for(&mut self, quiet: Duration, timeout: Duration) -> Result<()> {
        self.wait_idle_deadline(quiet, timeout)
    }

    fn wait_idle_deadline(&mut self, quiet: Duration, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
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
                let note = guard.query_note();
                drop(guard);
                return Err(Error::Timeout {
                    waiting_for: format!("{quiet:?} of output silence{note}"),
                    timeout,
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

    /// The child's OS process id, when the platform reports one.
    ///
    /// Useful for out-of-band inspection (`/proc`, `ps`, `lsof`). The pid
    /// belongs to the child until it has been reaped
    /// ([`wait_exit`](Self::wait_exit) or `Drop`) — after that the OS may
    /// reuse it, so don't deliver signals to a stored pid yourself;
    /// [`signal`](Self::signal) has that guard built in.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// Deliver `signal` to the child process (`kill(2)`) — the tool for
    /// graceful-shutdown paths: send `SIGTERM`, then assert the app saves
    /// its state and exits cleanly. Unix only.
    ///
    /// ```
    /// # use termlens::Signal;
    /// # fn main() -> termlens::Result<()> {
    /// let mut t = termlens::Terminal::builder()
    ///     .timeout(std::time::Duration::from_secs(10))
    ///     .args(["-c", "trap 'echo bye; exit 0' TERM; echo up; while :; do sleep 0.05; done"])
    ///     .spawn("sh")?;
    /// t.wait_until(|s| s.contains("up"))?;
    /// t.signal(Signal::Term)?;
    /// t.wait_until(|s| s.contains("bye"))?;
    /// assert!(t.wait_exit()?.success());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Input`] when the child has already been reaped (its pid may
    /// have been reused — signaling it would be misdirected) or reports no
    /// pid; [`Error::Io`] when `kill(2)` itself fails.
    #[cfg(unix)]
    pub fn signal(&mut self, signal: Signal) -> Result<()> {
        if let Some(status) = &self.exit_status {
            return Err(Error::Input(format!(
                "cannot deliver {signal:?} to `{}`: it already exited ({status})",
                self.command_desc
            )));
        }
        let Some(pid) = self.pid() else {
            return Err(Error::Input(format!(
                "cannot deliver {signal:?} to `{}`: the platform reports no pid",
                self.command_desc
            )));
        };
        let pid = libc::pid_t::try_from(pid)
            .map_err(|_| Error::Input(format!("pid {pid} exceeds the platform's pid range")))?;
        // SAFETY: kill(2) touches no memory. The pid is our own un-reaped
        // child (guarded above): worst case it is a zombie, for which kill
        // is defined and harmless — never an unrelated, recycled pid.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::kill(pid, signal.raw()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::Io(io::Error::last_os_error()))
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
        self.wait_exit_deadline(self.default_timeout)
    }

    /// [`wait_exit`](Self::wait_exit) with a per-call timeout — for the
    /// application whose shutdown is slower than everything else the
    /// suite waits on.
    ///
    /// # Errors
    ///
    /// Same as [`wait_exit`](Self::wait_exit), against `timeout`.
    pub fn wait_exit_for(&mut self, timeout: Duration) -> Result<ExitStatus> {
        self.wait_exit_deadline(timeout)
    }

    fn wait_exit_deadline(&mut self, timeout: Duration) -> Result<ExitStatus> {
        if let Some(status) = self.exit_status.clone() {
            return Ok(status);
        }
        let deadline = Instant::now() + timeout;
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
                    waiting_for: format!(
                        "`{}` to exit{}",
                        self.command_desc,
                        self.shared.lock().query_note()
                    ),
                    timeout,
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
    /// # The stale-frame trap
    ///
    /// The grid resizes immediately — `s.cols()` reports the new width on
    /// the very next snapshot — but its **content** is still the old
    /// frame, clipped to the new geometry, until the child handles
    /// SIGWINCH and repaints. This wait can therefore resolve on entirely
    /// stale content:
    ///
    /// ```no_run
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder().spawn("true")?;
    /// t.resize(50, 20)?;
    /// t.wait_until(|s| s.cols() == 50 && s.contains("tasks (10)"))?; // ← both true BEFORE the repaint
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Wait for something only the post-SIGWINCH frame can show — content
    /// that needs the new width, a complete status bar on the new bottom
    /// row — or use [`wait_frame`](Self::wait_frame) where the app emits
    /// synchronized updates. `docs/DESIGN.md` §2 shows the trap in full.
    ///
    /// # Errors
    ///
    /// [`Error::Input`] if either dimension is zero (a terminal has at
    /// least one row and one column), before the PTY or the grid is
    /// touched; [`Error::Pty`] if the ioctl fails.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        check_size(cols, rows)?;
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
    ///
    /// The reap is bounded (`DROP_REAP_GRACE`). A child that cannot be
    /// reaped is a bad outcome; a test binary that never exits — which an
    /// unbounded `wait` here produced whenever the child was wedged — is a
    /// worse one, and unlike the first it cannot even be diagnosed.
    fn drop(&mut self) {
        let _lifecycle = pty_lifecycle_guard();
        if self.exit_status.is_none() {
            let already_exited = matches!(self.child.try_wait(), Ok(Some(_)));
            if !already_exited {
                let _ = self.child.kill();
                let deadline = Instant::now() + DROP_REAP_GRACE;
                let mut backoff = INITIAL_BACKOFF;
                while !matches!(self.child.try_wait(), Ok(Some(_))) {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    thread::sleep(backoff.min(deadline - now));
                    backoff = next_backoff(backoff);
                }
            }
        }
        // Close the PTY fds while still holding the lock. Taking the writer
        // out of the shared cell closes its fd even though the reader
        // thread keeps the (now empty) cell alive — and it must happen
        // before the master is dropped, since the reader polls the
        // master's descriptor while holding this lock.
        drop(
            self.writer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        drop(self.master.take());
    }
}
