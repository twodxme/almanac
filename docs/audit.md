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
  **header subtitle for standing state** (how much is installed, how much is stale).
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

## Closed since

| Gap | Resolution |
|---|---|
| Spacing had no scale | Twenty distinct values snapped onto `2, 4, 6, 8, 12, 16, 20, 24` — a 4px scale with 2 and 6 as half-steps for tight chrome. A test fails the build on any value off it. |
| Nine badge classes overlapped | One `.pill` base with five modifiers (`crit`, `warn`, `ok`, `accent`, `quiet`) at a single size. Each modifier carries the base declarations, so one class name is enough at every call site. Three tests assert it: no retired class names, every applied variant is styled, and no variant overrides the size. |

## Still open

| Gap | Note |
|---|---|
| `omarchy update` opens a terminal | Everything else runs in-app. The system updater manages its own `sudo`, which wants a TTY. |
