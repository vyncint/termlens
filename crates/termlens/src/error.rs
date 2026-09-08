//! Error types. The prime directive: when a wait fails in CI, the log must
//! show what the terminal actually looked like — so timeout/EOF errors embed
//! a full [`Screen`] snapshot and render it in their `Display` output.
//!
//! The same screens reach a directory when `TERMLENS_ARTIFACT_DIR` is set
//! (#251), so a step after the tests can render them into the pull request
//! rather than leaving them in the log; see [`Error`].

use std::time::Duration;

use crate::Screen;

/// Convenience alias for `std::result::Result<T, termlens::Error>`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by [`Terminal`](crate::Terminal) operations.
///
/// # `TERMLENS_ARTIFACT_DIR`
///
/// Every variant that carries a [`Screen`] prints it, so a CI log shows
/// what the application displayed. When the environment variable
/// `TERMLENS_ARTIFACT_DIR` names a directory, the same screen is also
/// written there as it is embedded: `<test>-<n>.screen.json` with the
/// `serde` feature, `<test>-<n>.screen.txt` (the `with_styles` rendering,
/// which [`Screen::parse`] reads back) without, where `<test>` is the
/// current thread's name — under `cargo test`, the test's path. Unset, the
/// hook is one environment read and nothing else; insta's `.snap.new`
/// files stay where insta puts them. This repository's `report` action
/// (`.github/actions/report`) renders that directory into a pull
/// request's step summary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A `wait_*` call ran past its deadline. The screen at the moment of
    /// the timeout is embedded and printed, so a CI log alone is enough to
    /// see what the application was actually showing.
    #[error(
        "timed out after {timeout:?} while waiting for {waiting_for}\n\
         --- screen at timeout ---\n{screen}"
    )]
    Timeout {
        /// Human description of what was awaited.
        waiting_for: String,
        /// The deadline that expired.
        timeout: Duration,
        /// The screen when the deadline expired.
        screen: Screen,
    },

    /// The PTY reached end-of-file (the child exited or closed its
    /// terminal) while a wait's condition was still unmet. Waiting longer
    /// can never succeed, so this fails fast instead of burning the full
    /// timeout.
    #[error(
        "terminal closed (EOF) while waiting for {waiting_for}\n\
         --- final screen ---\n{screen}"
    )]
    Eof {
        /// Human description of what was awaited.
        waiting_for: String,
        /// The final screen contents.
        screen: Screen,
    },

    /// Spawning the child process failed.
    #[error("failed to spawn `{command}`: {reason}")]
    Spawn {
        /// The command line that failed to spawn.
        command: String,
        /// The underlying PTY/OS error.
        reason: String,
    },

    /// A PTY control operation (open, resize, reader/writer setup) failed.
    #[error("PTY error: {0}")]
    Pty(String),

    /// An OS-level I/O error (e.g. while waiting on the child process).
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A terminal size argument is invalid and was rejected before anything
    /// was spawned or sent to the child.
    #[error("invalid terminal size: {0}")]
    Size(String),

    /// The VT emulator panicked while processing the child's output, so the
    /// grid stopped advancing at the screen embedded here.
    ///
    /// The emulation runs on the reader thread, where a panic propagates
    /// nowhere: before this existed the drain simply died, every later
    /// snapshot returned the same frozen screen, and each wait ran to its
    /// deadline reporting a predicate that was never going to become true.
    /// A wait now fails immediately and says why. The screen is the last one
    /// taken before the failure — the emulator is not asked again, because
    /// its state after a panic means nothing.
    #[error(
        "the terminal emulator failed and the screen stopped advancing: {detail}\n\
         --- last screen before the failure ---\n{screen}"
    )]
    Emulator {
        /// The panic message from the emulator.
        detail: String,
        /// The last screen taken before the emulator failed.
        screen: Screen,
    },

    /// Typed input or control the child cannot receive — e.g. a mouse
    /// click while the application never enabled mouse tracking (sending
    /// it anyway would feed the app bytes it would misparse as garbage
    /// keys), or a signal to a child that has already been reaped (its
    /// pid may belong to someone else by now).
    #[error("input not receivable: {0}")]
    Input(String),

    /// A saved screen could not be read back by [`Screen::parse`]: the text
    /// is not the snapshot format of `docs/DESIGN.md` §3. The message names
    /// the line.
    #[error("could not parse a saved screen: {0}")]
    Parse(String),

    /// Typed input could not be delivered: the child is gone and the OS
    /// tore the terminal down, or it stopped reading its input and the
    /// write gave up at the terminal's deadline rather than blocking
    /// forever.
    ///
    /// Distinct from [`Error::Input`] on purpose. `Input` means the
    /// application cannot make sense of these bytes — a test bug. This
    /// means the bytes could not be handed over at all, which is a fact
    /// about the child rather than about the test, so the screen at the
    /// moment of the failure is embedded the way a timeout's is.
    #[error("failed to send {what}\n--- screen at the failed write ---\n{screen}")]
    Write {
        /// What was being sent, which command it was going to, and why the
        /// write failed.
        what: Box<str>,
        /// The screen when the write failed.
        screen: Screen,
    },
}

impl Error {
    /// The screen embedded in [`Error::Timeout`], [`Error::Eof`],
    /// [`Error::Emulator`] or [`Error::Write`], if any.
    #[must_use]
    pub fn screen(&self) -> Option<&Screen> {
        match self {
            Error::Timeout { screen, .. }
            | Error::Eof { screen, .. }
            | Error::Emulator { screen, .. }
            | Error::Write { screen, .. } => Some(screen),
            _ => None,
        }
    }

    /// The `TERMLENS_ARTIFACT_DIR` hook (#251): every error that carries a
    /// screen passes through here on its way out of the crate, and when the
    /// variable is set the screen is also written to that directory. The
    /// call is a no-op when it is not — the common case, and the reason
    /// the check is one environment read.
    pub(crate) fn recorded(self) -> Self {
        if let Some(screen) = self.screen() {
            artifact::write(screen);
        }
        self
    }
}

/// The `TERMLENS_ARTIFACT_DIR` hook. A CI log shows the screen a failing
/// wait embedded; this puts the same screen somewhere a step after the
/// tests can pick it up — the `report` action in this repository renders
/// each into the pull request's step summary.
pub(crate) mod artifact {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::Screen;

    /// The environment variable naming the directory. Unset means off.
    pub(crate) const VAR: &str = "TERMLENS_ARTIFACT_DIR";

    /// One counter per test process, so two screens from one test are two
    /// files rather than one overwritten.
    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Write `screen` to `$TERMLENS_ARTIFACT_DIR/<test>-<n>.screen.json`
    /// (with the `serde` feature) or `.screen.txt` (the `with_styles`
    /// rendering, which `Screen::parse` reads back). `<test>` is the
    /// current thread's name, which under `cargo test` is the test's path.
    /// Best effort: a directory that cannot be written is reported once on
    /// stderr, and the error the screen came from is returned regardless.
    pub(crate) fn write(screen: &Screen) {
        let Some(dir) = std::env::var_os(VAR).filter(|d| !d.is_empty()) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        let thread = std::thread::current();
        let test: String = thread
            .name()
            .unwrap_or("screen")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let (name, body) = render(screen, &format!("{test}-{n}"));
        let path = dir.join(name);
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, body)) {
            eprintln!("termlens: could not write {} ({VAR}): {e}", path.display());
        }
    }

    #[cfg(feature = "serde")]
    fn render(screen: &Screen, stem: &str) -> (String, String) {
        let json =
            serde_json::to_string(screen).unwrap_or_else(|_| screen.with_styles().to_string());
        (format!("{stem}.screen.json"), json)
    }

    #[cfg(not(feature = "serde"))]
    fn render(screen: &Screen, stem: &str) -> (String, String) {
        (
            format!("{stem}.screen.txt"),
            screen.with_styles().to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::{Cell, Style, TermState};

    fn tiny_screen() -> Screen {
        let mut cells = Vec::new();
        for ch in ['o', 'k'] {
            cells.push(Cell::new(ch.to_string(), Style::default(), false, false));
        }
        cells.push(Cell::new(String::new(), Style::default(), false, false));
        Screen::from_parts(3, 1, 0, 2, true, cells, TermState::default())
    }

    #[test]
    fn timeout_display_embeds_screen_dump() {
        let err = Error::Timeout {
            waiting_for: "text \"ready\"".into(),
            timeout: Duration::from_millis(250),
            screen: tiny_screen(),
        };
        let msg = err.to_string();
        assert!(msg.contains("timed out after 250ms"), "{msg}");
        assert!(msg.contains("--- screen at timeout ---"), "{msg}");
        assert!(msg.contains("size: 3x1  cursor: 0,2"), "{msg}");
        assert!(msg.contains("\nok"), "{msg}");
        assert_eq!(err.screen().unwrap().size(), (3, 1));
    }
}
