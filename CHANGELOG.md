# Changelog

User-visible changes, newest first. The running app identifies itself in the
`?` cheat-sheet footer (`Ember <version> · <commit> · built <date>`); a `+`
after the commit means it was built with uncommitted changes. Code listed here
is only in your running app if it was built at or after that change landed —
when in doubt, ask a session to rebuild.

## 1.4.0 — 2026-08-23

- Fix: People panel chips no longer stay broken for a person whose best face
  lives in another folder or a trashed photo — the representative now comes
  from the folder you have open, and a crop that can't be served shows a blank
  chip instead of a broken image.
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
