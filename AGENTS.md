# AGENTS.md

macOS photo-culling app (Tauri 2 + Rust + React/TS). **Read SPEC.md first** — it is the confirmed product spec. The user is the product owner and tests every slice on real Fujifilm X-T50 photos; they are not an engineer.

## Priorities (strict order)

1. Speed of the culling loop — hard budgets: p99 keypress→rendered flip ≤50ms (preloaded, zero cache-miss serves in steady state), cold folder-open→first image ≤1s.
2. Durability — a crash may never lose or silently alter a verdict. Journal write precedes UI acknowledgment, always.
3. Everything else.

## Stack

- Tauri 2, Rust stable. Frontend: React 19 + TypeScript (strict) + Vite.
- rusqlite (bundled, WAL). `image` crate (JPEG decode) + fast_image_resize (NEON resampling) for previews/thumbs. kamadak-exif (fast pass). exiftool via persistent `-stay_open` worker (MakerNotes reads, ALL XMP writes). objc2 for NSFileManager trash.
- Custom `photo://` protocol (async handler, ACAO headers, opaque IDs). Frontend image cache = explicit ImageBitmap LRU (`.close()` on evict); canvas renderer.
- Faces (SPEC §14): what shipped differs from the implementation plan in ways driven by real-photo acceptance — see **docs/FACES_DEVIATIONS.md** before "fixing" something that looks off-plan (notably: hide-a-person was deliberately removed). Stack: `ort` =2.0.0-rc.13 (CPU, static ONNX Runtime; build downloads the lib — `ORT_LIB_PATH` env overrides for offline builds; **Apple Silicon only**, no Intel prebuilts). Pinned models in `src-tauri/models/` (bundled as resources): YuNet `face_detection_yunet_2023mar.onnx` sha256 8f2383e4…52fa4 (fixed 640×640 input → letterbox), SFace `face_recognition_sface_2021dec.onnx` sha256 0ba9fbfa…4e79. Face chips `{id}-f{n}r{rev}.jpg` live in the preview cache — `rev` is `face_scan.chip_revision`, the identity of the exact detection that produced the crop, and chip URLs carry it (`photo://face/{id}/{n}/{rev}`), so a file surviving from an older scan can never be served as the current chip. A route miss 404s and enqueues a dedicated single-thread repair (one preview decode rebakes all of a photo's chips; repair also sweeps wrong-revision leftovers — cache dir stays safe to delete). All worker DB writes are conditional commits (epoch/enabled/gen/source/revision guards in `facestore.rs`); user face edits always win. `EMBER_FACES_FORCE=1` pins continuous reprocessing for gate runs.
  - Three privacy invariants are load-bearing, all cross-process (two Ember instances share the DB):
    1. **A face crop only reaches the disk inside `facestore::publish_chips`.** Bake and repair encode chips to `Vec<u8>` and hold them in memory; `publish_chips` takes SQLite's writer lock (`BEGIN IMMEDIATE`), re-checks the rows/epoch/scan revision, and only then writes. So a Delete-all in either process either precedes the publish (its sweep takes the files) or the publish refuses and nothing is ever staged. Never write a chip any other way, and never move the encode-then-write split back onto the filesystem.
    2. **Delete-all fails closed, and the settings mirror is a two-phase protocol.** Phase 1 commits `face_state['enabled_mirror_ok'] = 0` on its own; phase 2 applies the DB change **and writes settings.toml inside one `BEGIN IMMEDIATE`**, so enabled-state changes are totally ordered across processes and the file can never disagree with a trusted DB. An enable decided before a delete that has since committed sees `face_state['wipe_seq']` moved and yields. Until the file is known to match, the DB decides at launch.
    3. **Cleanup failure is never silent.** `delete_all_face_chips` is fallible; the wipe sets `face_state['chip_sweep_pending']`, cleared only by a sweep that removed everything, and retried at the next launch. `delete_face_data` reports the failure rather than claiming a successful deletion.
  - `face_scan.face_revision` identifies *which set of face rows a photo has*, not just "a human edited it": user edits bump it and so does every committed scan. That is what makes one detection per revision the rule, what lets a pending naming-undo tell "still my rows" from "re-detected since" (SQLite reuses rowids), and what pins a chip publish to the scan that produced its crops.
  - Model ownership is decided before any job is claimed (`facestore::claim_state`) by comparing `faces::MODEL_RELEASE` — a monotonic rank of the bundled model set, **bump it whenever a model or the preprocessing changes** — against the owning generation's stored rank. Never by hash history: a DB first indexed by a newer build has never seen an older build's hashes, and that older build must still park. Only a strictly newer rank may register (re-checked under the writer lock); identical hashes stay `Ready` whatever the rank — a ranked build meeting the pre-rank row of its own models *adopts* it in place (promotes `release`, no epoch bump, no reindex), which is what lets the DB enforce the equal-rank rule afterwards. **Unsupported: running a pre-rank (schema ≤v5) Ember binary concurrently with a v6+ one against the same DB.** The old code predates the rank gate entirely and can register itself by row order; nothing in new code can stop a writer that never calls it. The supported two-process reality is and has always been two instances of the *same installed build* (user window + gate harness).

## Commands

- `npm run tauri dev` — run the app (dev, with perf HUD on backtick).
- `npm run tauri:mcp` — same, plus the MCP bridge (cargo feature `mcp`, loopback
  `127.0.0.1:9223`) so Claude can screenshot/inspect/drive the webview via the
  `tauri` MCP server in `.mcp.json`. Design-review sessions only: never a gate
  run, never a ship build (the shipped .app is `--debug`, so the feature flag,
  not `debug_assertions`, is what keeps it out).
- `npm run tauri build` — release `.app` (install to /Applications at stable milestones).
- `npm run storybook` — isolated React chrome at `127.0.0.1:6006`; its MCP
  endpoint is `/mcp` for the project-scoped Codex and Claude connections.
- `npm run storybook:build` — type/bundle every story and generate manifests.
- `npm run e2e:build && npm run e2e` — synthetic Tauri harness, including axe
  accessibility and reviewed chrome snapshots; never touches the real Ember DB.
- `npm run e2e:visual:update` — explicitly replace visual baselines only after
  inspecting `e2e/.visual-output/actual/` and deciding the UI change is correct.
- `cd src-tauri && cargo test` — Rust unit tests (pairing, journal replay, recipe matcher).
- `cd src-tauri && cargo clippy && cargo fmt` — must be clean before commit.

## Prerequisites (one-time)

`brew install exiftool`; Rust via rustup (stable); Node ≥22.

## Iron rules

- **Never modify a RAF file.** RAF metadata goes to a `.xmp` sidecar.
- JPEG XMP writes must be atomic and leave image data byte-identical (verified with exiftool `-ImageDataHash` in tests).
- Trash operations are pair-atomic: both files or neither; failures roll back.
- No full-res decode in the flip hot path — full-res exists only in zoom mode.
- No bulk verdict mutations anywhere. All verdict changes flow through the append-only journal.
- Keep the flip path framework-free (imperative canvas + cache modules); React renders chrome, not the image.
- Face badges are DOM chrome over the canvas, positioned via `viewer.normalizedRectToCss` and **fit-mode only** — chasing the canvas transform with DOM nodes through a 120Hz pinch stream is exactly the work this app keeps off the interaction path. The layer is `pointer-events: none` so pan/pinch/double-click still reach the canvas.

## Data locations

- DB: `~/Library/Application Support/com.cleung.ember/ember.sqlite3`
- Config + keymap + `recipes.toml`: `~/Library/Application Support/com.cleung.ember/`
- Preview/thumb/chip cache: `~/Library/Caches/com.cleung.ember/` (safe to delete; ~1MB/photo). A janitor prunes it to `[cache] max_mb` (settings.toml, default 2048) once per launch, oldest-opened folders first, never the folder open in this session.

## Conventions

- Small conventional commits (`feat:`, `fix:`, `perf:`, `test:`, `docs:`); commit at every working milestone.
- **Every user-visible change adds a line to CHANGELOG.md, in the same commit**,
  under the top version heading. The `?` cheat-sheet footer shows the build
  stamp (version · commit · build date, from build.rs) — that pair is how the
  user tells what shipped. Bump the version (package.json, tauri.conf.json,
  Cargo.toml + lockfiles) and start a new heading only at an accepted
  milestone; dev-only changes get a "Dev-only:" line or none.
- Rust: clippy-clean, `cargo fmt`. TS: strict mode, no `any` in the image/cache/journal paths.
- Code stays clean and conventional (possible future open-sourcing) but built for exactly one user — no speculative abstractions.

## Design language

Ember's chrome is a darkroom: dark-only, quiet, native-feeling. The photograph
is the only hero — chrome never competes with it for color or attention. This
section outranks any installed design skill (frontend-design, HIG,
apple-design, web-design-guidelines); where they conflict with it, this wins.

For React chrome work, inspect the existing Storybook stories through the
`ember-storybook` MCP server when `npm run storybook` is running. Add or update
a synthetic story for stable component states, but verify integrated behavior
in the Tauri app—the component catalog is not acceptance evidence for the
canvas path, native operations, or real-photo workflows.

- **Deliberate exceptions to generic design-skill advice** — do not "fix" these:
  - System font stack (`-apple-system` / SF Pro) is the *correct* choice for a
    native Mac tool, not a lazy default. Never swap in a webfont.
  - Flat dark grounds, no gradients/atmosphere/texture. Backgrounds recede so
    photos read true; near-neutral dark grays are calibrated viewing surround.
  - The chrome is static by design (zero animations today). Motion may be added
    only as considered micro-feedback in chrome (panels, badges, toasts) —
    CSS-only, interruptible, `prefers-reduced-motion`-respecting — and **never
    on the flip path**: nothing animates on or delays keypress→rendered flip.
- **Distinctiveness budget** lives in semantics, not layout novelty: selection
  blue `#6ea8ff`, rating gold `#f5c518`, warning amber `#e8a33d`, danger red
  `#e07070`. Color in chrome means something or it isn't there. Layout follows
  macOS conventions (sidebar/filmstrip, panels, keyboard-first everything).
- **Known debt**: App.css predates tokens — ~15 ad-hoc gray literals. When a
  slice already touches an area, consolidate its colors into CSS variables
  (`--bg`, `--panel`, `--ink`, `--muted`, `--line` + the four semantics above);
  no bulk restyle commits.
- Density and restraint over expressiveness: hairline `1px` borders, small
  radii (3–4px), 11–13px UI type, tabular numerals for counts/ratings.

## Definition of done (every slice)

1. App runs (`tauri dev`, or installed `.app` at milestones).
2. Flip-storm harness passes: p99 ≤50ms, zero miss-serves, cold open ≤1s — regressions block the slice.
3. Durability tests pass (kill -9 replay; XMP queue drains; file-safety hashes).
4. The user has driven the slice on their real photos and accepted it.
5. Committed.

## Architecture boundaries

- Rust owns scanning, native filesystem operations, SQLite state, preview and
  metadata work, XMP writes, and Tauri commands. `src-tauri/src/lib.rs` is the
  command/composition root; persistence belongs in `store.rs`, external
  metadata writes in `xmp.rs`, and preview/protocol work in their named modules.
- React renders application chrome. The flip hot path stays outside React in
  `src/lib/session.ts`, `src/lib/viewer.ts`, and `src/lib/imageCache.ts`.
- Images travel through the async `photo://` protocol using opaque IDs. Do not
  replace it with base64 or large IPC payloads.
- Background work must not delay journal acknowledgment or contend with the
  user-facing flip path without a measured reason.
- Explicitly close evicted `ImageBitmap` objects; do not rely on WebKit or GC
  to retain or release decoded frames predictably.

## Agent safety and verification

- Inspect `git status` before editing and preserve unrelated or user-authored
  changes, including work under `.claude/worktrees/`.
- Do not delete or reset user data while testing. Although the preview cache is
  regenerable, remove it only when the task explicitly requires cache
  invalidation.
- Do not run `scripts/ship.sh` unless the user explicitly asks for delivery. It
  stages and commits all changes, builds, terminates the installed debug app,
  and launches the replacement.
- `scripts/checks.sh` and `scripts/gate.sh` require a real photo fixture path
  and terminate matching dev processes. Never invent a path; confirm it and
  warn before disrupting a running app.
- Run verification proportionate to the change. Useful focused checks are
  `npm test`, `npm run build`, `cd src-tauri && cargo test`,
  `cd src-tauri && cargo fmt --check`, and
  `cd src-tauri && cargo clippy --all-targets -- -D warnings`.
- Persistence, filesystem, concurrency, preview/protocol, and hot-path changes
  require the relevant Rust tests and regression gates. Report every check not
  run and why.
- When requirements, design records, and current behavior disagree, surface
  the conflict instead of silently choosing one.
