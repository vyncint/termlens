//! termlens fixture: draws one static frame on the alternate screen, hides
//! the cursor, then blocks until it reads `q`.
//!
//! Fixture rules: deterministic by construction — no clocks, no animation,
//! no randomness. The frame is identical on every run.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::Print;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};

const INNER: usize = 30;

fn frame() -> Vec<String> {
    let bar = "─".repeat(INNER);
    vec![
        format!("╭{bar}╮"),
        format!("│{:<INNER$}│", " hello-tui"),
        format!("│{:<INNER$}│", ""),
        format!("│{:<INNER$}│", " status: ready"),
        format!("│{:<INNER$}│", " press q to quit"),
        format!("╰{bar}╯"),
    ]
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, Hide)?;

    for (row, line) in frame().iter().enumerate() {
        queue!(out, MoveTo(0, u16::try_from(row).unwrap()), Print(line))?;
    }
    out.flush()?;

    loop {
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                break;
            }
        }
    }

    execute!(out, Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
