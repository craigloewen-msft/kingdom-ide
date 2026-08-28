# The agent loop

How a plan runs from decree to approval, and the failures the loop has been
taught to survive. Companion to [`AGENTS.md`](../AGENTS.md).

## How a plan runs

A prompt opens a plan under `Permissions::Propose`: the model may read, search
and run commands, and gets `patch` scoped to a single draft file — it cannot
change the project. It writes the plan to `.kingdom/draft.md` as it works, then
calls `propose_plan` with that path, which ends the turn and parks the plan in
`AwaitingReview` with a `Proposal` standing on it. The King either sends notes
back — ordinary `say` + `draft_plan` — or presses **Start with this**
(`approve_plan`): permissions widen to `Full` and the same conversation carries
on with tools it did not have.

Nothing is parked while he reads. The turn genuinely ends, so a server restart
mid-review loses nothing — the proposal is on the plan and the plan is on disk.
That is why `propose_plan` does not work like `ask_user_question`, which holds a
request open on a oneshot.

**Or he writes in the margin.** `proposal.rs` splits the plan into markdown
blocks and each takes a note; `annotate_proposal` records it, several may stand
at once, and `send_notes` drains them into **one** `Speaker::User` turn that
blockquotes each annotated block above the objection to it. Nothing new reaches
the model — no tool, no message kind, no prompt change. `Plan::propose` carries
the outgoing body onto the new proposal as `revises`, so the card can open on a
diff of what moved.

Three things there are load-bearing: notes live on the plan rather than in the
browser (a note typed and not sent must survive a reload) and are excluded from
`Plan::turns`, so half-written second thoughts never reach a model;
`ProposalNote` carries the annotated **text** beside the line number, because a
line is a reference into a document about to be replaced; and `send_notes`
reuses `receive`, so notes sent into a working chamber queue and are heard at
the next round boundary with no second code path.

**The draft is the mechanism, not a formality.** A scoped `patch`
(`Patch::for_draft`), a `<next_step>` cue appended to every successful draft
write, and a propose tool that takes a *path* rather than inline content. The
inline form failed measurably: a real plan spent 21 rounds and 33 tool calls and
never proposed, having settled the design by round 11 and then re-derived it
eight times. Nothing it decided was ever written down, so every round it faced
the same choice between emitting the whole plan from memory and looking a little
further — and looking always won.

**The plan is one document, filed when the worktree goes.** The draft lives at
`.kingdom/draft.md` inside the worktree, which is excluded from the repository
and deleted with the checkout. So `store::file_plan` copies it out to
`plans/<plan-id>--<slug>.md` in the King's profile: at approval, and again at
merge or archive for a plan never approved. The id makes the name unique, the
slug makes it readable, and `store::load` filters on the `json` extension so the
markdown sits beside the record without being mistaken for one.

The writing is **write-once**, which is load-bearing rather than tidy: filing
happens twice on the ordinary path, and between the two the court holds an
unrestricted `patch` and may rewrite its own draft. Without it, finishing a plan
would replace what the King agreed to with whatever the draft said at the end.
Two details: the draft must be read **before** the disposal (a test pins this
against real git), and an **in-place** plan has no teardown, so its draft is
deleted explicitly after filing — guarded on the filing having succeeded.

## When a reply fails

**An empty reply is not the end of a plan.** A reply with no content and no tool
calls is the *absence* of an answer, so `converse` asks again — up to
`MOST_ATTEMPTS`, with a short backoff, raced against the halt.
`ModelError::is_transient` says yes to `Empty` and `Transport` (which 5xx and
429 route to) and no to a refusal or a missing credential, because those are
answers and asking again only spends the King's quota to be told the same thing.

The harder half was that **nothing the King did could change the request.**
Failures are recorded as a `Note`, notes are excluded from `Plan::turns`, so
"keep going" rebuilt a byte-identical payload and got a byte-identical silence.
So an empty reply is noted as `NoteKind::EmptyReply` specifically and
`follows_silence` finds it — walking *past* the King's own words, because
`receive` appends them after the note. What it yields is `Brief::aside`:
rendered as a `system` message, never as a `Turn`, never in the transcript, and
never in the King's voice.

**And a reply is not called empty when it was not.** Three paths funnelled into
that message and are now named apart: tool calls dropped for want of an `id` or
a `name` (counted and reported), `content` sent as an array of parts rather than
a string (now read), and a reply carrying only reasoning (named as that, since
the fix is the effort setting). `answer_from` also logs a bounded slice of the
body on any parse failure.

## What a request may weigh

**A request too large to send is not the end of a plan either.** A plan died on
`413 Request Entity Too Large` and stayed dead: 413 is a 4xx, `Refused` is
fatal, and every "keep going" rebuilt the same body. The cause was that a
picture was replayed forever — six screenshots were 4.02 MB of a 5.3 MB body,
each already answered rounds earlier.

The rules now:

- **Images ride only while they are new** (`RECENT_REPLIES`), unconditionally
  rather than under pressure: a conversation that merely happens to fit today
  would otherwise keep every picture until the day it does not.
- **A dropped picture is admitted** in the tool result. A model that believes it
  can still see a screenshot describes it from memory and is confidently wrong;
  one told the attachment is gone simply takes another. A blind model hears
  nothing, since it never had the image.
- **`Budget` bounds the assembled body.** `MOST_REPLAYED` caps one result at
  12 KB; nothing capped the sum, and 300 results comfortably under that cap
  still added up to a refusal. Over budget, `shedding` drops in order of what it
  costs to lose: stale pictures, then old `reasoning.opaque`, then the tails of
  old results.
- **`opaque` is never taken from a live reply** (`LIVE_REPLIES`), because
  `Reasoning::without_opaque` records that a gateway *silently discards*
  thinking whose signature did not come back — where trimming a result says so.
  It *is* dropped beyond that window in the initial `Shedding`, not only under
  pressure: audited across four healthy plans, re-sent signatures were the
  single largest thing in a request (638 KB of 1.15 MB on one) and 100% of it
  was already dead by that constant's own definition.
- **The tally counts wire bytes, not `str::len`.** The body is JSON and tool
  output is mostly quotes and newlines, so raw lengths under-reported a real
  transcript by 1.69× — a request the budget called 3 MB went out at 5.1 MB.
  `escaped_len` is counted rather than fudged with a constant, because the ratio
  is entirely content-dependent.

`Budget::FULL` is a **guess** and the design assumes so: the only hard fact is
that 5.3 MB was refused. So 413 gets `ModelError::TooLarge` — not transient,
because resending an identical body is pointless, but `is_shrinkable`. `converse`
halves the budget and asks again with no backoff, down to a floor past which the
honest answer is to fail and say what was too big. The body is measured before
it goes so a 413 can report its own size.

The chamber header reports **both** limits: `ContextUsage` carries `bytes`
beside `tokens`, in the tooltip rather than beside the bar — it is the number to
reach for when a turn fails for no visible reason. `bytes: 0` reads as
*unmeasured* and draws nothing, which is what every older plan record loads as.

## What a round costs

A round is the unit of cost. `copilot::armed` has set `parallel_tool_calls` all
along, and nothing had ever asked a model to use it: across four plans, 702
rounds produced 840 tool calls — 1.20 per round, 84% carrying exactly one, and
not a single round carrying three. `BATCHING` is the sentence that asks. Its
second clause — *wait when a call needs an earlier result* — is as load-bearing
as the ask, because batching a read with the write that depends on it trades a
token bill for a correctness bug.

A prompt line is a weak instrument, and the honest caveat is that it can lose to
a stronger prior.

## Questions, attention, and the King's voice

**A question is asked one at a time.** `ask_user_question` may put up to four;
the chamber used to render all of them live, so the first click settled the call
and the other answers were discarded without the court learning they had been
asked. They are now put one at a time with Back/Next and **Submit** on the last,
and `compose_answer` folds the lot into the single `String` the parked oneshot
takes.

Every question ends in Submit, including a lone one. That buys three things:
`multi_select` becomes answerable at all, an option and a sentence of his own
can stand together, and the set is answered as one act. A single question still
sends its **bare answer** with no scaffolding. A question left alone is named as
`(no answer)` rather than dropped — silence and omission look identical to a
model, which then fills the gap with the guess it stopped to avoid making.

His place in the wizard is local state and survives the push socket because
`Transcript`'s `<For>` is keyed by `(index, entry_version)` and an in-flight
call holds version 1 throughout. A change to that key would silently send him
back to question one every time the court did anything.

**The rail says whose move it is.** A plan parked on a question is still
`PlanStatus::Drafting`, so a badge read off the status said "Drafting" in
working green. `Attention` answers the different question, and is deliberately
**not** a sixth `PlanStatus`: a status is where a plan is in its life, and a
sixth variant would ripple through `ALL`, the map legend, `is_settled` and every
match on plan state to say one word. `Plan::wants_attention` is the single
definition, read by the rail, the chamber header and the pulse alike.

The same argument runs once lower down: "Drafting" was also one word for an
agent *reading* code to draw a plan up and an agent *changing* files under an
approved one — the two halves this product's stance rests on. That distinction
is `Permissions`, so a live plan is badged **Exploring** or **Working** by its
remit. Attention outranks it: an agent with hands that stops to ask still reads
"Question". The ranking lives in `sidebar::badge_for` and nowhere else, and the
chamber header calls the same function.

Two traps, both the same shape: **approval moves the permissions and nothing
else**, the status being `Drafting` on both sides of the grant. So the remit had
to join the rail's `<For>` key, and it had to join `PlanPulse`, or a plan
approved in another tab would never have been heard about.

**A second socket carries the rail, and it carries a digest.** Whole plans on
the wire are free only for a channel keyed *per plan*, where one watcher is
looking at exactly what is sent. The rail asks "which of my thirty plans needs
me?", and answering it that way would wake every open tab with every transcript
on every round to repaint a badge. So `/watch/kingdom` carries `PlanPulse` — id,
city, title, status, what it is doing, what stage it is at, what it wants, and
when it last moved — **deduped** against the last pulse sent for that plan.
Dedupe narrows what is *sent*, never what a message *says*, so a listener that
falls behind has missed only intermediate states.

Two details are load-bearing. The badge cache in `KingdomState` stores an
`Option` *inside* the map, because "the server says this plan wants nothing" and
"nothing has been said about this plan" must stay different answers. And **both**
sockets write it — the chamber's, which computes it, and the rail's, which is
told — and they cannot disagree because `wants_attention` is one definition.

The age under each row is on the pulse rather than worked out in the browser,
because the transcript would answer it only for the *one* plan whose chamber is
open; every other row would report the age of the opening fetch forever, which
looks right and is wrong.

**The King can speak over a running turn, and can stop one.** The composer is
never disabled. Words sent mid-turn are queued on the plan (`Plan::queued`, kept
out of the transcript and so out of `Plan::turns`) and heard by
`Plan::hear_queued` at the top of the next round — the one moment where nothing
is half-done. Splicing them in mid-deed would hand the model a conversation in
which a tool call and its result are separated by something nobody said at the
time. `converse` drains on its two normal exits, and deliberately *not* on its
failure exits, where re-entering the loop would burn the round budget against a
model that just errored.

**Stop** signals `turns::halt`, which `converse` races against its two long
awaits. Cooperative rather than an abort, so the code that clears the busy mark
and settles the in-flight deed still runs — the difference between a stopped
plan and a wedged one. The interrupted deed is closed as `ToolOutcome::Refused`,
as `store::reconcile` closes a deed the server died during, because an unsettled
call is replayed to the model as still running forever. The plan lands in
`AwaitingReview`, not `Failed`. A halted `bash` keeps its process; the `JOBS`
handle survives for a later turn.

`turns.rs` answers a narrower question than `Plan::working_on`, and the gap is
load-bearing. `working_on` is a *description* that survives a restart and a
panic; the registry is emptied by a guard on every exit path. `say` branches on
the registry, so a plan whose busy mark outlived its turn still takes the direct
path and is un-wedged by being spoken to — branching on `is_busy()` would queue
every message behind a turn nothing would ever drain. `stop_plan` reads the same
absence as its diagnosis, which is why Stop is also the cure that used to need a
server restart.

## The `Propose` boundary

**It is a statement of the job, not a sandbox.** It keeps `bash`, which
`Sandbox::root` is explicit about not containing — a command naming an absolute
path writes wherever it likes. Withholding it would buy a guarantee Kingdom
cannot keep while costing the model `git log`, `cargo tree` and running the
failing test it proposes to fix. What it narrows is `patch`: offering the
editing tool unrestricted says *you may change the project*, and offering it
scoped to a draft says *you may write down what you would change*.
`system_prompt.rs` says plainly that the shell is a boundary the model is
trusted to keep rather than one that is enforced. Closing that properly means an
OS-level sandbox, which is a deliberate later decision.

## The prompt is Phoenix IDE's

The prompt and the tool descriptions were ported wholesale because its agents
demonstrably answered better on the same work. Three rules come with that.

*The order is the point.* The remit renders **last**, after the project's
`AGENTS.md` and the skill catalogue, because it is what the model must still be
holding when it picks its first tool. Kingdom used to render it early and then
bury it under up to 64 KB of guidance. Anything appended after the remit puts
that distance back, and a test pins the ordering.

*Phoenix wins on wording, never on facts about Kingdom.* Where a Phoenix string
would describe behaviour Kingdom does not have, the behaviour is authoritative:
its `bash` description is trimmed of the `label` and `since` arguments this tool
does not take. The shared-machine block goes the other way — no Phoenix
counterpart, kept anyway, because several agents on one machine is Kingdom's own
subject. Both departures are tested.

That block is also the clearest live instance of the rule below it. It used to
tell every plan to "pick an unusual free port rather than taking a project's
default"; `namespaces/net.rs` then gave an isolated plan a loopback of its own,
which made the advice false — so it is gone. An isolated or sealed plan is now
told plainly that `:3000` here is its own, and a plan on the host network keeps
only the half that was never about port numbers: do not kill what you did not
start, and say so if a port is taken.

*A claim is kept only while it is true.* The mermaid hint was **not** ported at
first, because Kingdom had no markdown renderer and the claim had once cost a
plan 25 of its 30 reasoning blocks arguing with the prompt.
`components/markdown.rs` is that renderer, so the sentence is back and the test
that once forbade the word now requires it. If the renderer ever goes, both go
with it.

*What was deleted with it.* The house blocks on ending a turn, on the cost of
re-reading and on writing tests are gone, and so is the `NUDGE` machinery that
sent a narration-only reply back round. A reply with prose and no tool call now
simply ends the turn, as it does in Phoenix.
