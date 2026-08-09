//! termlens fixture: prints `size: <cols>x<rows>` on startup and reprints it
//! every time the terminal is resized (kernel delivers SIGWINCH after
//! TIOCSWINSZ; crossterm surfaces it as `Event::Resize`). Exits on `q`.
//!
//! Proves that `Terminal::resize()` really reaches the child.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::Print;
use crossterm::terminal::{
    self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};

fn draw(out: &mut impl Write, cols: u16, rows: u16) -> io::Result<()> {
    queue!(
        out,
        MoveTo(0, 0),
        Clear(ClearType::CurrentLine),
        Print(format!("size: {cols}x{rows}")),
        MoveTo(0, 1),
        Clear(ClearType::CurrentLine),
        Print("press q to quit")
    )?;
    out.flush()
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, Hide)?;

    // Initialize crossterm's event source (which registers the SIGWINCH
    // listener) BEFORE the first draw. The harness treats the first drawn
    // size as "ready to be resized"; without this, a resize landing between
    // the draw and the first event::read() is silently lost (SIGWINCH's
    // default disposition is ignore) and the test deadlocks.
    let _ = event::poll(std::time::Duration::from_secs(0))?;

    let (cols, rows) = terminal::size()?;
    draw(&mut out, cols, rows)?;

    loop {
        match event::read()? {
            Event::Resize(cols, rows) => draw(&mut out, cols, rows)?,
            Event::Key(key)
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') =>
            {
                break;
            }
            _ => {}
        }
    }

    execute!(out, Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
