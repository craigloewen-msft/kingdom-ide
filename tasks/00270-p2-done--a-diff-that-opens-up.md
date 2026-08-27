# A diff that opens up

The diff panel showed a change and three lines either side of it, and nothing
else. That says *what* moved and often not *where*: the `fn` a hunk sits inside
starts above the first row drawn, so the King read a rewritten line with no way
to tell which function it belonged to without leaving the panel.

Now every break between hunks is a control strip, and he opens it twenty lines
at a time from either end, or all at once.

```
⋯ 373 lines not shown          [ ↑ 20 ]  [ Show all 373 ]  [ ↓ 20 ]
```

- **↑ 20** — the lines against the change *below*. The one he reaches for.
- **↓ 20** — read on from the change *above*.
- **Show all N** — only while what remains fits one answer.
- The strip goes when the two revealed runs meet, because nothing is behind it.

## The panel gained two edges it never had

A gap is now drawn **before the first hunk and after the last**. That is not an
edge case bolted on: a change on line 400 of an 800-line file used simply to be
the top of the panel, with no sign that 399 lines came first and 400 followed.
Measured in the rehearsal, `src/lib.rs` with two changes far apart drew three
strips — 116 above, 373 between, 191 below — where it had drawn one bare `⋯`.

## Why the lines are fetched rather than shipped

The obvious implementation is to send the whole file and fold it in the browser.
That would undo the cap the panel is built around. `MOST_ROWS` (4,000) exists
because the cost of a diff is DOM nodes, and *a 40,000-line file with one changed
line is currently cheap* — precisely because the unchanged 39,990 never leave the
server. Shipping them would make the cheapest case the most expensive one.

So expansion is a request: `plan_diff_context` → `review::context`. One answer is
capped at `MOST_CONTEXT` for the same reason, and that constant sits in
`kingdom-core` rather than beside the reader — the browser decides whether to
offer "Show all" against the same number the server will enforce, and two of them
would be a button that quietly gives less than it says.

## Nothing is re-diffed to answer, and that is a fact rather than a shortcut

The region between two hunks is **the same text in both versions by
construction**: `similar`'s `grouped_ops` only ever breaks inside a run of
`Equal` lines. So `context` reads both versions, takes the two slices, and pairs
them straight across.

It **checks** they match rather than trusting the arithmetic, and refuses with
*"this file has changed since it was compared"* if they do not. The ordinary way
to reach that is the court rewriting the file between the panel fetching the diff
and the King pressing a button — and pairing two lines that no longer correspond
would put unrelated text opposite itself with nothing on screen saying so. Driven
in the browser: editing line 50 under an open panel and pressing **Show all**
produced exactly that sentence, in the strip's own place.

That refusal also drove one small change nobody would find by testing the server.
`ServerFnError`'s `Display` prefixes *"error running server function: "*, which is
a fact about the transport, and in a strip a few words wide it buried the
sentence the King actually needed. `plainly` takes the server's own words.

## A truncated diff offers no control at all

`FileDiff::may_expand` is false for anything but `DiffVerdict::Shown`, and
**truncated** is the case it exists for. Rows were dropped part-way through a
hunk, so the hunk's declared range no longer describes what is on screen; an
expansion computed from it would skip the dropped lines silently. No button is a
better answer than a lying one, and the panel already says the comparison is
partial.

## What a hunk had to learn to say

`Hunk` carried rows and no position — it knew what it *showed* and nothing about
what it was hiding. It now carries what a unified diff spells `@@ -a,b +c,d @@`,
and `FileDiff` carries both files' line counts, without which the leading and
trailing gaps cannot be measured.

The four numbers are taken from the group's **own ranges**, not read back off the
rows. A hunk that is entirely an insertion has no old line to read a position
from, and would have to guess. They are also taken *before* the row cap trims
anything: what a hunk covers is a fact about the two files, and truncating what
is drawn must not make it read as covering less.

The arithmetic on top of them is in `kingdom-core`, not the view — pure, compiles
to both targets, and pinned by seven tests. `Gap` carries **both** files'
positions rather than one number, because by the second hunk the columns have
drifted: in the rehearsal file old line 124 and new line 126 are the same text,
and a single number would have misnumbered one column. `Gap::narrowed` returning
`None` is what takes a strip off screen when its two runs meet.

## What the view had to be split into

The row block was ~60 lines living inside the hunk `<For>`. Revealed context
lines need exactly it — both `Side`s plus the note composer spanning them — so it
is now a `Row` component. One renderer, so **a line the King opened up takes a
margin note the same way a line the diff chose to show does**, which is usually
why he opened it. Confirmed in the browser: a note on revealed line 250 landed in
the review margin with its ✎ mark, having never been part of the diff.

`Gap` renders a **fragment** — rows, strip, rows — and never a wrapper element.
`.diff-grid` is the grid and `.diff-row` is `display: contents`; a box around any
of it would take the cells out of the grid and put the two columns out of step.

Its reveal state is local and clears when the gap it was handed moves, which is
exactly when the court has edited the file. Rows left standing across that would
be text the King is still reading and the workspace no longer holds. Rehearsed:
a third edit while the panel was open recomputed four gaps and dropped every
revealed line.

The strip's content is `position: sticky; left: 0; width: 100cqi`, the trick
`.diff-composer` already used. With wrapping off the grid is as wide as the
longest line in the file, so a strip that scrolled with it would carry its own
buttons off the panel's right edge. Measured with the grid scrolled 234px
sideways: strip pinned at the stage's 801–1440, buttons ending at 1430.

## What was checked, in a browser

Against a seeded realm on port 3987, records in a temp `KINGDOM_HOME`:

| | |
|---|---|
| three strips on a two-hunk file | 116 / 373 / 191, all correct |
| ↑ 20 on the leading gap | 116 → 96, lines 97–116 drawn |
| Show all 353 between the hunks | 56 rows → 409 |
| line numbers after two reveals | 97–505 continuous, **zero jumps** |
| a note on revealed line 250 | lands in the margin with its mark |
| the file edited under the panel | four gaps recomputed, reveals dropped |
| a reveal of a region since rewritten | refused, in the strip |
| Show all on the trailing gap | opens to line 826, strip vanishes |
| sticky strip, wrapping off | buttons stay inside the panel |

The full check passes: fmt, clippy, all four suites (87 / 281 / 203 / 19), and
the wasm build of `kingdom-app`.

## What was deliberately not done

- **No expansion in the source view.** It already shows the whole file.
- **No memory of what was revealed** across a refetch. Keeping it means showing
  text that may have changed underneath.
- **20 and 400** are GitHub's step and a bound; both are single constants.
