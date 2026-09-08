//! termlens fixture: the `draw` function of a small ratatui application,
//! kept in a library so the fidelity test can render it twice — through the
//! PTY by termlens and in-process by ratatui's `TestBackend` — and diff the
//! two cell by cell (#252). Where they disagree the bug is in the terminal
//! layer — crossterm's encoding, the PTY, or termlens's emulation — which
//! is precisely the layer nothing else tests.
//!
//! Fixture rules: deterministic by construction — no clocks, no animation,
//! no randomness. A given `State` at a given size draws one picture.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};

/// Everything the picture depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Incremented by `j`, shown in the title.
    pub counter: u32,
    /// The highlighted row of the list.
    pub selected: usize,
    /// What the last event was, on the status line — a resize names its
    /// size, so a test can wait for the application to have handled it.
    pub last: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            counter: 0,
            selected: 0,
            last: "ready".to_owned(),
        }
    }
}

/// The list's rows. One is CJK, so the wide-character path is on the table.
pub const ITEMS: [&str; 4] = ["Alpha", "Beta", "東京", "Delta"];

/// Draw the whole application into `frame`.
pub fn draw(frame: &mut Frame<'_>, state: &State) {
    let [title, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(format!(
            "ratatui-app  Counter: {}",
            state.counter
        )))
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        title,
    );

    let items: Vec<ListItem<'_>> = ITEMS.iter().map(|item| ListItem::new(*item)).collect();
    let list = List::new(items)
        .block(Block::bordered().title(" items "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    let mut list_state = ListState::default().with_selected(Some(state.selected));
    frame.render_stateful_widget(list, body, &mut list_state);

    frame.render_widget(
        Paragraph::new(format!("last: {}   j/k move, q quits", state.last))
            .style(Style::default().fg(Color::DarkGray)),
        status,
    );
}

/// `draw` rendered in-process at `cols`×`rows`: what ratatui itself says the
/// screen should be, for the fidelity test to hold the PTY rendering against.
///
/// # Panics
///
/// If ratatui cannot draw into a test backend, which it always can.
#[must_use]
pub fn render(cols: u16, rows: u16, state: &State) -> Buffer {
    let backend = ratatui::backend::TestBackend::new(cols, rows);
    let mut terminal = Terminal::new(backend).expect("a test backend always opens");
    terminal
        .draw(|frame| draw(frame, state))
        .expect("a test backend always draws");
    terminal.backend().buffer().clone()
}
