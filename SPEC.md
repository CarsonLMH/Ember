# SPEC.md — Ember

*Working name from the project folder; rename freely.*

## 1. Goal

A fast, reliable macOS photo-culling app for one user, built to replace a commercial culler that kept losing culling sessions. Priorities in strict order: **(1) speed of the culling loop, (2) durability of culling decisions, (3) everything else.**

## 2. Core model

- **Photo** = logical unit. A JPEG + RAF with the same stem in the same directory is one photo (case-insensitive `.jpg/.jpeg/.raf`). Preview always renders the JPEG; RAF-only files render the full-res JPEG preview embedded in the RAF. Singletons are first-class, marked with a subtle pair badge (J+R / J / R). Duplicate stems in different subfolders are distinct photos (keyed by full path).
- **Verdict model**: **stars 0–5** + **trashed**. No pick/reject flag. B-roll is implicit: unstarred and not trashed. A folder is "done" when everything is starred, consciously left unstarred, or in the Trash.
- **X = trash immediately**: the pair (both files or nothing) goes to the **macOS system Trash** via the native API, recoverable with Put Back. If trashing one file of a pair fails, the other is restored — never a half-deleted pair.
- **Safety nets**: multi-step **Cmd+Z undo** (restores trashed files from the Trash, reverts ratings) with redo; plus a **session trash list** panel — everything trashed, restorable per-item in one click. Undo history is journal-backed and survives restarts.
- Ratings stay on the current photo by default. **Auto-advance** is an explicit,
  persistent `V` toggle; trashing inherently advances.

## 3. The loop (primary flows)

1. **Open folder** (Cmd+O or drag) → recursive scan → first image visible **≤1s**, indexing continues in background. Sort: capture time (default) / filename / rating, reverse toggle. Existing XMP ratings in files (e.g. from Capture One) are adopted as initial state.
2. **First pass**: arrow through everything. `1–5` stars standouts in place (`V`
   opts into auto-advance), `X` trashes garbage, arrows skip B-roll. All
   navigation ≤50ms per flip.
3. **Tightening passes**: filter to **Unstarred** (survivors) or **Starred**
   (tighten the top), repeat. Filters: All / Unstarred / Starred / exact star
   level / N stars or more / recipe / unknown-recipe — combinable (AND).
4. **Resume**: reopening a half-culled folder restores sort, filter, and exact position. No seen-tracking.
5. **Session stats** in HUD: total / starred / unstarred / trashed.

## 4. Viewer

- **Zoom**: pinch, `Z` toggles fit↔100%, `F` jumps to 100% centered on the **Fujifilm AF point** from MakerNotes; `A` toggles AF-point overlay rectangle. **Zoom lock is automatic**: while zoomed, arrow keys keep the same crop position on the next/previous photo (burst focus comparison); returning to fit ends it.
- **Histogram**: `H` cycles off → luminance → RGB overlay; small corner panel; state persists.
- **Blinkies**: `B` toggles; highlights = any channel ≥250, shadows = all channels ≤5; thresholds tunable in settings.
- **EXIF panel**: `I` slides a docked right sidebar; image reflows (never overlapped). Grouped sections: Exposure, Lens, Film/Recipe, Camera, File.
- **Filmstrip**: vertical strip docked on one side, visible by default (portrait
  shots make it nearly free), `T` hides it for edge-to-edge. It opens already
  centered on the current photo; thumbnails show star/trash badges and current
  position. **Left/right arrows are always previous/next** regardless of strip
  orientation. No grid view.
- **HUD**: filename, position (n/m), stars, pair badge, session stats. Dev builds add a timing overlay (flip latency, cache hits).

## 5. Film recipes

- A recipe = named fingerprint over MakerNotes params: film simulation, dynamic range, WB mode + R/B shift, highlight tone, shadow tone, color, sharpness, noise reduction, clarity, grain (effect + size), color chrome effect, color chrome FX blue.
- **Each param accepts exact value, a set, a numeric range, or "any"** — e.g. Color: −4…+4; Grain: {off, weak-large, strong-large}. Photo matches if every param is within bounds. Hard-identity params (film sim, WB shift) are typically exact.
- No match → **"unknown recipe"**, with one-action **"save these settings as a new recipe"** pre-filled from that photo's EXIF.
- Recipes live in a human-readable TOML file in app config (portable, hand-editable). Filter by recipe and by unknown-recipe.

## 6. Keyboard (defaults — all remappable)

`←/→` prev/next · `1–5` stars · `0` clear · `X`/`Del` trash ·
`Shift+1–5` exact-star filter · `Ctrl+Shift+1–5` N-or-more filter · `Z` 100% ·
`F` AF-point 100% · `A` AF overlay · `H` histogram · `B` blinkies · `I` EXIF ·
`T` filmstrip · `V` auto-advance · `U` cycle filter · `S` sort · `R` refresh ·
`Cmd+Z/Shift+Z` undo/redo · `?` cheat sheet.
Remapping via a human-editable config file (change a line, relaunch); `?` overlay always reflects actual binds. In-app editor is post-v1.

## 7. Durability & persistence

- **Source of truth**: append-only action journal in SQLite (WAL mode) in Application Support, written **synchronously before the UI advances**. Crash at any moment loses nothing — state is replayable. No code path can bulk-rewrite verdicts.
- **XMP write-through** in a background queue seconds behind: rating embedded **into the JPEG** (atomic temp-file rewrite, image data untouched) and a standard **`.xmp` sidecar for the RAF** (RAF never modified). Pending-write count visible; queue flushes on quit and survives crashes.
- **Why embedded-JPEG matters**: the iCloud Photos → Capture One mobile path strips sidecars; the embedded JPEG rating is what survives. Capture One desktop reads both.
- Mood/role **tag data model** ships in v1 (standard XMP `dc:subject` keywords, same write path); tagging UI is post-v1.

## 8. Performance strategy

Budgets: **≤50ms** keypress→rendered flip (preloaded), **≤1s** cold open→first image, ~1,500 photos max without degradation, on an M1 Max/32GB.

- Rust owns decode + preview generation: screen-fit preview JPEGs generated ahead of the cursor (one 1/2-scale decode also yields histogram + blinkies data); frontend owns decoded pixels in an explicit ImageBitmap cache (never trusting WebKit's evictable caches) and renders via canvas blit. Full-res decode happens only in zoom mode (current ±1).
- Images reach the webview via an async custom protocol (no base64/IPC copies). Preview disk cache makes reopening a folder near-instant.
- Metadata two-tier: fast Rust EXIF pass (capture date, orientation) at scan for immediate sort; `exiftool -stay_open` batch worker for MakerNotes (AF point, recipe params), prioritized around cursor, cached in SQLite by (path, mtime, size).
- Filmstrip thumbnails derived from previews, disk-cached.
- **Slice 1 of the build proves the budgets before any culling features exist**; timings instrumented (p99 flip latency, cache-miss serves, cold-open time), regressions visible in a dev HUD.

## 9. Edge cases

Manual refresh (`R`) reconciles external changes; missing files degrade gracefully ("missing" badge, never a crash). External volumes fine (per-volume Trash). iCloud-dataless files materialize on read. Symlinked dirs resolved and deduped.

## 10. Stack & install

- **Tauri 2** (Rust) + TypeScript/React/Vite frontend: vs Electron it's dramatically lighter with real native API access from Rust; vs native Swift (the other credible option) Tauri keeps the perf-critical core in Rust; the honest risk — WKWebView compositing/caching behavior — is mitigated by the owned-pixels canvas pipeline and proven in slice 1. rusqlite, native trash API, exiftool.
- **Install**: each stable milestone ships as a normal `.app` in /Applications (Spotlight/Dock launch); dev builds for testing. One-time setup: `brew install exiftool`, Rust toolchain.

## 11. Non-goals (v1)

Videos, iPad, editing/RAW development, HDR/EDR, content detection, online recipe lookup, sequencing/curation (a future app consumes this one's XMP), GPS, batch rename, SD ingest, grid view, accounts/cloud. Color labels: declined. Seen-tracking: declined. (Face detection was a v1 non-goal; it is now chartered post-v1 as §14.)

## 12. Post-v1 roadmap (ordered)

1. Burst/scene grouping in the filmstrip 2. Tag palette UI 3. Compare mode 4. In-app keybind editor 5. Live file watching.

## 13. Resolved judgment calls

1. Multiple recipe matches → first match in recipe-file order wins the label; EXIF panel lists all matches.
2. Existing ratings in files are adopted on first open (journal wins thereafter).
3. Zoom lock is automatic while zoomed — no separate key.
4. Pairing is same-directory only.
5. `0` clears stars; B-roll needs no key.
6. Filters combine with AND.

## 14. Faces (post-v1)

iOS-Photos-style people support scoped to Ember's shape: detect faces in the
open folder, name recurring faces once, auto-recognize them from then on,
filter the folder to a person.

- **Stack**: on-device only — YuNet detector + SFace recognizer (opencv_zoo,
  MIT/Apache-2.0) through ONNX Runtime (`ort`, CPU, statically linked).
  Models bundled and pinned by SHA-256. Windows-portable by construction;
  macOS builds are Apple Silicon only (no Intel ONNX Runtime prebuilts).
- **Data**: SQLite only (DB tables `persons` / `faces` / `face_scan` — see
  docs/METADATA.md). Face labels are NOT verdicts: never journaled, never in
  Cmd+Z, never written to files or sidecars. Recoverability comes from
  exact-prior-state undo on naming and durable per-face rejections ("not X"
  can never be re-assigned by any automatic path).
- **Indexing**: automatic background worker, one thread, lowest priority —
  strictly after previews exist, cursor-prioritized, cached per file. Proven
  by gate to leave the flip loop untouched (storm runs with inference pinned
  active). All worker writes are conditional commits guarded by DB-backed
  epoch/enabled/generation/revision checks — a user correction always beats
  an in-flight scan, in any process.
- **UX**: People panel (`p`, trash-panel style) — named people with counts,
  rename, merge-on-rename-collision; unnamed recurring
  clusters with one-line naming, per-face exclude, and "not a person / don't
  label" for statues and photographed photos; faces seen only once are
  collapsed and rescue-only (unlabeled is their resting state); undo toasts;
  "rescan faces" re-detects a folder after a settings change, names intact.
  On the photo: face badges (named only, `Shift+f` toggles) with a correction
  menu, plus an unnamed-face count that reveals boxes for naming in place.
  Person filter (`Shift+p`) AND-combines with stars/recipe/tag.
- **Auto-recognition** is calibrated on real photos before it ships
  (thresholds in settings.toml `[faces]`, measured 2026-08: assign at 0.45
  with a 0.08 margin, detect at 0.8); prototypes come from user-confirmed
  faces only — outliers excluded — with a margin test, so one mistake cannot
  cascade. Corrections are absolute: "not X" bars X from every automatic
  path, and naming X retracts it.
- **Privacy**: "Delete all face data" wipes embeddings, names and chips AND
  durably disables indexing (survives relaunch and concurrent processes);
  re-enabling is an explicit act that starts from scratch. It fails closed:
  if the settings file can't be updated the database keeps indexing off, and
  if a face image file can't be removed the command says so instead of
  reporting success — the removal is retried at the next launch. Everything is
  local; nothing ever leaves the machine.
