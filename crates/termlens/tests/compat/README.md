# Saved-screen compatibility corpus

Files written by **published** releases of termlens, in the two formats a
saved screen takes — the snapshot text format of `docs/DESIGN.md` §3 and
the JSON the `serde` feature writes — and the test that holds every later
release to reading them (`tests/compat.rs`).

**The rule: a file in here is never edited. A new version adds a
directory.** The insta snapshots under `tests/snapshots/` are regression
*outputs* and are re-recorded when an intended change lands; these are
frozen *inputs*, and a change to the header, the `styles:` block or the
JSON shape that would re-record a snapshot fails here instead. That is the
difference between "the format stays valid" as a sentence in STABILITY.md
and as a thing CI checks.

Each directory holds six shapes, as `<name>.txt` and its JSON twin
`<name>.json`, written by that version from the same `Screen`:

| shape | what it pins |
| --- | --- |
| `plain` | `Display` alone: the header, a visible cursor, trailing blank rows |
| `styled` | `with_styles()` with every token the `styles:` block can carry — `fg=` indexed, `bg=#rrggbb`, and the eight attributes in SGR order |
| `hidden` | `cursor: hidden` in the header, and `(none)` for a styles block with nothing in it |
| `wide` | CJK, a combining mark, an emoji, and a wide glyph ending at the right margin |
| `masked` | `mask_rect` and `mask_matching` output: a fill character and blanks, with the original's styles kept |
| `text-of-styled` | `Display` of a screen that *has* styles: the text format is style-blind, and the JSON twin carries them anyway |

`0.10.1/` was written by the published 0.10.1 through a registry
dependency (`termlens = "=0.10.1"`), not a path — the JSON there has no
`format` field and, for `styled`, lists the SGR parameters 0.10.1 wrongly
named as unsupported (#320), because that is what 0.10.1 wrote. Later
directories are written by the release they are named after, from a
consumer of the published crate.
