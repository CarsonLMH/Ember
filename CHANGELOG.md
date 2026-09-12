# Changelog

User-visible changes, newest first. The running app identifies itself in the
`?` cheat-sheet footer (`Ember <version> · <commit> · built <date>`); a `+`
after the commit means it was built with uncommitted changes. Code listed here
is only in your running app if it was built at or after that change landed —
when in doubt, ask a session to rebuild.

## Unreleased

- Fix: when trashing a JPEG+RAF pair fails after the JPEG moved, Ember now
  reports a failed rollback honestly—with both errors and the two recovery
  paths—instead of claiming the JPEG was restored.

- The `?` shortcut sheet is now grouped by intent, with exact-rating and
  rating-or-more filters separated so the right chord is easy to find. It is
  also a real focused modal: culling keys cannot act behind it.
- Sort wording now says what Ember actually uses: Date captured, Filename, or
  Rating, with `S` to cycle and `Shift+S` to reverse.
- Dev-only: functional, accessibility, and real-photo gate runs now start
  truly hidden and use a prohibited macOS activation policy. Only explicit
  visual/performance E2E commands may show or focus the test app.
- `Shift+T` enters a picture-only immersion mode; `Shift+T` or `Esc` restores
  the exact previous panels and inspection overlays without stealing native
  `Tab` focus traversal. Safety/error notices stay visible.
- Resizing the viewer by opening panels, toggling the filmstrip, or entering
  immersion now preserves the actual zoom percentage instead of shifting a
  100% inspection view.
- The filmstrip now mounts directly at the current photo, resets cleanly when
  switching folders, and requests its already-virtualized thumbnails
  immediately, removing the warm-start lag.
- `Ctrl+Shift+1–5` filters to that star rating or higher; `Shift+1–5` remains
  the exact-rating filter.
- Ratings now stay on the current photo by default. `V` still enables and
  remembers auto-advance when wanted.

## 1.4.0 — 2026-08-23

- People and Trash are now real right-column sidebars — opaque, full height,
  their own scroll — instead of cards floating over the metadata panel. One
  takes the column at a time (`p` / the trash button; `i` swaps back to
  metadata; Esc closes). Person rows get a hover state and a visible Rename
  control (no more double-click), the scan status lives under the title, and
  nothing in the footer is clipped any more.
- Fix: People panel chips no longer stay broken for a person whose best face
  lives in another folder or a trashed photo — the representative stays in
  the folder you have open, and a temporarily missing crop shows a blank chip
  while Ember keeps probing for its repaired image.
- Fix: Escape closes People from its naming fields, clicking another person
  commits a pending rename, and clickable face chips work from the keyboard.
- Fix: a person-filtered view now picks up faces matched just as the filter is
  being enabled, instead of sometimes freezing on the first small cluster.
- The filmstrip can receive keyboard focus, so keyboard and assistive-technology
  users can reach and scroll it.
- **Focus check**: every photo gets a sharpness score measured at the camera's
  autofocus point, shown as a dim `AF n` chip in the HUD (higher = sharper;
  compare within a burst, not across scenes) and an Autofocus section in the
  metadata panel. Ember never judges on its own — set `soft_threshold` under
  `[focus]` in settings.toml to arm the amber chip, filmstrip dot, and Shift+A
  soft-focus filter.
- Build stamp in the `?` cheat-sheet footer, plus this changelog.
- Fix: photos already owned by another folder session no longer appear as
  verdict-less ghosts when a parent folder is opened — the app offers a jump
  to the owning session instead.
- Fix: star ratings adopted from files (rated on-camera or by other tools) now
  reach the XMP write queue instead of living only in the database.
- Dev-only: MCP bridge build (`npm run tauri:mcp`) and a `/design-audit`
  skill that drives it — screenshots and DOM measurements of the live app for
  design reviews. Never in the shipped build.
- Dev-only: Codex now shares the live Tauri and Storybook inspection setup,
  with a harness check that keeps Claude and Codex configuration aligned.
- Fix: `F` exits zoom from any zoomed state, not just AF-point zoom.
- Fix: a tags-only re-edit at the same star rating can no longer have its
  metadata write silently dropped by an older queued write completing late.
- People panel: delete a person entirely (their faces return to Unnamed);
  "re-run recognition" copy now says what the button actually does.
- Dev-only: MCP bridge for design-review sessions (`npm run tauri:mcp`);
  never part of shipped builds.

## 1.3.0 — 2026-08-11

- **Faces** (SPEC §14): on-device face detection and recognition — People
  panel on `p`, name/merge/correct people, auto-recognition with calibrated
  thresholds, filter by person on `Shift+P`, face badges on `Shift+F`.
  All processing local; "Delete all face data" wipes durably.
- Cache janitor: the preview cache is pruned to a budget (`[cache] max_mb`)
  once per launch, oldest-opened folders first, never the open folder.

## 1.2.0 — 2026-08-09

- **Tags**: tag palette on `g` (vocabulary in tags.toml), tag filter on
  `Shift+G`, HUD chips and filmstrip badges. Tags write to XMP `dc:subject`
  per docs/METADATA.md, journaled and undoable like ratings.

## 1.1.0 — 2026-08-09

- First release of Ember under its public history: the accepted culling loop
  (sub-50ms flips, journaled verdicts, crash durability), instant trash with
  session undo and pair-atomic file moves, star filters, recipes with a
  quick-switcher on `c`, RAF-only folders, cross-volume trash restore,
  histogram + blinkies, remappable keys with the `?` cheat sheet.
