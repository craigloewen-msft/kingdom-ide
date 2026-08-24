# Rename conversation send button from "Decree" to "Send"

## Change

`crates/kingdom-app/src/components/conversation.rs:301` — the reply button in a
plan's chamber currently reads `Decree` when idle. Change that label to `Send`.

```rust
{move || if drafting.get() { "Drafting\u{2026}" } else { "Send" }}
```

Only the idle label changes; the in-flight `Drafting…` label stays.

## Out of scope

- `DecreeBar` (the kingdom-map decree bar) and the `decree-input` CSS class keep
  their names — this is a copy change to one button, not a rename of the
  metaphor.

## Verification

`cargo test -p kingdom-app --features ssr --no-default-features` (no test pins
this string; a compile check is sufficient).
