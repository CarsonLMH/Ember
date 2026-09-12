# Surface registry

Keys below are the defaults in `src-tauri/src/keymap.rs`; anchors are CSS
selectors in the rendered DOM. Do not inspect the user keymap or database in
synthetic mode. If a key does nothing, verify the default in source and report
the runtime mismatch as a limit.

## Data-mode boundary

Real-data lookup is an opt-in fallback, never setup. SQL descriptions below are
selection criteria for a private audit that already has explicit, one-run user
consent; they are not commands to run during an ordinary audit. Never print a
folder path, filename, EXIF value, person label, face record, or database row.
Prefer an equivalent Storybook state, the generated synthetic fixture, or a
code-only finding. A missing state does not expand the audit's permission.

## Unsafe during any audit

These write to the journal, the DB, settings, or localStorage. Never press or
click them while auditing. If a state needs one of them, review it from code.

| Input | Effect |
|---|---|
| `0`–`5` | rating verdict (journaled) |
| `x`, `Backspace`, `Delete` | trash the pair (journaled, moves files) |
| `Cmd+z`, `Cmd+Shift+z` | replays the journal |
| `s`, `Shift+s` | sort cycle/reverse — persisted on `folders.sort/reverse` |
| `Shift+0`…`Shift+5`, `Shift+a`, person/recipe/tag filter picks | persisted on `folders.filter` (restore to "all" if you must touch one: `Shift+0`) |
| `g` then a tag key, tag palette clicks | tags a photo (journaled) |
| `v` | auto-advance toggle (persisted) |
| `r` | rescans the folder (safe for data, but triggers preview/face work and reshuffles the session) |
| Trash panel: restore / empty / delete | moves files |
| People panel: naming, ✕, "not a person", rename, merge, delete, enable/disable, delete-all, rescan, re-run, calibration | face data / settings |
| Face badge menu: any item; naming input `Enter` | face data |
| `Cmd+o` | native dialog — cannot be driven anyway |

Benign side effects of the audit itself (document them in Method): opening a
folder touches `folders.updated_at`/`cursor_photo`; flipping with arrows moves
the resume position; `Shift+f` (badges) and `t` (filmstrip) persist a UI
toggle in localStorage — restore them to how you found them.

## Surfaces

### Viewer HUD (header row)
- Open: always visible with a folder open. Anchor `.hud`.
- States: normal; `missing on disk` chip (needs a photo whose file is gone —
  from code); filter chip active (from code; or `Shift+0` is a no-op when
  already "all"); AF chip dim vs amber (amber needs `soft_threshold` set —
  from code).
- Safe: flips (`ArrowRight`/`Left`, `Home`, `End`).
- Code: `src/App.tsx` (`.hud*`), CSS `src/App.css` "hud".

### Filmstrip
- Open/close: `t`. Anchor `.filmstrip`, rows `.strip-row`, current `.strip-current`.
- States: at rest; with stars/tags/soft/missing marks (pick a folder where
  `photos.rating>0` and `tags<>''` — query below); scrolled to first/last (`Home`/`End`).
- Safe: flips; clicking a `.strip-thumb` (navigation only).
- Private-data selection criterion after consent: photos with a nonzero rating
  or tags in a dense window. Do not reproduce the matching stems or rows.
- Code: `src/components/Filmstrip.tsx`.

### Metadata panel
- Toggle: `i`. Anchor `.exif-panel`, groups `.exif-group`.
- States: full; no-recipe-match vs matched recipe (needs a folder with a
  saved recipe in `recipes.toml`); Autofocus section with/without score.
- Safe: toggle, scroll. **Unsafe:** "save as new recipe…" (writes `recipes.toml`).
- Code: `src/components/ExifPanel.tsx`, `RecipeSwitcher.tsx` for the recipe dropdown.

### Histogram
- Toggle: `h`. Anchor `.hist-panel`. Canvas content — screenshot only, measure the chrome.
- Code: `src/App.tsx`.

### Trash panel
- Open: click `.hud-trash-btn` (no key; disabled when nothing is trashed and
  panel closed). Anchor `.dock[aria-label="Trash"]`, rows `.trash-list li`, thumbs `.trash-thumb`, empty `.trash-empty`.
- States: with trashed photos (`photos.trashed=1` — query: `select count(*) from photos where folder_id=? and trashed=1`); empty.
- Safe: open, scroll, close. **Unsafe:** every button inside.
- Note: shares its shell with the People panel — root cause A of the faces audit may be an app problem, not a faces one; audit this surface to find out.
- Code: `src/App.tsx` (`dock === 'trash'`), CSS `.dock*` and `.trash-*`.

### People panel
- Toggle: `p`. Anchor `.dock[aria-label="People"]`. Header `.dock-head` (status `.dock-status`, close `.dock-close`), sections `.people-section`, rows `.person` (expand `.person-expand`), chips `.face-chip`, clusters `.people-cluster`, footer `.dock-foot`, toasts `.people-toast`, confirms `.people-confirm`.
- States: named list; a person expanded (clicking `.person-expand` is safe —
  it only loads faces); unnamed clusters at every scroll depth; loose faces
  (`show N faces seen only once` is safe; selecting chips is safe, naming is not);
  footer; disabled/first-run, scanning, toasts, confirms — from code.
- Private-data selection criterion after consent: a folder containing named and
  unnamed faces. Do not reproduce its path, names, counts, or rows.
- Code: `src/components/PeoplePanel.tsx`; deviations: `docs/FACES_DEVIATIONS.md`.
- Reference run: faces audit 2026-08-23.

### Face badges (on the photo)
- Toggle: `Shift+f` (persisted in localStorage — restore). Anchor `.face-layer`, badges `.face-badge`, rings `.face-ring` / `.face-ring-auto` / `.face-ring-unnamed`, pill `.face-unnamed`, menu `.face-menu`.
- States: confirmed + auto + unnamed on one photo (query: photos with
  `assigned_by='user'`, `'auto'` and `person_id is null` faces together);
  badge menu open (clicking a badge is safe; **its items are not**); naming
  input open ("This is someone else…" is safe; `Enter` is not); unnamed boxes
  revealed (clicking the pill is safe).
- Jump to a photo without flipping: People panel → expand person → click the
  chip whose `img.src` contains the photo id (navigation only).
- Code: `src/components/FaceBadges.tsx`.

### Switchers (recipe `c`, tag filter `Shift+g`, person filter `Shift+p`) and tag palette (`g`)
- Anchor `.cheat-sheet.recipe-switcher`, rows `.recipe-row`, selected `.recipe-sel`, empty `.recipe-empty`.
- States: list; selection moved with `ArrowDown` (safe); empty state (a folder with no recipes/tags/people).
- Safe: open, arrows, `Escape`. **Unsafe:** `Enter`, digits, row clicks (they pick = persist a filter / apply a tag).
- All four share one component shape — audit them together; a finding on one is a finding on the pattern.
- Code: `src/components/{RecipeSwitcher,TagSwitcher,PersonSwitcher,TagPalette}.tsx`.

### Cheat sheet
- Toggle: `?`. Anchor `.cheat-sheet` (without `.recipe-switcher`). Footer carries the build stamp.
- Safe entirely.
- Code: `src/components/CheatSheet.tsx`.

### Perf HUD
- Toggle: `` ` ``. Anchor `.perf-hud`. Dev chrome — audit only if asked.

### Notices and toasts
- Anchor `.notice` (app-level, from `session.notify`), `.people-toast` (panel).
- Reaching one safely: most notices follow a mutation. `r` (rescan) produces
  one without touching verdicts but reshuffles the session — acceptable if
  you restore position; otherwise from code.

### Empty / error overlay
- Anchor `.overlay-msg`, `.overlay-msg.error`. No folder open = the first
  screenshot before `EMBER_OPEN` takes effect, or launch without `EMBER_OPEN`.

### Canvas-only overlays (zoom `z`, focus zoom `f`, AF overlay `a`, blinkies `b`)
- Drawn on `.viewer-canvas`; nothing to measure in the DOM. Screenshot only,
  and remember the brief: nothing on the flip path changes for a design
  finding without a gate run.
