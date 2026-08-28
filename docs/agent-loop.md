# The agent loop

How a plan runs from decree to approval, and every failure mode the loop has
been taught to survive. Companion to [`AGENTS.md`](../AGENTS.md); the
blow-by-blow of each change is in [`../tasks/`](../tasks).

## How a plan runs

A prompt opens under `Permissions::Propose`: the
model may read, search, and run commands, and gets `patch` scoped to a single
draft file — it cannot change the project. It writes the plan to
`.kingdom/draft.md` as it works, then calls `propose_plan` with that path, which
ends the turn and parks the plan in `AwaitingReview` with a `Proposal` standing
on it. The user either sends notes back — ordinary `say` + `draft_plan`, the
composer's own path — or presses **Start with this**, which is `approve_plan`:
the permissions widen to `Full` and the same conversation carries on with tools
it did not have.

**Or he writes in the margin.** `proposal.rs` splits the plan into markdown
blocks, and each one takes a note: `annotate_proposal` records it on the
`Proposal`, several may stand at once, and `send_notes` drains them into **one**
`Speaker::User` turn that blockquotes each annotated block above the objection to
it. Nothing new reaches the model — no tool, no message kind, no prompt change.
The court revises its draft and calls `propose_plan` again exactly as it always
did, and `Plan::propose` carries the outgoing body onto the new proposal as
`revises`, so the card can open on a diff of what moved. A switch in the head
turns that off; it is not drawn at all on a first proposal, which has nothing to
be read against.

Three things there are load-bearing. The notes live on the plan rather than in
the browser, for the reason `queued` does — a note typed and not sent must
survive a reload — and they are excluded from `Plan::turns` the same way, so
half-written second thoughts never reach a model. `ProposalNote` carries the
annotated **text** beside the line number, because a line is a reference into a
document that is about to be replaced and the quote is the half that cannot go
stale. And `send_notes` reuses `receive`, the branch `say` already splits out, so
notes sent into a working chamber queue and are heard at the next round boundary
with no second code path to get wrong.

`revises` is a whole body rather than a stored diff: the diff is computed for
display and thrown away, and keeping one would be a third rendering of prose that
already exists twice and is free to drift from both — the liability recorded
against the old `approved/` ledger, further down.

Nothing is parked while they read. The turn genuinely ends, so a server restart
mid-review loses nothing — the proposal is on the plan and the plan is on disk.
That is the whole reason `propose_plan` does not work like `ask_user_question`,
which holds a request open on a oneshot; the module docs there spell it out.

**The draft is the mechanism, not a formality.** This is Phoenix's shape, ported
part for part: a scoped `patch` (`PatchTool::for_task_proposal_drafts` there,
`Patch::for_draft` here), a `<next_step>` cue appended to every successful draft
write pointing back at the propose tool, and a propose tool that takes a *path*
rather than inline content.

It exists because the inline form failed in a specific, measured way. A real
plan asked for a file-tree view spent 21 rounds and 33 tool calls and never
proposed at all: its reasoning had settled the design by round 11 and then
re-derived it eight times, renaming the same component on each pass. Nothing it
decided was ever written down, so every round it faced the same choice between
emitting the whole plan from memory and looking a little further — and looking
always won. Giving it somewhere to put the plan is what fixes that; a paragraph
of prose telling it to stop looking is what Kingdom tried instead, and Phoenix
sends no such paragraph.

**The plan is one document, and it is filed when the worktree goes.** The court
drafts to `.kingdom/draft.md` inside its own worktree and revises it there as it
works — that file *is* the plan. But `.kingdom/` is excluded from the
repository, so the draft is never committed, and `git worktree remove --force`
deletes it with the checkout. So `store::file_plan` copies it out to
`plans/<plan-id>--<slug>.md` in the profile: at approval, and again at merge or
archive for a plan that was never approved. The id makes the name unique (slugs
collide), the slug makes it readable in `ls`, and `store::load` filters on the
`json` extension so the markdown sits beside the record without being mistaken
for one.

This is Phoenix's `tasks/` directory with its one liability removed. Phoenix
commits its task files into the project; Kingdom files the plan into the King's
profile, because a plan is Kingdom's bookkeeping *about* a repository rather
than that repository's content.

The writing is **write-once**, and that is load-bearing rather than tidy:
filing happens twice on the ordinary path, and between the two moments the court
holds an unrestricted `patch` and may rewrite its own draft freely. Without it,
finishing a plan would replace what the King agreed to with whatever the draft
said by the end. A failed write costs the document rather than the approval or
the merge, and the finish tries again.

Two details worth keeping straight. The draft must be read **before** the
disposal — `worktree.rs` has a test that pins this against real git, because
after the teardown there is nothing left to read and the patch cannot carry an
excluded file. And an **in-place** plan has no teardown, so its draft is deleted
explicitly after filing — guarded on the filing having succeeded, since that is
the only remaining copy.

This replaced an `approved/<plan-id>.md` ledger holding a second rendering of
the same prose. Its stated justification was that a revision after approval
replaces the standing proposal — but `Plan::propose` cannot be reached once
permissions widen (`propose_plan` is not offered at `Full` and refuses there
anyway), so that loss was not reachable. The guarantee it offered is kept by the
filed plan. Nothing already on disk was deleted, and `profile::migrate` still
brings `approved/` forward.

## When a reply fails

**An empty reply is not the end of a plan.**
A reply that arrives with no
content and no tool calls is the *absence* of an answer rather than an answer,
and `converse` now asks again -- up to `MOST_ATTEMPTS`, with a short backoff,
raced against the halt like every other long await. Only failures a retry could
fix are retried: `ModelError::is_transient` says yes to `Empty` and `Transport`
(which a 5xx and a 429 now route to) and no to a refusal or a missing
credential, because those are considered answers and asking again only spends
the user's quota to be told the same thing.

The half that made this feel unfixable was never the first failure, though — it
was that **nothing the King did could change the request.** `settle` records a
failure as a `Note`, notes are deliberately excluded from `Plan::turns`, so
"keep going" rebuilt a byte-identical payload and got a byte-identical silence.
A real plan died this way three times in ninety seconds with its window 10%
full. So an empty reply is noted as `NoteKind::EmptyReply` specifically, and
`follows_silence` finds it — walking *past* the King's own words, because
`receive` appends them after the note and reading `transcript.last()` would
answer `false` on precisely the turn this exists to catch. A test pins that
sequence. What it yields is `Brief::aside`: rendered on the wire as a `system`
message, never as a `Turn`, never in the transcript, and never in the King's
voice — the same containment `copilot::shown` gives an image, for the same
reason.

**And the reply is no longer called empty when it was not.** Three paths funnelled
into that one message: tool calls dropped silently for want of an `id` or a
`name` (now counted and reported, naming what was unreadable), `content` sent as
an array of parts rather than a string (now read — `as_str()` on an array is
`None`, which became `""`, which became "empty reply"), and a reply carrying only
reasoning (now named as that, since the fix is the effort setting rather than the
gateway). `answer_from` also logs a bounded slice of the body on any parse
failure: this module logged *nothing*, which is why diagnosing the original bug
ended in "unknowable".

**And a request too large to send is not the end of one either.** The same
failure through a different door: a plan died on `413 Request Entity Too Large`
and stayed dead, because 413 is a 4xx, `Refused` is fatal, and every "keep
going" rebuilt a byte-identical body from the same transcript and was rejected
identically. Three deaths in ninety seconds.

The cause was that **a picture was replayed forever**. `read_image` puts base64
on the live plan, `store::save` strips it on the way to *disk* but nothing takes
it out of memory, and `copilot::messages` sent `shown()` for every tool call in
the transcript on every round. Six screenshots became 4.02 MB of a 5.3 MB body,
each already looked at and answered rounds earlier. So images now ride only while
they are new (`RECENT_REPLIES`), which is the code catching up with what
`ToolArtifact`'s own doc already claimed — `images` is "what the model was shown,
true for one turn".

Three things there are load-bearing. The window is **unconditional** rather than
a response to pressure: a conversation that merely happens to fit today would
otherwise keep every picture until the day it does not, and the King would meet
this mid-investigation instead of never. A dropped picture is **admitted** in the
tool result, for the reason `replayed` marks a truncation — a model that believes
it can still see a screenshot describes it from memory and is confidently wrong
about the UI it was asked to verify, while one told the attachment is gone simply
takes another. And a **blind** model hears nothing either way, since it never had
the image and the notice would only invite a screenshot it cannot read.

Beyond that, `Budget` bounds the assembled body. `MOST_REPLAYED` already capped
one result at 12 KB; nothing capped the sum, which is how 300 results comfortably
under that cap still added up to a refusal. Over budget, `shedding` drops in
order of what it costs to lose: stale pictures, then old `reasoning.opaque`, then
the tails of old results. `opaque` is the delicate one — `Reasoning::without_opaque`
records that a gateway *silently discards* thinking whose signature did not come
back, so it is never taken from a reply recent enough to still be live
(`LIVE_REPLIES`), and a test pins that under deliberate pressure.

The number in `Budget::FULL` is a **guess**, and the design assumes so. The only
hard fact is that 5.3 MB was refused; the real limit is unpublished and varies.
So 413 gets `ModelError::TooLarge` — not transient, because resending the
identical body is pointless, but `is_shrinkable`, which is a different question
with a different remedy. `converse` halves the budget and asks again with no
backoff (nothing is unwell; the next request is simply smaller), down to a floor
past which the honest answer is to fail and say what was too big. Being wrong in
either direction is survivable, which is what makes a guess acceptable here.

Two smaller things. The body is measured before it goes so a 413 can report its
own size, because "Request Entity Too Large" with no number attached is what made
this feel unknowable. And `shedding` tallies each reply *once* and then asks that
tally repeatedly — weighing candidates by re-walking the transcript re-serialised
every tool call's arguments a dozen times over, which cost 110 ms on a real plan
against 3 ms for the whole assembly.

The tally counts **wire** bytes, not `str::len`, and that distinction bit once
already during this very change. The body is JSON, where every quote and newline
costs an extra byte, and tool output is mostly quotes and newlines: counting raw
lengths under-reported the real transcript by 1.69x, so a request the budget
called 3 MB went out at 5.1 MB — the size that was refused to begin with. A
budget with no headroom is not a budget. `escaped_len` is counted rather than
fudged with a constant, because the ratio is entirely content-dependent (base64
escapes to nothing, a build log nearly doubles), and a test now pins the estimate
against a genuinely assembled body.

The chamber header now reports **both** limits. `ContextUsage` carries `bytes`
beside `tokens` — the measurement `copilot.rs` already took so a 413 could name
its own size, kept rather than discarded — and the header's tooltip says what the
last request weighed on the wire. The King watched 257k of 1M tick by while the
gateway refused him on bytes, and the bar was telling the truth about the wrong
quantity. It is in the tooltip rather than beside the bar because it is the
number to reach for when a turn fails for no visible reason, not one to watch;
`bytes: 0` reads as *unmeasured* and draws nothing, which is what every plan
recorded before the field existed loads as.

**A request stopped carrying thinking nobody was using.** The other half of the
same lesson as the pictures above, found by auditing four plans that did *not*
die. `shedding` consulted `LIVE_REPLIES` only when a body exceeded
`Budget::FULL`, so a conversation comfortably under budget re-sent every signed
reasoning blob it had ever produced, forever. On real plans that was the single
largest thing in a request — 638 KB of 1.15 MB on one, 651 KB of 1.45 MB on
another — and in both cases **100%** of it older than `LIVE_REPLIES` and
therefore dead by that constant's own definition. Cumulatively it was 46–53% of
everything those turns put on the wire.

So `opaque_from_reply` is now set in the *initial* `Shedding`, exactly as
`images_from_reply` already was, and for the reason stated there: a conversation
that merely happens to fit today would otherwise keep every blob until the day it
does not. The pressure loop no longer needs a step for signatures — the bound it
would have walked to is already applied — and it must never reach past it, since
dropping a *live* signature is silent (`Reasoning::without_opaque`) where
trimming a result says so.

## What a round costs

**And a round is now understood to be the unit of cost.** `copilot::armed` has
set `parallel_tool_calls` all along, with a comment explaining that it saves
(N−1) round trips whenever a model recognises a batch as independent — and
nothing had ever asked a model to use it. Across those same four plans, 702
rounds produced 840 tool calls: 1.20 per round, 84% of rounds carrying exactly
one, and not a single round in any of them carrying three. `BATCHING` is the
sentence that asks, kept for the `SHARED_MACHINE` reason rather than Phoenix's
(Phoenix sends no such line; this is a fact about Kingdom's transport). Its
second clause — *wait when a call needs an earlier result* — is as load-bearing
as the ask, because batching a read with the write that depends on it trades a
token bill for a correctness bug.

The honest caveat is that a prompt line is a weak instrument: `workspace_block`'s
own second clause fixed the redundant-`cd` habit outright and then lost to a
stronger prior the moment a plan worked across two repositories, and the same
audit found 71% of one plan's `bash` calls had the prefix back. Measured after
the change, a four-file read arrived as one round of four calls.

**A screenshot no longer costs two rounds.** `browser_take_screenshot` returned a
path and left the model to spend a whole round on `read_image` for a file it had
just asked to be created, reasoning that "the bytes must not be spent on a model
that may not need them". The records disagreed: across a real kingdom, 131
screenshots drew 128 `read_image` calls. 98% is not a model deciding, and the
call now hands back the picture with the path.

Nothing about the weight changes — `shown` puts an image on the wire only while
it is within `RECENT_REPLIES`, so a picture delivered this way decays exactly as
one delivered by `read_image` did, one round earlier and one round cheaper. The
half of the old reasoning that was right is kept: a **blind** model still gets
the path alone. That check could not live in `ToolSpec::for_model`, which
narrows by withholding a tool — this tool is worth offering either way, since the
King sees the picture regardless — so `Sandbox` carries `sighted` and `api.rs`
sets it from the same `Model::can_see` the tool list is built from.

## Questions, attention, and the King's voice

**A question is asked one at a time, and the rail says one is standing.** Two
halves of the same fault. `ask_user_question` may put up to four questions, and
the chamber used to render all of them with every option live — so the *first*
click sent its own label and settled the call, and the other three answers were
discarded without the court ever learning they had been asked. They are now put
to the King one at a time (`Question` in `conversation.rs`), with Back and Next
between them and **Submit** in place of Next on the last. `compose_answer` folds
the lot into the single `String` the parked oneshot takes.

Every question ends in Submit, including a lone one. That costs the old
one-click path a second click and buys three things worth more: `multi_select`
becomes answerable at all (it had been in the schema, and read by nothing), an
option and a sentence of his own can stand *together*, and the set is answered
as one act. A single question still sends its **bare answer** with no
scaffolding, so the far side reads exactly as it always did — the mock's "You
chose X" path and the tool's own test needed no change. Several are labelled and
kept in the order asked, and a question he left alone is named as `(no answer)`
rather than dropped: silence and omission look identical to a model, which then
fills the gap with the guess it stopped to avoid making.

The King's place in the wizard is local state, and it survives the push socket
for a reason that is not obvious: `Transcript`'s `<For>` is keyed by
`(index, entry_version)` and an in-flight call holds version 1 throughout, so
deeds landing elsewhere re-render the list without rebuilding that row. A change
to that key would silently send him back to question one every time the court
did anything.

**And the rail could not have told him.** A plan parked on a question is still
`PlanStatus::Drafting` — asking moves nothing — so a badge read off the status
said "Drafting" in the working green, the same thing it says for a plan happily
running a build. `Attention` answers the different question *whose move is it*,
and is deliberately **not** a sixth `PlanStatus`: a status is where a plan is in
its life, and a sixth variant would ripple through `ALL`, the map legend,
`is_settled` and every match on plan state to say one word.
`Plan::wants_attention` is the single definition, read by the rail, the chamber
header and the pulse alike.

**The same argument runs a second time, one question lower down.** "Drafting"
was also the same word for an agent reading the code to draw a plan up and an
agent changing files under a plan the King accepted — the two halves this
product's whole stance rests on. That distinction is `Permissions`, which is
`Propose` from the moment a decree opens a plan and widens to `Full` exactly
once, in `api::approve_plan`. So a live plan is badged **Exploring** or
**Working** by its remit, and `PlanStatus::Drafting` no longer reaches the rail
as a word at all. Attention still outranks it: an agent with hands that stops to
ask still reads "Question", because whose move it is beats what stage the work
is at.

The ranking, in `sidebar::badge_for` and nowhere else — whose move it is, then
what stage, then where the plan is in its life. The chamber header calls the
same function, so the two surfaces the King reads one plan from cannot drift.

Two traps came with it, both of the same shape: **approval moves the permissions
and nothing else.** The status is `Drafting` on both sides of the grant. So the
remit had to join the rail's `<For>` key — a row reused on an unchanged key would
have read "Exploring" for the whole life of the accepted work — and it had to
join `PlanPulse`, or a plan approved in another tab would never have been heard
about, the pulses being deduped on a digest that had not changed.

**A second socket carries it, and it carries a digest.** `events.rs` argues at
length that whole plans on the wire are free — and that argument holds only for a
channel keyed *per plan*, where one watcher is looking at exactly what is sent.
The rail asks "which of my thirty plans needs me?", and answering it the same way
would wake every open tab with every transcript on every round to repaint a
badge. So `/watch/kingdom` carries `PlanPulse` — id, city, title, status, what it
is doing, what stage it is at, what it wants, and when it last moved — and is
**deduped** against the last pulse sent for that plan. The digest makes a message
cheap; the dedupe makes
most rounds send nothing at all. Dedupe narrows what is *sent*, never what a
message *says*: a pulse is a whole digest, so a listener that falls behind has
still missed only intermediate states, which is the same property that makes lag
survivable on the plan channel.

Two details there are load-bearing. The badge cache in `KingdomState` stores an
`Option` *inside* the map, because "the server says this plan wants nothing" and
"nothing has been said about this plan" must stay different answers — a question
answered in another tab pulses `None`, and treating that as silence would fall
back to a transcript fetched before the answer and go on showing a question
nobody is asking. And **both** sockets write it: the chamber's, which holds the
whole plan and computes it, and the rail's, which is told. They cannot disagree,
because `wants_attention` is the one definition on both ends.

**Why the age is on the pulse rather than worked out in the browser.** The rail
also draws, under each plan, how long since anything happened in it
(`Plan::last_activity` → `PlanPulse::last_activity`, cached beside the badge in
`KingdomState::last_activity`). The transcript would answer that too — and only
for the *one* plan whose chamber is open, because that is the only plan the
browser is ever sent whole. Every other row would report the age of the opening
fetch, forever, which is worse than reporting nothing: the number would look
right and say an agent had gone quiet when it had not. It is also the one field
that moves on its own, and so the only cost this feature charges the dedupe — a
small one, since a plan actually working already pulses about once per deed as
`working_on` changes with it, and an idle plan publishes nothing at all.

**The King can speak over a running turn, and can stop one.** The composer is
never disabled. Words sent mid-turn are queued on the plan (`Plan::queued`, kept
deliberately *out* of the transcript and therefore out of `Plan::turns`) and
heard by `Plan::hear_queued` at the top of the next round — the one moment where
nothing is half-done. Splicing them in mid-deed would hand the model a
conversation in which a tool call and its result are separated by something
nobody said at the time. `converse` also drains on its two normal exits, because
otherwise words queued just as a turn ended would be waited on by nobody; it
deliberately does *not* drain on its failure exits, where re-entering the loop
would burn the round budget against a model that just errored.

**Stop** signals `turns::halt`, which `converse` races against its two long
awaits with `tokio::select!`. Cooperative rather than an abort, so the code that
clears the busy mark and settles the in-flight deed still runs — the difference
between a stopped plan and a wedged one. The interrupted deed is closed as
`ToolOutcome::Refused`, exactly as `store::reconcile` closes a deed the server
died during and for the same reason: an unsettled call is replayed to the model
as still running, forever. The plan lands in `AwaitingReview`, not `Failed` —
nothing failed, and `Failed` is the status the chamber offers a retry against. A
halted `bash` keeps its process, as Phoenix's does; the `JOBS` handle survives
for a later turn to peek at or kill.

`turns.rs` answers a narrower question than `Plan::working_on`, and the gap is
load-bearing. `working_on` is a *description* that survives a restart and a
panic; the registry is emptied by a guard on every exit path. `say` branches on
the registry, so a plan whose busy mark outlived its turn still takes the direct
path and is un-wedged by being spoken to — branching on `is_busy()` would queue
every message behind a turn nothing would ever drain, turning today's
recoverable wedge into a permanent one. `stop_plan` reads the same absence as
its diagnosis and repairs such a plan, which is why Stop is also the cure that
used to need a server restart.

## The `Propose` boundary

**It is a statement of the job, not a sandbox.** It keeps
`bash`, which `Sandbox::root` is explicit about not containing — a command that
names an absolute path writes wherever it likes. Withholding it would buy a
guarantee Kingdom cannot keep while costing the model `git log`, `cargo tree`
and running the failing test it is proposing to fix. What it narrows is `patch`:
offering the editing tool unrestricted says *you may change the project*, and
offering it scoped to a draft says *you may write down what you would change*.
`system_prompt.rs` says the rest in words, and says plainly that the shell is a
boundary the model is trusted to keep rather than one that is enforced. Closing
that properly means an OS-level sandbox, which is a deliberate later decision.

## The prompt is Phoenix IDE's

The prompt and the tool descriptions were ported wholesale
because its agents demonstrably answered better on the same work. Three things
about that are worth keeping straight.

*The order is the point.* The remit renders **last**, after the project's
`AGENTS.md` and the skill catalogue, because it is what the model must still be
holding when it picks its first tool. Kingdom used to render it early and then
bury it under up to 64 KB of guidance. Anything appended after the remit puts
that distance back, and a test pins the ordering.

*Phoenix wins on wording, never on facts about Kingdom.* Where a Phoenix string
would describe behaviour Kingdom does not have, the behaviour is authoritative:
its `bash` description is trimmed of the `label` and `since` arguments this tool
does not take. `SHARED_MACHINE` goes the other way — no Phoenix counterpart,
kept anyway, because several agents on one machine is Kingdom's own subject.
Both departures are tested.

The mermaid hint is the case that shows the rule working in both directions. It
was **not** ported at first, because Kingdom had no markdown renderer and the
claim had once cost a plan 25 of its 30 reasoning blocks arguing with the
prompt; the comment where it belonged said "restore this the day a renderer
exists". `components/markdown.rs` is that renderer, so the sentence is back and
the test that once forbade the word now requires it. If the renderer ever goes,
both go with it.

*What was deleted with it.* The house blocks on ending a turn, on the cost of
re-reading, and on writing tests are gone, and so is the `NUDGE` machinery in
`api.rs` that sent a narration-only reply back round. A reply with prose and no
tool call now simply ends the turn, as it does in Phoenix.
