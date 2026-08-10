# CLAUDE.md

macOS photo-culling app (Tauri 2 + Rust + React/TS). **Read SPEC.md first** — it is the confirmed product spec. The user is the product owner and tests every slice on real Fujifilm X-T50 photos; they are not an engineer.

## Priorities (strict order)

1. Speed of the culling loop — hard budgets: p99 keypress→rendered flip ≤50ms (preloaded, zero cache-miss serves in steady state), cold folder-open→first image ≤1s.
2. Durability — a crash may never lose or silently alter a verdict. Journal write precedes UI acknowledgment, always.
3. Everything else.

## Stack

- Tauri 2, Rust stable. Frontend: React 19 + TypeScript (strict) + Vite.
- rusqlite (bundled, WAL). `image` crate (JPEG decode) + fast_image_resize (NEON resampling) for previews/thumbs. kamadak-exif (fast pass). exiftool via persistent `-stay_open` worker (MakerNotes reads, ALL XMP writes). objc2 for NSFileManager trash.
- Custom `photo://` protocol (async handler, ACAO headers, opaque IDs). Frontend image cache = explicit ImageBitmap LRU (`.close()` on evict); canvas renderer.
- Faces (SPEC §14): `ort` =2.0.0-rc.13 (CPU, static ONNX Runtime; build downloads the lib — `ORT_LIB_PATH` env overrides for offline builds; **Apple Silicon only**, no Intel prebuilts). Pinned models in `src-tauri/models/` (bundled as resources): YuNet `face_detection_yunet_2023mar.onnx` sha256 8f2383e4…52fa4 (fixed 640×640 input → letterbox), SFace `face_recognition_sface_2021dec.onnx` sha256 0ba9fbfa…4e79. Face chips `{id}-f{n}.jpg` live in the preview cache; a miss on the `photo://face/` route 404s and enqueues a dedicated single-thread repair (one preview decode rebakes all of a photo's chips — cache dir stays safe to delete). All worker DB writes are conditional commits (epoch/enabled/gen/revision guards in `facestore.rs`); user face edits always win. `EMBER_FACES_FORCE=1` pins continuous reprocessing for gate runs.

## Commands

- `npm run tauri dev` — run the app (dev, with perf HUD on backtick).
- `npm run tauri build` — release `.app` (install to /Applications at stable milestones).
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

## Data locations

- DB: `~/Library/Application Support/com.cleung.ember/ember.sqlite3`
- Config + keymap + `recipes.toml`: `~/Library/Application Support/com.cleung.ember/`
- Preview/thumb/chip cache: `~/Library/Caches/com.cleung.ember/` (safe to delete; ~1MB/photo). A janitor prunes it to `[cache] max_mb` (settings.toml, default 2048) once per launch, oldest-opened folders first, never the folder open in this session.

## Conventions

- Small conventional commits (`feat:`, `fix:`, `perf:`, `test:`, `docs:`); commit at every working milestone.
- Rust: clippy-clean, `cargo fmt`. TS: strict mode, no `any` in the image/cache/journal paths.
- Code stays clean and conventional (possible future open-sourcing) but built for exactly one user — no speculative abstractions.

## Definition of done (every slice)

1. App runs (`tauri dev`, or installed `.app` at milestones).
2. Flip-storm harness passes: p99 ≤50ms, zero miss-serves, cold open ≤1s — regressions block the slice.
3. Durability tests pass (kill -9 replay; XMP queue drains; file-safety hashes).
4. The user has driven the slice on their real photos and accepted it.
5. Committed.
