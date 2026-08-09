//! termlens fixture: a minimal "form" that proves `send()` + `wait_until`
//! round-trips. It echoes printable characters into an input line, names the
//! last key it decoded (stable custom format, independent of crossterm's
//! `Debug`), and reports submitted lines.
//!
//! Exit codes: 0 on Esc, 42 on Ctrl-X (exercises exit-status plumbing).
//!
//! Fixture rules: deterministic — redraws only in response to input.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::Print;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};

#[derive(Default)]
struct App {
    input: String,
    last: String,
    submitted: String,
}

/// Stable, greppable one-token description of a key event.
fn describe(key: &KeyEvent) -> String {
    if let KeyCode::Char(c) = key.code {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return format!("ctrl:{c}");
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return format!("alt:{c}");
        }
        return format!("char:{c}");
    }
    match key.code {
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::F(n) => format!("f:{n}"),
        other => format!("unknown:{other:?}"),
    }
}

fn draw(out: &mut impl Write, app: &App) -> io::Result<()> {
    let lines = [
        "form-echo ready".to_string(),
        format!("input: {}", app.input),
        format!("last: {}", app.last),
        format!("submitted: {}", app.submitted),
    ];
    for (row, line) in lines.iter().enumerate() {
        queue!(
            out,
            MoveTo(0, u16::try_from(row).unwrap()),
            Clear(ClearType::CurrentLine),
            Print(line)
        )?;
    }
    out.flush()
}

fn cleanup(out: &mut impl Write) -> io::Result<()> {
    execute!(out, Show, LeaveAlternateScreen)?;
    disable_raw_mode()
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, Hide)?;

    let mut app = App::default();
    draw(&mut out, &app)?;

    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('x') {
            cleanup(&mut out)?;
            std::process::exit(42);
        }

        match key.code {
            KeyCode::Esc => break,
            KeyCode::Enter => {
                app.submitted = std::mem::take(&mut app.input);
            }
            KeyCode::Backspace => {
                app.input.pop();
            }
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                app.input.push(c);
            }
            _ => {}
        }
        app.last = describe(&key);
        draw(&mut out, &app)?;
    }

    cleanup(&mut out)?;
    Ok(())
}
