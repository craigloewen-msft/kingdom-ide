# Raise the court's acting cap from 24 to 500 rounds

## Why

`MOST_ROUNDS` (`crates/kingdom-app/src/api.rs:468`) stops the model after 24
acts in one turn. Real work — reading a codebase, running builds, iterating on a
failing test — routinely needs far more. The cap should still exist as a
runaway-loop guard, but at 500 rather than 24.

## Changes

- `crates/kingdom-app/src/api.rs:468` — `const MOST_ROUNDS: usize = 24;` → `500`.
- Its doc comment stays (the runaway-loop rationale is unchanged); reword only
  if it names the number.
- `crates/kingdom-app/src/llm/system_prompt.rs:24` — the `MOST_GUIDANCE` doc
  comment says the prompt is resent on "a loop that may run 24 times". Update to
  500. Note this makes the per-turn guidance bill ~20x worse in the worst case;
  the 64 KiB cap is left as is, but the comment should reflect the new figure.
- Check the note text near `api.rs:752` ("The court acted {cap} times ...") — it
  interpolates the cap, so no change needed; confirm only.

## Verification

- `cargo test -p kingdom-app --features ssr --no-default-features`
- `cargo test -p kingdom-core`
