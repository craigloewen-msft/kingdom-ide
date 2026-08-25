# The Done button stands at full height

In the chamber's composer, **Done** renders roughly half the height of **Send**
beside it. It reads as a broken control rather than a quieter one.

## Cause

`style/components/_conversation.scss`, `.done-btn`:

```scss
padding: 0 14px;   // <- no vertical padding at all
font-size: 13px;
```

Its sibling in `style/components/_decree-bar.scss`, `.start-btn`, is
`@include m.royal-button(11px 22px)` — 11px top and bottom over the inherited
14px body font, so about 39px tall. `.done-btn` is its line box and nothing
else, about 17px.

The composer (`.chamber-composer`) is `align-items: flex-end`, so the short
button sits on the shared baseline and the height difference is fully visible
instead of being hidden by centring.

Note `.done-btn` also carries a 1px border that `.start-btn` (borderless
gradient) does not, so matched padding alone leaves it 2px taller.

## The fix

In `.done-btn`, replace `padding: 0 14px` with vertical padding that makes its
box match `.start-btn`, accounting for the border and the smaller font:

- `padding: 11px 14px` as the starting point, then
- trim so the two buttons measure equal — the 13px font gives a ~2px shorter
  line box and the border adds 2px, so `11px` is likely already right, but it
  should be measured rather than assumed.

Keep the quieter treatment (panel fill, dim ink, gold on hover). This is about
size only — Done should stay visually secondary to Send, just not stunted.

## Verifying

- `cargo leptos watch` against a proving ground
  (`KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1`), open a plan's chamber.
- Read back `offsetHeight` for `.start-btn` and `.done-btn` in the browser and
  confirm they are equal.
- Check the disabled state (`Closing…`) and the picker-open state, since the
  label text changes width and the chevron sits inside the button.
- Screenshot the composer to confirm it reads as two peers.

## Out of scope

The `.done-picker` panel below it, and any change to what Done does.
