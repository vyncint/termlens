//! termlens fixture: a minimal "form" that proves `send()` + `wait_until`
//! round-trips. It echoes printable characters into an input line, names the
//! last key it decoded (stable custom format, independent of crossterm's
//! `Debug`), and reports submitted lines.
//!
//! Exit codes: 0 on Esc, 42 on Ctrl-X (exercises exit-status plumbing).
//!
//! Every redraw is bracketed in a DEC 2026 synchronized update, so this
//! fixture also exercises `wait_frame`. F2 triggers a deliberately *torn*
//! draw — half a row, a real 150ms pause, then the rest, all inside one
//! synchronized update — to prove `wait_frame` never observes the tear.
//! That sleep is the only nondeterminism in any fixture, and it affects
//! timing only, never content.
//!
//! Fixture rules: deterministic — redraws only in response to input.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::style::Print;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, Clear, ClearType,
    EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};

#[derive(Default)]
struct App {
    input: String,
    last: String,
    submitted: String,
}

/// Stable, greppable one-token description of a key event.
/// Stable one-token description of a mouse event.
fn describe_mouse(mouse: &MouseEvent) -> Option<String> {
    let kind = match mouse.kind {
        MouseEventKind::Down(_) => "down",
        MouseEventKind::Up(_) => "up",
        MouseEventKind::ScrollUp => "scrollup",
        MouseEventKind::ScrollDown => "scrolldown",
        _ => return None,
    };
    Some(format!("mouse:{kind}:{},{}", mouse.column, mouse.row))
}

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
    // Special keys: stable "ctrl+alt+shift+name" prefix order. BackTab
    // inherently carries SHIFT; keep its historical bare name.
    let mut prefix = String::new();
    if key.code != KeyCode::BackTab {
        for (on, name) in [
            (key.modifiers.contains(KeyModifiers::CONTROL), "ctrl+"),
            (key.modifiers.contains(KeyModifiers::ALT), "alt+"),
            (key.modifiers.contains(KeyModifiers::SHIFT), "shift+"),
        ] {
            if on {
                prefix.push_str(name);
            }
        }
    }
    let name: String = match key.code {
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
    };
    format!("{prefix}{name}")
}

fn draw(out: &mut impl Write, app: &App) -> io::Result<()> {
    queue!(out, BeginSynchronizedUpdate)?;
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
    queue!(out, EndSynchronizedUpdate)?;
    out.flush()
}

/// Half a frame, a real pause, then the rest — inside ONE synchronized
/// update. An unsynchronized observer sees the tear; `wait_frame` must not.
fn draw_torn(out: &mut impl Write) -> io::Result<()> {
    queue!(
        out,
        BeginSynchronizedUpdate,
        MoveTo(0, 5),
        Clear(ClearType::CurrentLine),
        Print("torn: left")
    )?;
    out.flush()?;
    std::thread::sleep(std::time::Duration::from_millis(150));
    queue!(out, MoveTo(10, 5), Print(" right"), EndSynchronizedUpdate)?;
    out.flush()
}

fn cleanup(out: &mut impl Write) -> io::Result<()> {
    execute!(out, DisableMouseCapture, Show, LeaveAlternateScreen)?;
    disable_raw_mode()
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, Hide, EnableMouseCapture)?;

    let mut app = App::default();
    draw(&mut out, &app)?;

    loop {
        let key = match event::read()? {
            Event::Key(key) => key,
            Event::Mouse(mouse) => {
                if let Some(description) = describe_mouse(&mouse) {
                    app.last = description;
                    draw(&mut out, &app)?;
                }
                continue;
            }
            _ => continue,
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('x') {
            cleanup(&mut out)?;
            std::process::exit(42);
        }

        if key.code == KeyCode::F(2) {
            draw_torn(&mut out)?;
            continue;
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
