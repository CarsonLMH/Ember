# Implementation Plan: Culling controls and instant filmstrip

## Overview

Add a picture-only immersion mode, minimum-star filters, a clearer shortcut
reference, rating-in-place by default, and a filmstrip that is ready at the
saved cursor on first paint. Preserve Ember's existing sort engine and make its
photographic meaning clearer instead of adding a misleading filesystem-date
sort. Every slice stays outside the measured flip path and lands as a tested,
revertible commit.

## What the audit found

- Sorting already supports capture date, filename, rating, and reverse order,
  persisted per folder. Capture date is EXIF `DateTimeOriginal`, with file
  modification time only as a fallback.
- `Shift+1` through `Shift+5` already select exact star ratings.
- Auto-advance is already a persistent `V` toggle, but a missing preference
  currently means on.
- Warm filmstrip lag is primarily a mount-order bug: a restored folder can open
  at photo 157 while the virtual strip first renders rows near photo 1, then
  measures and scrolls after paint. The latest folder inspected had all 197
  thumbnails already cached.
- The `?` sheet is a flat list with no semantic sections.

## Architecture decisions

- **Rating defaults to staying put.** Keep `V` as an explicit opt-in and keep
  its `localStorage` key; only the stored value `"1"` enables it. Do not create
  a second settings or database authority for a non-verdict UI preference.
- **Filters remain composable text modes.** Add `star1plus` through
  `star5plus`; the folder table already persists filter strings, so no schema
  migration is needed. `Ctrl+Shift+N` selects N-or-more; `Shift+N` remains
  exact N.
- **Use photographic dates.** Retain Date captured, Filename, Rating, and
  Reverse. Do not add filesystem birth time: imports, copies, and cloud tools
  can rewrite it, so it often describes transfer time rather than the photo.
- **Immersion is session-only and presentation-only.** `Shift+T` toggles it,
  `Escape` exits, and bare `Tab` remains native focus traversal. It suppresses
  filmstrip, HUDs, docks, face badges, histogram,
  performance HUD, and canvas analysis decorations without changing their
  underlying preferences. Loading, durability failures, and error notices stay
  visible; safety beats purity.
- **Fix filmstrip geometry before changing backend scheduling.** Render the
  initial virtual window around the restored cursor, establish scroll position
  in a layout effect, and remove redundant browser lazy-loading. Measure cold
  cache separately before changing retry cadence. Never block folder open or
  derive thumbnails from originals on the critical path.
- **Group help by user intent in React.** Keep the backend's live remapped
  bindings as the source of truth, then group them into Navigate; Rate &
  recover; Filter & sort; Inspect photo; Panels & display; Folder & help. A
  fallback Other group ensures a binding can never disappear.

## Dependency graph

```text
Filter modes + key actions ─┐
Rating default semantics ───┼─> grouped final shortcut inventory
Immersion key + visibility ─┘

Filmstrip geometry is independent, but App.tsx/Filmstrip.tsx work lands
sequentially to keep each commit buildable.
```

## Task list

Tasks and acceptance checks are tracked in `tasks/todo.md`.

### Phase 1: Culling semantics

1. Default ratings to stay on the current photo; keep `V` as opt-in.
2. Add exact-vs-minimum star filter modes and shortcuts.

### Checkpoint: Culling semantics

- Unit tests prove journal acknowledgement still precedes the UI change.
- Exact and minimum filters are distinct and persistable.
- The app builds with no flip-path additions.

### Phase 2: Startup and immersion

3. Make the filmstrip mount at the restored cursor without a post-paint jump.
4. Add session-only picture-only immersion mode.

### Checkpoint: Runtime behavior

- Warm-cache current thumbnail is in view and loaded immediately.
- Immersion preserves navigation/rating and restores the prior layout exactly.
- Fit and locked zoom survive the viewer resize.

### Phase 3: Findability and acceptance

5. Group the `?` sheet and clarify sort wording.
6. Run full unit, Rust, Storybook, accessibility, E2E, durability, and
   real-photo performance checks; then build and install the accepted local app.

## Risks and mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Rating under an active filter appears to move despite auto-advance being off | Confusing | Document and test that an out-of-filter photo is removed; the next match occupying its slot is filter correctness, not auto-advance. |
| A modifier chord falls through to a bare rating | Wrong verdict | Unit-test `Ctrl+Shift+DigitN` and the existing modified-key guard. |
| Immersion hides a failed journal/XMP message | Silent durability failure | Keep notices, errors, and loading states visible in immersion. |
| Immersion resize changes fit/zoom framing | Culling disruption | Use existing resize/redraw plumbing and add runtime fit/locked-zoom checks. |
| Filmstrip work steals resources from first image or flips | Budget regression | Frontend-only geometry fix first; no synchronous thumbnail generation; run normal and faces-active storms. |
| Existing custom `keymap.toml` lacks new action lines | Poor discoverability | Missing actions already inherit new defaults; the live grouped cheat sheet shows them. Do not rewrite the user's file. |

## Deliberately out of scope

- Filesystem creation/birth-time sorting.
- Face-recognition threshold calibration and the current `peopletest` fixture
  qualification issue.
- Unrelated People-panel review findings.
