//! termlens fixture: prints `size: <cols>x<rows>` on startup and reprints it
//! every time the terminal is resized (kernel delivers SIGWINCH after
//! TIOCSWINSZ; crossterm surfaces it as `Event::Resize`). Exits on `q`.
//!
//! Proves that `Terminal::resize()` really reaches the child.
//!
//! Each draw clears the whole screen first and writes `press q to quit`
//! last, so that line is on the screen before a resize and again once the
//! repaint is complete, and on no screen in between. That is what lets
//! `resize_and_wait`'s test wait on it and know which of the two it got.
//!
//! `--on-resize WHAT` changes what a resize draws, for the tests of what
//! `resize_and_wait` counts as the application having drawn (#788):
//!
//! - `hide-first`: hide the cursor, in a write of its own, then pause, then
//!   repaint as usual: output that draws nothing, read ahead of the repaint;
//! - `query-first`: ask where the cursor is and read the answer, then pause,
//!   then repaint as usual;
//! - `hide-only`: hide the cursor and draw nothing at all;
//! - `same`: write `press q to quit` again where it already is, and nothing
//!   else, so the repaint leaves every cell as it was;
//! - `clear-only`: clear the screen and draw nothing else.
//!
//! The pause is what keeps the first write in a read of its own, as a real
//! application's would be whenever it did any work between the two.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::{self, Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::Print;
use crossterm::terminal::{
    self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};

/// Long enough that the harness has read the first write before the
/// repaint is written, on a loaded machine too.
const PAUSE: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, PartialEq, Eq)]
enum OnResize {
    Redraw,
    HideFirst,
    QueryFirst,
    HideOnly,
    Same,
    ClearOnly,
}

fn draw(out: &mut impl Write, cols: u16, rows: u16) -> io::Result<()> {
    queue!(
        out,
        Clear(ClearType::All),
        MoveTo(0, 0),
        Print(format!("size: {cols}x{rows}")),
        MoveTo(0, 1),
        Print("press q to quit")
    )?;
    out.flush()
}

fn on_resize(out: &mut impl Write, what: OnResize, cols: u16, rows: u16) -> io::Result<()> {
    match what {
        OnResize::Redraw => draw(out, cols, rows),
        OnResize::HideFirst => {
            execute!(out, Hide)?;
            std::thread::sleep(PAUSE);
            draw(out, cols, rows)
        }
        OnResize::QueryFirst => {
            cursor::position()?;
            std::thread::sleep(PAUSE);
            draw(out, cols, rows)
        }
        OnResize::HideOnly => execute!(out, Hide),
        OnResize::Same => {
            queue!(out, MoveTo(0, 1), Print("press q to quit"))?;
            out.flush()
        }
        OnResize::ClearOnly => execute!(out, Clear(ClearType::All)),
    }
}

fn parse_args() -> io::Result<OnResize> {
    let mut args = std::env::args().skip(1);
    let mut what = OnResize::Redraw;
    while let Some(arg) = args.next() {
        if arg != "--on-resize" {
            return Err(io::Error::other(format!("unknown argument {arg:?}")));
        }
        what = match args.next().as_deref() {
            Some("hide-first") => OnResize::HideFirst,
            Some("query-first") => OnResize::QueryFirst,
            Some("hide-only") => OnResize::HideOnly,
            Some("same") => OnResize::Same,
            Some("clear-only") => OnResize::ClearOnly,
            other => {
                return Err(io::Error::other(format!("unknown --on-resize {other:?}")));
            }
        };
    }
    Ok(what)
}

fn main() -> io::Result<()> {
    let what = parse_args()?;
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
    let mut size = (cols, rows);

    loop {
        match event::read()? {
            // A resize to the size the program already has is not one. The
            // Windows console reports one as the program starts. There the
            // `clear-only` repaint erased the first frame before any test
            // could see it, while every other mode left it as it was
            // (windows.yml runs 45444329525 and 45444351961).
            Event::Resize(cols, rows) if (cols, rows) == size => {}
            Event::Resize(cols, rows) => {
                size = (cols, rows);
                on_resize(&mut out, what, cols, rows)?;
            }
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
