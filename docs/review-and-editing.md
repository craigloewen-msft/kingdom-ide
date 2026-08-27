# Review and editing

What the King can read, annotate and change once a plan has done work — the
review drawer, the diff, margin notes on code, editing files by hand, and what
the court can show him.

## The review drawer

**The King can read what a plan changed, not only what it said.**
The files rail
is **split**: the plan's own file tree above, and below it a **review drawer**
listing every file this plan has touched with its `+`/`−` counts. Both are on
screen at once, with a draggable divider between them, because the question "what
did my agent change?" is answered *against* "what is in this project?" — tabs made
holding both in view impossible. A row in either opens the file in the panel the
spyglass occupies: the drawer opens a side-by-side **diff**, and the tree opens
the file **whole**, because most files in a project have no diff at all and the
tree offers all of them.

The tree reads **the plan's workspace, not the city's checkout**, and that was a
correction rather than a choice. An isolated plan works in a worktree, so keyed
on the city the rail listed one copy of the project while the court edited
another — tolerable while it was read-only decoration, and not tolerable once a
row opens a file the King writes notes against: line 34 of the city's checkout is
not line 34 of the worktree, so the court would be sent an objection about code
it cannot see. `list_directory` is keyed on the plan for that reason, and
`SKIP_DIRS` gained `.kingdom` with it, since a city's worktree folder holds
entire further copies of the project.

Three decisions there are load-bearing. **The comparison is against
`merge-base(default, HEAD)`, not against `main`** — `git diff main` is
symmetric, so every commit that lands on main while an agent works renders as a
deletion *by the plan*, and the King opens the drawer to review his agent and is
shown files it never touched. A test in `review.rs` pins that against a real
repository. **It reads the plan's own workspace**, which is the worktree rather
than the city, and it counts committed, uncommitted and untracked work alike,
because a plan's checkout is normally in all three states at once and a drawer
showing only commits would be empty for most of a plan's life. And **the rows
arrive already paired**: deciding which deletion sits opposite which insertion
needs the differ that knows a replacement was a replacement, so `review.rs` does
it and the browser renders two columns without re-deciding anything — a flat
sequence of tagged lines would be mispaired on any uneven replace.

The diff, the source view and the spyglass are **alternatives for one panel**,
held in a single `Aside` value in `conversation.rs`: opening any closes the
others, because there is one signal holding one value rather than booleans that
must remember to close each other. The transcript is deliberately outside that
decision — it is not one of the alternatives, it is the thing they are
alternatives beside, and the panel is always to its **right** rather than stacked
above it. It used to stack below 1100px, which put a diff between the King and
the chamber header and pushed the transcript off the bottom of the screen.

**Any of the three can take the conversation's room as well as its own.** A
panel 640px wide is not enough to review several files against each other, and
while he is doing that the transcript is not what he is reading. So a **Focus**
chip in the panel's bar turns `.chamber-body` from a row into a column: the
panel takes the full width above, and the conversation keeps the strip beneath
it. `Escape` leaves, as it leaves every overlay.

Three decisions there are load-bearing. It is **one flag for all three panels**,
remembered in `localStorage` — a width is about what a particular panel needs to
be legible, but focus is about what the King is doing, and clicking from a diff
to the file beside it does not change that. It is **gated on something being in
the slot** (`Aside::is_showing`), or a flag remembered from last visit would hide
the transcript of a chamber with an empty column, with the toggle that would put
it back living in a panel bar that is not on screen. And the **review margin and
the composer survive it** — that is the whole reason focus shortens the
conversation column instead of hiding it, since a review that cannot be sent from
the screen it was written on is not a review mode. The files rail survives for a
simpler reason: it is outside that box, and it is how reviewing several files
happens at all. What goes is the header and the log, and the log goes by
`display: none` so its scroll position is still there when he comes back.

One mechanical detail is worth knowing before changing it. Each panel sets its
width **inline** from the resizer's signal, and an inline style beats a class
rule — so focus does not override the width, it *removes* it: the closure yields
`Option<String>`, which is how tachys spells "reset this property", and no
`!important` appears on either side. The same move retired `--panel-width`, the
inline var a note composer used to size itself against; it was the one number
that claimed to know how much room there was, and it stopped being true the
moment the panel could grow without that signal moving. `.diff-stage` is now a
`container-type: inline-size` and the composers are `100cqi`, which asks the box
itself.

**And the diff shows both of its sides.** It did not, and the cause was one rule
rather than the layout: `.diff-grid` was `width: max-content` with columns of
`minmax(50%, 1fr)`, so the grid grew to the longest line and each column took
half of *that*. At the panel's 640px default one real line of this repository
resolved the columns to 856.8px each — the new side began at x=857 inside a box
640 wide, off screen until the King thought to scroll sideways. Short files were
fine, which is why it survived so long: the failure bites exactly on the real
code a diff is opened for. The rows now wrap (`minmax(0, 1fr)`, `pre-wrap`,
`overflow-wrap: anywhere`), and a wrapped pair stays level for free because a
grid row is as tall as its tallest cell — the part a hand-rolled two-pane view
gets wrong. A **Wrap** chip turns it off for a King who would rather scroll, and
the source view is deliberately untouched: `_source.scss` argues that wrapping
there breaks the correspondence between what he types and the line number he just
read, and that argument does not apply to a read-only pair of columns.

**And he can open the diff up.** Three lines of context either side of a change
say *what* moved and often not *where* — the function a hunk sits inside starts
above the first row drawn, so the King reads a rewritten line with no idea which
`fn` it belongs to. Every break between hunks is now a control strip that says
how many lines it is hiding and offers to reveal them: **↑ 20** for the lines
against the change below (the usual one, since that is where the signature is),
**↓ 20** to read on from the change above, and **Show all N** while what remains
fits one answer. The run **before the first hunk and after the last** gets a
strip too, which the panel never had at all — a change on line 400 used simply
to be the top of the panel, with no sign that 399 lines came first.

Four decisions there are load-bearing.

**The lines are fetched, not shipped with the diff.** Sending the whole file and
folding it in the browser would be simpler and would undo the cap the panel is
built around: `MOST_ROWS` exists because the cost of a diff is DOM nodes, and a
40,000-line file with one changed line is cheap today precisely because the
unchanged 39,990 never leave the server. So expansion is a request
(`plan_diff_context` → `review::context`), the sixth path where an outsider names
a file and the server opens it — held to `within_workspace` like the other five,
which the source-reading test now pins. One answer is capped at `MOST_CONTEXT`,
and that constant lives in `kingdom-core` rather than beside the reader, because
the browser decides whether to offer "show all" against the same number the
server enforces.

**Nothing is re-diffed to answer.** The region between two hunks is the same text
in both versions by construction — a grouped diff only ever breaks inside a run
of unchanged lines — so `context` takes the two slices and pairs them straight
across. It **checks** they match and refuses if they do not, because the ordinary
way to get there is the court rewriting the file between the panel fetching the
diff and the King pressing the button, and pairing two lines that no longer
correspond would put unrelated text opposite itself with nothing saying so. The
refusal reads *"this file has changed since it was compared"*, in the strip's own
place: it is a fact about those lines, not about the comparison.

**A truncated diff offers no control at all** (`FileDiff::may_expand`). Rows were
dropped part-way through a hunk, so its declared range no longer describes what
is on screen and a reveal computed from it would silently skip them. No button is
a better answer than a lying one, and the panel already says the comparison is
partial.

**A reveal is forgotten when the diff refetches.** The counts live in the `Gap`
component and clear when the gap it was handed moves — which is exactly when the
court has edited the file. Lines left standing across that would be text the King
is still reading and the workspace no longer holds. Revealed rows are otherwise
ordinary rows: they render through the same `Row` component the hunks do, so a
line he opened up takes a margin note the same way — which is usually why he
opened it.

What yields instead is the **cities rail**, which folds itself to a strip below
1250px (`app.rs::fold_rail_when_cramped`). A chamber can want four columns at
once, and that rail is the one the King has finished using by the time he is
reading a diff. Two things there are load-bearing: it **never writes to storage**,
so the stored flag stays his *preference* and widening the window gives back what
he chose rather than what the last resize left behind; and it defers to a choice
made at the current width (`rail_decided_at`), because otherwise opening the rail
on a laptop is undone by the very next resize — and window managers send a flurry
of them. Crossing the threshold is what makes that choice stale.

The drawer was also the first thing to open a plan's workspace directory and find
nothing there. `sample::starter_plans` builds a `Workspace` from `City::path`,
which is *relative* to the kingdom root, and hands it to a field documented as
absolute; `api::grounded` closes that at the one boundary that holds the root,
and a test pins it.

## Notes and edits on code

**And he can write in the margin of the code, not only of the plan.** Every line
of both panels takes a note. They gather into one **review** above the composer,
and one button sends the lot as a single `Speaker::User` turn: `ReviewNote` →
`annotate_file` → `send_file_notes` → `file_notes_as_decree`. That is
deliberately the same shape marginal notes on a *proposal* already had, part for
part, because it is the same act performed against code instead of prose — and
four of its decisions are carried over for their original reasons. The notes live
on the plan, so one typed and not sent survives a reload and a second tab. They
are kept out of the transcript and therefore out of `Plan::turns`, so a
half-written second thought cannot reach a model. `quote` travels beside `line`,
because a line number is a reference into a file about to be rewritten. And
`take_review_notes` drains rather than reads, so nothing can compose the decree
and leave the notes standing to be sent twice.

What is **not** carried over is where they live: on the `Plan` rather than on a
`Proposal`. These are written against work in progress, so they must survive
approval — reviewing what the court has *built* is the case they exist for, and by
then there is no standing proposal to hang them on. That is also why the two
margins are kept apart when both stand: a proposal note asks the court to revise
a document and propose again, a line note asks it to change code, and one decree
meaning two things is worse than two buttons.

Three smaller things are load-bearing. `send_file_notes` reuses `receive`, the
branch `say` already splits out, so a review sent into a working chamber queues
and is heard at the next round boundary with no second code path to get wrong.
The decree is **grouped by file and ordered by line** rather than left in the
order the notes were written, because a model given nine notes shuffled across
four files has to sort them before it can start — and the margin groups the same
way, so the King checks his review against something that reads in the order he
will be answered in. And a note on the **old** column of a diff carries
`NoteSide::Base` and is reported as "in the version before your changes": a note
on a deleted line is an ordinary review comment, and a bare line number would
point the court at whatever now occupies that position.

One behaviour is worth stating because it looks like an oversight. **A panel with
a composer open does not refetch.** Both panels otherwise follow the court's
edits, which is right while the King is only reading and wrong the moment he is
typing against line 34 — the lines would shift under him and the note would land
on something he never read.

**And he can change the file himself.** The source panel has two modes. *Notes*
is the panel above: every line takes a comment for the court. *Edit* replaces the
lines with one box, and he can save the file or delete it — `plan_file_text` →
`plan_write_file` / `plan_delete_file` over `edit.rs`. A mode of one panel rather
than a fourth `Aside`, because it is the same file in the same slot; a King who
spots a typo while reading should not have to close what he is looking at to fix
it. It is deliberately **not** offered on the diff: editing one column of a
comparison is ambiguous about which column, and the comparison goes stale under
the cursor as it is typed into.

Those routes are the fifth, sixth and seventh places an outsider names a path and
the server opens it, and they raise the stakes — the earlier four only *read* a
file the King should not see, and these overwrite or delete one. All seven go
through `within_workspace` and none has a resolver of its own, which a test now
pins by reading the source. They refuse a **settled** plan, as `annotate_file`
does and for its reason, and they deliberately do **not** consult `Permissions`:
that is what bounds the *court*, and gating it would mean the man reviewing a
proposal cannot fix the typo he just found in it.

Four decisions there are load-bearing.

**The buffer is fetched, not rebuilt from the rendered lines.** `FileText` is a
second type beside `SourceText` precisely so it can be whole and byte-exact where
that one is numbered and truncated. Joining `SourceText`'s lines back with `\n`
would need no request at all and would rewrite every CRLF file as LF and add or
remove a final newline, because those lines come from `str::lines()` — a
whole-file diff the King never asked for, landing in his agent's branch. A cap on
`FileText` would be the same class of harm: a truncated buffer saved back is a
file with its tail deleted, so a file too long to edit is *refused* rather than
part-shown.

**And the line endings are restored on the way out**, which is the same lesson
one layer lower and was found only by driving the real panel. A **DOM textarea
normalises CRLF to LF in its `value`**: the bytes reach the browser intact, the
King types one character, and what comes back has had every `\r` stripped by the
platform before any of Kingdom's code ran. The server-side round-trip test passed
throughout — it never went near a DOM. So `edit::write` gives the text the
convention the file on disk already had, guarded twice: only if the file *was*
CRLF, and only if what arrived has no `\r` at all, which is the signature of a
wholesale strip rather than of a deliberately mixed file. Nothing in the browser
can be trusted to preserve this, and nothing in the browser needs to.

**A save cannot overwrite work it never saw.** Every read carries a `FileStamp`
— length and an FNV-1a hash, the not-cryptographic-and-doesn't-need-to-be trick
`profile::hash` already uses — and a write or delete sends it back to be checked.
The King reads a file while his agent works in the same workspace, so without
this a save at the wrong moment silently destroys a round of the agent's work,
which is the exact collision this product exists to surface rather than to cause.
Optimistic rather than a lock, because a lock has to be released by something and
the something is a browser tab that may simply be closed. A missing file stamps
as `ABSENT`, which is what makes deleting an already-deleted file a refusal
rather than a silent success.

**Unsaved text is never dropped on the floor.** Dirty buffers are stashed by path
for the chamber's lifetime, so glancing at another file mid-edit and coming back
restores what was typed. No modal and no `confirm()` — there is nothing to lose,
so there is nothing to ask about. Edit mode also suspends the refetch, which is
the composer's rule above for a sharper version of its reason: text moving under
a cursor is worse than under a quote.

Two smaller things. A save appends a `NoteKind::Workspace` note, which the King
reads and the model never sees — notes are excluded from `Plan::turns` by design,
and the court finds out the honest way, since `patch` reads a file fresh on every
call and refuses an anchor that is no longer there. That note also lengthens the
transcript, which is the change signal the review drawer already refetches on, so
his own edit refreshes the drawer's counts by the route the court's edits use.
And a **delete** bumps a `revision` the files tree watches: that tree caches every
listing and deliberately never re-lists, so without it a deleted file would sit
in the rail forever. On delete only — a save changes no listing.

## Images and narration

**The court can see, and can be seen.** `read_image` was the tool that closed the
loop `browser_take_screenshot` opened, and it cost a domain change: `ToolOutcome`
carries images beside its text. That machinery is now used by the screenshot tool
directly (see above) and `read_image` remains for every *other* picture — a
diagram or a mockup already in the workspace. Two things about it are load-bearing
and easy to undo by accident. Images are *not* persisted — `store.rs` strips them,
because a plan's record is rewritten on every update and would otherwise grow by a
megabyte per screenshot forever. And chat-completions has no image part on a
tool result, so `copilot.rs` sends the picture as a following `user` message,
built only on the wire and never as a `Turn` — the Responses API is the real fix
and the comment there says so.

A model that cannot see is never offered `read_image` (`ToolSpec::for_model`,
beside the existing `can_act` narrowing). The vision flag is read from three
places in Copilot's `/models` payload because the catalogue is not ours; if it
ever reads as blind for everything, that is where to look.
`browser_take_screenshot` is the one tool that check does *not* withhold — it is
worth having either way, since the King sees the picture regardless — so it asks
`Sandbox::sighted` at run time instead.

**The King reads what the court said, not only what it ran.** A model narrates
the move it is about to make in the *same reply* as the tool calls, and that
sentence is the reason for the deeds under it. `ToolCall::narration` has carried
it since task 00110 and `copilot.rs::messages()` replayed it to the model, but
the chamber drew only the commands until 00200. It renders above the deed now, as
the header of the block rather than as a `chat-msg`: a bubble carries a speaker
column and a clock, which would present the preamble of an action as a separate
thing the court said.

The grouping is the part to keep straight. **Narration belongs to a reply, not to
a call** — `api.rs` records it on the first call of a batch and `None` on the
rest, so a reply asking for six things replays as one decision rather than six
deliberations. `conversation.rs::remark` honours that same shape, and it is drawn
in `Transcript`'s `<For>` body rather than inside `ToolCallLine`, because
`Question` and `Subagents` render tool calls too and a batch's first call can be
any of the three. Putting it in `ToolCallLine` would lose the sentence exactly
when the court explains *why it is stopping to ask you something*.

`Reasoning::text` rides beside it, collapsed to `thinking (N lines)` and not
rendered as markdown — reasoning is a stream of thought with stray `#` in it that
was never meant as formatting. It is deliberately ranked below the remark: a
remark is what the court chose to say, and reasoning is what it happened to think
on the way there.

**And so can the King.** A screenshot renders in the chamber, under the deed
that took it. The picture is *not* carried on the plan: `ToolOutcome::Done`
gained `artifacts` — workspace-relative **paths** beside the base64 `images` —
and `artifact.rs` serves the file back over `/plan/{id}/artifact/{*path}`,
resolved through the plan's own `Sandbox`. The two channels look alike and are
not, which is the thing to keep straight: `images` feed a model for one turn and
are stripped on save; `artifacts` feed the conversation and are persisted, which
is the only reason a reloaded chamber can show the picture again. Inlining the
bytes instead would have re-broken all three of the constraints above — the
store's, the provider's, and the watch socket's, which re-sends whole plans.

That route is the one place in Kingdom where an outsider names a file and the
server opens it, so it refuses rather than guesses: outside the workspace, or a
media type `read_image` would not accept, is a refusal. It must not become a
general file server for a plan's checkout.

The placeholder court deliberately includes a **failed plan**, a plan **mid
draft**, and one with a **proposal standing in front of the user**, because
those are states the UI exists to show — and the last is the one the product's
whole stance rests on. Do not "clean up" the sample data into a court of tidy
settled plans — it would make the most important visual states unreachable
during development. A test pins this.
