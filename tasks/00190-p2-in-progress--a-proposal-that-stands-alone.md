# A proposal that stands alone

When the court puts a plan to the King, the card should take the whole
conversation column — nothing else in view to read while he is judging it.

## What is already here

This worktree carries the change uncommitted, in two files:

- `crates/kingdom-app/src/components/conversation.rs` — `ProposalCard` gains a
  `full` signal defaulting to **true**, rendered as `class:full`, plus a quiet
  `.proposal-expand` button in the card head toggling
  "Show conversation" / "Read in full".
- `style/components/_conversation.scss` — `.chat-proposal.full` takes `flex: 1`,
  wider padding, a 20px title and a body that fills what is left and scrolls
  inside it; `.chamber-column:has(.chat-proposal.full) .chamber-log { display:
  none; }` takes the log out of the flow rather than shrinking it. The log stays
  mounted, so its scroll position survives the toggle.

The composer deliberately stays visible: "say what you would change below" is
still the third option the card claims to be.

## What this task is

Verify and land it.

1. `cargo leptos build` (or `watch`) and confirm both targets compile.
2. Bring up a proving ground —
   `KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1 cargo leptos watch` — and open
   the starter plan that has a standing proposal (the sample court includes one
   on purpose).
3. Check by eye:
   - the card fills the column, log hidden, composer still present;
   - a long body scrolls *inside* the card and the accept/set-aside buttons stay
     reachable;
   - "Show conversation" restores the log at its previous scroll position, and
     the button flips to "Read in full";
   - with the spyglass open the card still behaves (it owns the column, not the
     frame);
   - `decided` dimming still reads correctly after pressing Start with this.
4. Fix whatever that turns up — expected candidates only if seen: body
   `max-width` for line length, action-button size at the larger scale.
5. `cargo test -p kingdom-core` and
   `cargo test -p kingdom-app --features ssr --no-default-features`.
6. Commit the two files together with a message in the house voice.

## Out of scope

Overlaying the whole chamber *frame* (header, spyglass) rather than the
conversation column. The column is where his attention is; taking the header
away would also take the plan's identity and the way back.
