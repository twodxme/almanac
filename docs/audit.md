# Interface audit and gap analysis

A measured audit of Almanac's GTK4 interface: every count below came from the
source, not from recollection. Two bugs were found by measuring rather than by
eye — the card grid's one-column layout, and page chrome managed from
seventeen scattered call sites.

The full rendered version with severities and before/after figures is
[audit.html](audit.html).

## Conventions this interface follows

- **One primary action**, in the header, whose label and handler come from the
  current page. Never a second one in a page toolbar.
- **Toasts for events** ("Removed X", "Could not reach the remote"); the
  **header subtitle for standing state** ("130 installed · 31 with updates").
  Summaries are scoped to the page that owns them, because loaders are
  asynchronous and one page's result must not overwrite another's.
- **Secondary pages** (Featured, AI, Health check) share one layout: full
  width, boxed lists, section heading plus description, 12px margins. The App
  Store's card grid and the package table keep their own, because a grid and a
  sortable table genuinely are different things.
- **Untrusted text never goes into a row with markup enabled.** Adwaita rows
  parse Pango markup; an unescaped `&` in a skill description blanks the row.
  Build the row, disable markup, *then* set the text.
- **Nothing destructive runs before a dialog has shown the full list** of what
  it would touch.
- **Every row has an affordance or an explanation.** A row that cannot act
  says why.

## Still open

| Gap | Note |
|---|---|
| Spacing has no scale | Margins in use: 4, 5, 6, 8, 10, 12, 14, 16. Gaps: 3, 4, 5, 6, 7, 8, 10, 20. Each is locally sensible; none derive from a scale. |
| Nine badge classes overlap | Three font sizes across `crit-pill`, `stale-pill`, `store-chip`, `count-badge`, `store-installed` and three `trust-*`. Should collapse to three semantic levels plus one accent. |
| `omarchy update` opens a terminal | Everything else runs in-app. The system updater manages its own `sudo`, which wants a TTY. |
