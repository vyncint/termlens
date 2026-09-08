//! termlens fixture: writes exactly the bytes its arguments describe, in
//! order, then waits, sleeps, reads or exits as told — the program under
//! test wherever the suite used to run `sh -c 'printf …; read _'` (#249).
//!
//! A shell was a variable in every test that was not about the shell: which
//! `sh` runs decides how `printf` reads `\033`, what `read` does at EOF, how
//! fast a loop spins — and whether there is a shell at all. This is the same
//! program on every platform, with an argument per step and no quoting to
//! decode. Steps apply left to right:
//!
//! ```text
//! TEXT             literal text — any argument that is not a step below
//! NL  CR           a newline / a carriage return
//! --text WORD      literal text that happens to spell a step name
//! --esc BYTES      ESC followed by BYTES          `--esc '(0'`  is  ESC ( 0
//! --csi BYTES      ESC [ followed by BYTES         `--csi '?2026h'`
//! --raw SPEC       bytes with escapes: \e \n \r \t \a \\ and \xNN
//! --sleep DUR      pause for DUR: `250ms`, `1.5s`, `2s`
//! --wait           read one line from stdin and discard it — "hold the
//!                  terminal open until the test sends Enter"
//! --wait-for WORD  read lines until one is exactly WORD
//! --echo-line      read one line from stdin and write it back, without
//!                  its newline
//! --echo           copy stdin to stdout, line by line, until EOF
//! --seq N          the integers 1..=N, one per line
//! --cwd            the current directory, as the process sees it
//! --pid            this process's id, in decimal
//! --env NAME       the value of environment variable NAME, or `unset`
//! --environ        every environment variable as NAME=VALUE, one per
//!                  line, sorted
//! --exit CODE      exit now with CODE
//! --loop           run the steps before it once, then the steps after it
//!                  forever
//! ```
//!
//! And the steps that read what the terminal *typed back* — a query's reply,
//! a mouse report, a paste — which need the line discipline out of the way:
//!
//! ```text
//! --raw-mode       ICANON and ECHO off: bytes arrive as sent, unechoed
//!                  (what `stty -icanon -echo` did); ICRNL is left on, so
//!                  --wait still ends at Enter
//! --no-icrnl       ICRNL off too, so a CR arrives as CR (what raw mode
//!                  does in an application) — --wait then needs a LF
//! --read N         read exactly N bytes and write them, ESC as `E` and
//!                  BEL as `G` so a reply is legible on the grid
//! --skip N         read exactly N bytes and write nothing
//! --read-hex N     read exactly N bytes and write them as lowercase hex
//! --read-quiet N   read up to N bytes, stopping after 2s without one,
//!                  and write them as --read does
//! --read-count N C read exactly N bytes and write how many were C
//! --winsize        the tty's size as the kernel reports it:
//!                  `COLSxROWS px WIDTHxHEIGHT`
//! --kill-self      raise SIGTERM against this process
//! --on-term TEXT CODE  on SIGTERM, write TEXT and exit CODE …
//! --idle           … and sit here until that happens
//! ```
//!
//! Every emitting step is one `write_all` and a flush, so a test that wants
//! two writes says so with two steps. `--wait` at EOF exits 0: a harness that
//! closed the terminal has finished with it.
//!
//! Fixture rules: **no timing but the explicit `--sleep` and the 2s of
//! `--read-quiet`; std, plus `libc` on Unix for the terminal-mode, ioctl and
//! signal steps and nothing else.** A dependency with behaviour of its own
//! would make this a second thing the suite tests; a clock would make it a
//! second source of flakiness. Off Unix the terminal-mode steps are accepted
//! and do nothing — the tests that need them are Unix-only for other reasons.

use std::io::{self, BufRead, Write};
use std::process;
use std::time::Duration;

#[derive(Debug, Clone)]
enum Step {
    Write(Vec<u8>),
    Sleep(Duration),
    Wait,
    WaitFor(String),
    EchoLine,
    Echo,
    Seq(u64),
    Cwd,
    Pid,
    Env(String),
    Environ,
    Exit(i32),
    RawMode,
    NoIcrnl,
    Read(usize),
    Skip(usize),
    ReadHex(usize),
    ReadQuiet(usize),
    ReadCount(usize, u8),
    Winsize,
    KillSelf,
    OnTerm(Vec<u8>, i32),
    Idle,
}

fn usage(reason: &str) -> ! {
    eprintln!("emit: {reason}");
    eprintln!("see the crate doc in fixtures/emit/src/main.rs for the steps");
    process::exit(2)
}

/// Decode `--raw`: `\e` `\n` `\r` `\t` `\a` `\\` and `\xNN`; every other
/// byte is itself. A lone or malformed escape is an error, not a guess.
fn raw(spec: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(spec.len());
    let mut chars = spec.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('e') => out.push(0x1b),
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('a') => out.push(0x07),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) if hex.len() == 2 => out.push(b),
                    _ => usage(&format!("--raw: `\\x{hex}` is not two hex digits")),
                }
            }
            other => usage(&format!(
                "--raw: unknown escape `\\{}`",
                other.map_or(String::new(), String::from)
            )),
        }
    }
    out
}

/// `250ms`, `1.5s`, `2s`.
fn duration(spec: &str) -> Duration {
    let (number, unit) = spec.strip_suffix("ms").map_or_else(
        || (spec.strip_suffix('s').unwrap_or(spec), 1.0),
        |n| (n, 0.001),
    );
    match number.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Duration::from_secs_f64(v * unit),
        _ => usage(&format!(
            "--sleep: `{spec}` is not a duration like 250ms or 1.5s"
        )),
    }
}

fn count(flag: &str, value: &str) -> usize {
    value
        .parse()
        .unwrap_or_else(|_| usage(&format!("{flag} needs a byte count")))
}

/// The steps, and the index the forever-loop starts at, if there is one.
fn parse(args: impl Iterator<Item = String>) -> (Vec<Step>, Option<usize>) {
    let mut steps = Vec::new();
    let mut loop_from = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut next = |flag: &str| {
            args.next()
                .unwrap_or_else(|| usage(&format!("{flag} needs a value")))
        };
        let step = match arg.as_str() {
            "NL" => Step::Write(b"\n".to_vec()),
            "CR" => Step::Write(b"\r".to_vec()),
            "--text" => Step::Write(next("--text").into_bytes()),
            "--esc" => {
                let mut b = vec![0x1b];
                b.extend_from_slice(next("--esc").as_bytes());
                Step::Write(b)
            }
            "--csi" => {
                let mut b = b"\x1b[".to_vec();
                b.extend_from_slice(next("--csi").as_bytes());
                Step::Write(b)
            }
            "--raw" => Step::Write(raw(&next("--raw"))),
            "--sleep" => Step::Sleep(duration(&next("--sleep"))),
            "--wait" => Step::Wait,
            "--wait-for" => Step::WaitFor(next("--wait-for")),
            "--echo-line" => Step::EchoLine,
            "--echo" => Step::Echo,
            "--seq" => Step::Seq(
                next("--seq")
                    .parse()
                    .unwrap_or_else(|_| usage("--seq needs a count")),
            ),
            "--cwd" => Step::Cwd,
            "--pid" => Step::Pid,
            "--env" => Step::Env(next("--env")),
            "--environ" => Step::Environ,
            "--exit" => Step::Exit(
                next("--exit")
                    .parse()
                    .unwrap_or_else(|_| usage("--exit needs an exit code")),
            ),
            "--loop" => {
                loop_from = Some(steps.len());
                continue;
            }
            "--raw-mode" => Step::RawMode,
            "--no-icrnl" => Step::NoIcrnl,
            "--read" => Step::Read(count("--read", &next("--read"))),
            "--skip" => Step::Skip(count("--skip", &next("--skip"))),
            "--read-hex" => Step::ReadHex(count("--read-hex", &next("--read-hex"))),
            "--read-quiet" => Step::ReadQuiet(count("--read-quiet", &next("--read-quiet"))),
            "--read-count" => {
                let n = count("--read-count", &next("--read-count"));
                let which = next("--read-count");
                match which.as_bytes() {
                    [b] => Step::ReadCount(n, *b),
                    _ => usage("--read-count needs one byte to count"),
                }
            }
            "--winsize" => Step::Winsize,
            "--kill-self" => Step::KillSelf,
            "--on-term" => {
                let text = next("--on-term").into_bytes();
                let code = next("--on-term")
                    .parse()
                    .unwrap_or_else(|_| usage("--on-term needs an exit code"));
                Step::OnTerm(text, code)
            }
            "--idle" => Step::Idle,
            other if other.starts_with("--") => usage(&format!("unknown step `{other}`")),
            text => Step::Write(text.as_bytes().to_vec()),
        };
        steps.push(step);
    }
    (steps, loop_from)
}

/// Read one line from stdin. `None` at EOF — the terminal is gone.
fn line(stdin: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut s = String::new();
    if stdin.read_line(&mut s)? == 0 {
        return Ok(None);
    }
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    Ok(Some(s))
}

/// A reply as a grid can show it: ESC as `E`, BEL as `G`, the rest itself.
fn legible(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .map(|&b| match b {
            0x1b => b'E',
            0x07 => b'G',
            other => other,
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_exact(stdin: &mut impl BufRead, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    stdin.read_exact(&mut buf)?;
    Ok(buf)
}

/// What the `--on-term` handler is to do, once a SIGTERM has arrived.
struct OnTerm {
    text: Vec<u8>,
    code: i32,
}

fn run(
    steps: &[Step],
    out: &mut impl Write,
    stdin: &mut impl BufRead,
    on_term: &mut Option<OnTerm>,
) -> io::Result<()> {
    for step in steps {
        match step {
            Step::Write(bytes) => out.write_all(bytes)?,
            Step::Sleep(d) => std::thread::sleep(*d),
            Step::Wait => {
                if line(stdin)?.is_none() {
                    process::exit(0);
                }
            }
            Step::WaitFor(word) => loop {
                match line(stdin)? {
                    Some(l) if l == *word => break,
                    Some(_) => {}
                    None => process::exit(0),
                }
            },
            Step::EchoLine => match line(stdin)? {
                Some(l) => out.write_all(l.as_bytes())?,
                None => process::exit(0),
            },
            Step::Echo => {
                let mut buf = String::new();
                while stdin.read_line(&mut buf)? > 0 {
                    out.write_all(buf.as_bytes())?;
                    out.flush()?;
                    buf.clear();
                }
            }
            Step::Seq(n) => {
                for i in 1..=*n {
                    writeln!(out, "{i}")?;
                }
            }
            Step::Cwd => {
                let dir = std::env::current_dir()?;
                out.write_all(dir.to_string_lossy().as_bytes())?;
            }
            Step::Pid => write!(out, "{}", process::id())?,
            Step::Env(name) => match std::env::var_os(name) {
                Some(value) => out.write_all(value.to_string_lossy().as_bytes())?,
                None => out.write_all(b"unset")?,
            },
            Step::Environ => {
                let mut vars: Vec<String> = std::env::vars_os()
                    .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
                    .collect();
                vars.sort();
                for var in vars {
                    writeln!(out, "{var}")?;
                }
            }
            Step::Exit(code) => {
                out.flush()?;
                process::exit(*code);
            }
            Step::RawMode => tty::raw_mode(false)?,
            Step::NoIcrnl => tty::raw_mode(true)?,
            Step::Read(n) => out.write_all(&legible(&read_exact(stdin, *n)?))?,
            Step::Skip(n) => {
                read_exact(stdin, *n)?;
            }
            Step::ReadHex(n) => out.write_all(hex(&read_exact(stdin, *n)?).as_bytes())?,
            Step::ReadQuiet(n) => out.write_all(&legible(&tty::read_quiet(stdin, *n)?))?,
            Step::ReadCount(n, which) => {
                let got = read_exact(stdin, *n)?;
                write!(out, "{}", got.iter().filter(|b| *b == which).count())?;
            }
            Step::Winsize => out.write_all(tty::winsize()?.as_bytes())?,
            Step::KillSelf => {
                out.flush()?;
                tty::kill_self();
            }
            Step::OnTerm(text, code) => {
                tty::trap_term();
                *on_term = Some(OnTerm {
                    text: text.clone(),
                    code: *code,
                });
            }
            Step::Idle => loop {
                if tty::term_received() {
                    if let Some(OnTerm { text, code }) = on_term.take() {
                        out.write_all(&text)?;
                        out.flush()?;
                        process::exit(code);
                    }
                    process::exit(0);
                }
                std::thread::sleep(Duration::from_millis(10));
            },
        }
        out.flush()?;
    }
    Ok(())
}

fn main() {
    let (steps, loop_from) = parse(std::env::args().skip(1));
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut on_term = None;
    // A write that fails is the terminal going away under us — the harness
    // has torn down. Nothing to report to, so nothing to report.
    let result = match loop_from {
        Some(from) => run(&steps[..from], &mut out, &mut stdin, &mut on_term).and_then(|()| loop {
            run(&steps[from..], &mut out, &mut stdin, &mut on_term)?;
        }),
        None => run(&steps, &mut out, &mut stdin, &mut on_term),
    };
    if result.is_err() {
        process::exit(0);
    }
}

/// The terminal-mode, ioctl and signal steps: one `tcsetattr`, one `ioctl`,
/// one `raise`, one `signal`. Everything the shell scripts reached for
/// `stty`, `python3 -c 'fcntl.ioctl…'`, `kill -TERM $$` and `trap` to do.
#[cfg(unix)]
mod tty {
    use std::io::{self, BufRead};
    use std::sync::atomic::{AtomicBool, Ordering};

    static TERM_RECEIVED: AtomicBool = AtomicBool::new(false);

    fn termios() -> io::Result<libc::termios> {
        // SAFETY: a zeroed termios is a valid value for tcgetattr to fill.
        #[allow(unsafe_code)]
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: fd 0 and a live, writable termios.
        #[allow(unsafe_code)]
        if unsafe { libc::tcgetattr(0, &mut t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(t)
    }

    fn apply(t: &libc::termios) -> io::Result<()> {
        // SAFETY: fd 0 and a termios tcgetattr filled.
        #[allow(unsafe_code)]
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `stty -icanon -echo`, and `-icrnl` when asked.
    pub(super) fn raw_mode(no_icrnl: bool) -> io::Result<()> {
        let mut t = termios()?;
        t.c_lflag &= !(libc::ICANON | libc::ECHO);
        if no_icrnl {
            t.c_iflag &= !libc::ICRNL;
        }
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        apply(&t)
    }

    /// Up to `max` bytes, ending after two seconds without one — `stty min
    /// 0 time 20` for the duration of the read, then back to blocking.
    pub(super) fn read_quiet(stdin: &mut impl BufRead, max: usize) -> io::Result<Vec<u8>> {
        let mut t = termios()?;
        let before = t;
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 20;
        apply(&t)?;
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while out.len() < max {
            let want = buf.len().min(max - out.len());
            match stdin.read(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    apply(&before)?;
                    return Err(e);
                }
            }
        }
        apply(&before)?;
        Ok(out)
    }

    /// `TIOCGWINSZ`, as `COLSxROWS px WIDTHxHEIGHT`.
    pub(super) fn winsize() -> io::Result<String> {
        // SAFETY: a zeroed winsize is a valid value for the ioctl to fill.
        #[allow(unsafe_code)]
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: fd 0, the request the struct is for, and a live struct.
        #[allow(unsafe_code)]
        if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(format!(
            "{}x{} px {}x{}",
            ws.ws_col, ws.ws_row, ws.ws_xpixel, ws.ws_ypixel
        ))
    }

    pub(super) fn kill_self() -> ! {
        // SAFETY: raise(3) touches no memory.
        #[allow(unsafe_code)]
        unsafe {
            libc::raise(libc::SIGTERM);
        }
        // With the default disposition the line above did not return.
        std::process::exit(0)
    }

    extern "C" fn on_term(_: libc::c_int) {
        TERM_RECEIVED.store(true, Ordering::SeqCst);
    }

    pub(super) fn trap_term() {
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe; the function pointer outlives the process.
        #[allow(unsafe_code)]
        unsafe {
            libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        }
    }

    pub(super) fn term_received() -> bool {
        TERM_RECEIVED.load(Ordering::SeqCst)
    }
}

/// Off Unix there is no line discipline to switch off and no signal to
/// trap; the steps are accepted so a program reads the same everywhere,
/// and the tests that depend on them are Unix-only for other reasons.
#[cfg(not(unix))]
mod tty {
    use std::io::{self, BufRead};

    pub(super) fn raw_mode(_no_icrnl: bool) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn read_quiet(stdin: &mut impl BufRead, max: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; max];
        let n = stdin.read(&mut out)?;
        out.truncate(n);
        Ok(out)
    }

    pub(super) fn winsize() -> io::Result<String> {
        Ok("unsupported".to_owned())
    }

    pub(super) fn kill_self() -> ! {
        std::process::exit(143)
    }

    pub(super) fn trap_term() {}

    pub(super) fn term_received() -> bool {
        false
    }
}
