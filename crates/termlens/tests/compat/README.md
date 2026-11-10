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
named as unsupported (#320), because that is what 0.10.1 wrote.

`0.11.0/` and every later directory are written **by the release tree at
the release commit**, with the `write_corpus` writer in `tests/compat.rs`
(`docs/RELEASING.md`, step 2b) — the crate is not on crates.io yet when
its release PR is cut, and the tree at the tag is byte for byte what gets
published. Its six text files are identical to 0.10.1's, which is the
point; its JSON carries `"format": 1` and the #320 fix.

`0.11.1/pending-wrap.json` is a seventh file, in that directory only: a
screen the published 0.11.1 wrote after a write filled its last column,
so the JSON carries the cursor at `col == cols` — the pending-wrap
position no grid has (#401). Every later reader clamps that column onto
the last cell rather than refusing the file (#375), and this file is what
holds it to that; the six shapes cannot, because the writer no longer
produces the column. It is JSON only: the text format drops a hidden
cursor's position, so only the JSON ever carried the column.

Three more files sit outside the six shapes, each with its JSON twin, for
shapes the text reader refused until #643 or #683. Both were written by the
**published** release (a scratch crate on `termlens = "=<version>"` from
crates.io, its lockfile resolving vt100 0.16), and `tests/compat.rs` names
them in `EXTRA_SHAPES`:

| file | what it pins |
| --- | --- |
| `0.11.1/pending-wrap-visible` | `printf '\r\nabcdef'` on 6x2: the visible cursor at `1,6`, one past the last column, in the text header. It reads clamped onto `1,5`, as the JSON reader clamps it |
| `0.11.6/stranded-wide` | `printf 'abcd東xy\r\nABC東'` on 8x3, then `resize(5, 3)`: `東` left wide in the last column with no continuation cell (DESIGN §3's truncation case). It reads as that cell and renders back byte for byte |
| `0.11.6/stranded-wide-redrawn` | `printf 'abcd東xyz'` on 8x2, `resize(5, 2)`, `resize(8, 2)`, then `printf '\033[1;6HZyw'`: the stranded `東` left before `Zyw`, which fills the row to the edge (#683). It reads with `東` as the one-column cell its JSON twin has, and renders back byte for byte |

One pair sits outside `EXTRA_SHAPES` because its twins do not show one
picture, and the extra shapes are held to showing one:
`0.11.6/vs16-styled-after`, written by the published 0.11.6 the same way
as the two above. It is `printf '⚠️\033[31mERR\033[0m ok'` on 10x2:
0.11.6 drew the emoji one column wide, which its JSON twin records; the
text is the same bytes in either layout, and the reader takes it two
columns wide because the row has room (DESIGN §3, rule 4; #723). Its own
two tests in `tests/compat.rs` (`VS16_BEFORE_0_11_7`) pin that reading: the text
renders back byte for byte, every cell after the emoji sits a column
right, the `styles:` span and the cursor keep their columns, and the
diff against the JSON twin is that row and nothing else.
