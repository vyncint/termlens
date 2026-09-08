//! termlens fixture: a ratatui counter/list on the alternate screen, every
//! repaint bracketed in a DEC 2026 synchronized update. `j`/`k` move the
//! highlight (`j` also counts), a resize is acknowledged on the status line,
//! `q` quits. The picture is `ratatui_app::draw`, shared with the fidelity
//! test.

use std::io;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui_app::{draw, State, ITEMS};

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut state = State::default();
    let result = run(&mut terminal, &mut state);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, state: &mut State) -> io::Result<()> {
    paint(terminal, state)?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('j') | KeyCode::Down => {
                    state.counter += 1;
                    state.selected = (state.selected + 1).min(ITEMS.len() - 1);
                    state.last = "down".to_owned();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    state.selected = state.selected.saturating_sub(1);
                    state.last = "up".to_owned();
                }
                _ => continue,
            },
            Event::Resize(cols, rows) => state.last = format!("resize:{cols}x{rows}"),
            _ => continue,
        }
        paint(terminal, state)?;
    }
}

/// One complete frame: the whole draw inside Begin/End, so `wait_frame`
/// never sees a torn repaint.
fn paint(terminal: &mut ratatui::DefaultTerminal, state: &State) -> io::Result<()> {
    execute!(io::stdout(), BeginSynchronizedUpdate)?;
    terminal.draw(|frame| draw(frame, state))?;
    execute!(io::stdout(), EndSynchronizedUpdate)?;
    Ok(())
}
