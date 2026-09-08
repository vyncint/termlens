# Emulator backends: the comparison behind decision 2

[STABILITY.md](STABILITY.md) §2 says `vt100` stays and the attribute
shadow stays on purpose. This is the comparison it rests on (#150),
measured in September 2026 against the seven methods the `Emulator` trait
needs — `process`, `snapshot`, `mid_sequence`, `in_sync_update`,
`input_modes`, `mode_state`, `set_size` — and against the three places the
backend, not the design, was the limit: blink/conceal/strikethrough (the
shadow), mouse modes collapsing into one value, and exotic grapheme
clusters rendering however the backend renders them.

## The candidates

| | `vt100` 0.16.2 | `alacritty_terminal` 0.26.0 | `wezterm-term` |
|---|---|---|---|
| On crates.io | yes | yes | **no** — git-only in the wezterm workspace; the `tattoy-wezterm-term` fork is a third party's |
| Licence | MIT | Apache-2.0 only | MIT |
| MSRV | 1.70 | 1.85.0, moves with Alacritty releases | wezterm's, moves with wezterm |
| Source size | 3,950 lines | 11,739 lines | larger; a workspace of crates (`wezterm-cell`, `-escape-parser`, `-surface`, `termwiz`) |
| Dependency tree (`default-features = false`) | 9 crates: `vte`, `unicode-width`, `itoa`, and theirs | 27 crates: `vte`, `polling`, `rustix`, `rustix-openpty`, `signal-hook`, `parking_lot`, `regex-automata`, `base64`, `home`… — the tty/event-loop half is not feature-gated | `image`, `serde`, `terminfo`, `url`, `lru`, `miniz_oxide`, `unicode-normalization`, `wezterm-bidi`… |
| Headless driving | `Parser::process(bytes)`; `Callbacks` trait for what it does not handle | `Term::new(config, dims, EventListener)` + `vte::ansi::Processor::advance`; `VoidListener` exists | `Terminal::new` with a `TerminalConfiguration` and a writer; designed around a renderer |
| SGR blink (`5`/`6`) | dropped | **dropped** — no `Flags::BLINK`; `Attr::Blink` is not handled | tracked |
| SGR conceal (`8`) | dropped | tracked (`HIDDEN`) | tracked |
| SGR strikethrough (`9`) | dropped | tracked (`STRIKEOUT`) | tracked |
| Extra attributes | — | double/curly/dotted/dashed underline, underline colour | the same and more |
| Mouse modes | one value (the last set) | distinct flags: `MOUSE_REPORT_CLICK`, `MOUSE_DRAG`, `MOUSE_MOTION`, `SGR_MOUSE`; no 1005 | distinct |
| Kitty keyboard, focus, bracketed paste, alt screen, insert, origin, LNM | partial (see below) | all as `TermMode` flags | all |
| Grapheme clusters | one `char` + width; zero-width chars appended to the previous cell | one `char` + `zerowidth: Vec<char>`, same shape | `finl_unicode` segmentation; the strongest of the three |
| Scrollback reflow on resize | no | yes (`grid/resize.rs`) | yes |
| Unhandled-sequence hook | `Callbacks::unhandled_*` — the basis of `Screen::unsupported` | none; unknown sequences are silently ignored | none |
| Release cadence | slow: 0.16.2 (July 2025) is the newest | with Alacritty, several a year, breaking freely | with wezterm |

## What termlens already does in front of the backend

The `Emulator` trait was meant as a swap point, but the more useful thing
it turned out to be is a place to stand *in front of* the backend with a
staged stream both parsers receive. Everything below is termlens's own and
would survive any swap unchanged:

- **Character sets** (G0–G3, SO/SI, SS2/SS3, DEC Special Graphics, UK):
  glyph substitution before the parser.
- **Tab stops** (HTS/TBC/CHT/CBT, and plain `HT`): rewritten to `CHA`.
- **Insert mode** (IRM): rewritten to `ICH` sized in columns.
- **The unsupported record** (`Screen::unsupported`): every sequence the
  backend did not implement, from `Callbacks` — the honesty layer that
  says "this grid may be wrong" instead of looking plausible.
- **Queries and their answers**, the title, the clipboard, links, the
  counters, graphics payloads: the sequence tracker, not the emulator.
- **Blink, conceal, strikethrough**: the shadow parser.

## Why a swap does not pay today

1. **`wezterm-term` cannot be depended on.** It is not published;
   `deny.toml` refuses git sources and crates.io refuses git dependencies.
   It would have to be vendored, which is the option the shadow was chosen
   over at 3,950 lines, now at many times that.
2. **`alacritty_terminal` does not close the gap it would be adopted
   for.** It tracks two of the three attributes and drops blink. A swap
   keeps a shadow (or a fork) for one attribute, so the mechanism stays
   and only the reason shrinks. It also pulls an event loop and a tty
   layer termlens has its own of, triples the dependency count, moves the
   MSRV onto Alacritty's schedule, and — because it ignores what it does
   not implement — would cost `Screen::unsupported` its source.
3. **The shadow is not a performance problem.** 40,000 lines through an
   80x24 screen: 260 ms with it, 263 ms without; 284 ms against 282 ms
   when every line carries four SGR sequences. A terminal's throughput is
   the PTY round trip.
4. **The three limits have been answered in front of the backend, or
   accepted.** Mouse modes: `Screen::mouse_modes` reports the set the
   application enabled from the tracker (#151). Graphemes: the
   unicode-torture fixture pins vt100's rendering rather than promising
   one, and no candidate promises Unicode-correct segmentation *and* the
   rest of this table.

## What would reopen the question

- **vt100 gains blink, conceal and strikethrough** (three spare bits in
  `mode: u8`, an 80-line patch): `emu/shadow.rs` deletes and
  `convert_cell` reads the flags. This is the outcome to watch for, and
  the module header says so.
- **A candidate appears that is published, tracks all three attributes,
  can be driven headless without an event loop, and reports the
  sequences it does not handle.** Then the comparison is worth re-running
  against the trait, behind a feature flag, with the fixture suite as the
  judge — the ratatui fidelity test and the unicode-torture snapshot are
  the two that would move.
- **A consumer needs reflowed history.** Neither the design nor the
  backend offers it today (`Terminal::resize` says why); a backend that
  does would be an argument, not a decision.
