# Shift+Enter makes a new line in the composer

Today the decree bar (`components/prompt_bar.rs`) and the chamber composer
(`components/conversation.rs`) are single-line `<input type="text">`. Enter
submits; there is no way to type a second line, so a multi-paragraph decree has
to be pasted or flattened.

## What changes

1. **`prompt_bar.rs`** — replace the `.decree-input` `<input>` with a
   `<textarea rows=1>`:
   - `on:keydown`: submit only when `ev.key() == "Enter" && !ev.shift_key()`,
     and call `ev.prevent_default()` so the newline is not also inserted.
     Shift+Enter falls through to the browser's own behaviour — a new line.
   - keep `prop:value` / `on:input` / `disabled` exactly as they are.
   - grow with the text rather than scrolling: an effect that sets the
     element's height from its `scroll_height` on input, capped (~6 rows) so a
     long decree cannot swallow the map.
2. **`conversation.rs`** — the same change for the chamber composer (line ~508).
   Same key rule, same auto-grow. The `.question-own-words` input (~line 1000)
   is a one-line answer by nature and stays an `<input>`.
3. **`style/components/_decree-bar.scss`** — `.decree-input` needs to work as a
   textarea: `resize: none`, `overflow-y: auto`, a line-height that matches the
   `field()` padding so one row is the same height the input is now, and
   `font: inherit` (a textarea does not inherit the page font by default).
   Check `_conversation.scss:557` still lines up.

## Why keep Enter as submit

Sending is the common action and the muscle memory is set by every chat
surface; Shift+Enter is the equally standard escape hatch. Inverting them
would cost a keystroke on every prompt to save one on the rare multi-line one.

## Verification

No new test: this is keyboard behaviour in a WASM view, and the suite launches
no browser. Verified by hand — type two lines with Shift+Enter, confirm the
box grows, confirm Enter still opens a plan and clears the box, confirm the
bar's height at rest is unchanged.

`cargo leptos build` must be clean.
