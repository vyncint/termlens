//! Error types. The prime directive: when a wait fails in CI, the log must
//! show what the terminal actually looked like — so timeout/EOF errors embed
//! a full [`Screen`] snapshot and render it in their `Display` output.

use std::time::Duration;

use crate::Screen;

/// Convenience alias for `std::result::Result<T, termlens::Error>`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by [`Terminal`](crate::Terminal) operations.
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
}

impl Error {
    /// The screen embedded in [`Error::Timeout`] / [`Error::Eof`], if any.
    #[must_use]
    pub fn screen(&self) -> Option<&Screen> {
        match self {
            Error::Timeout { screen, .. } | Error::Eof { screen, .. } => Some(screen),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::{Cell, Style};

    fn tiny_screen() -> Screen {
        let mut cells = Vec::new();
        for ch in ['o', 'k'] {
            cells.push(Cell::new(ch.to_string(), Style::default(), false, false));
        }
        cells.push(Cell::new(String::new(), Style::default(), false, false));
        Screen::from_parts(3, 1, 0, 2, true, cells)
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
