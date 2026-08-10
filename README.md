# Ember

A fast, durable macOS photo-culling app for Fujifilm JPEG+RAF workflows.
Born from one lost culling session too many: instant arrow-key flipping
(p99 ≤ 50ms), verdicts that survive `kill -9`, a readable metadata panel,
fuzzy film-recipe matching, and on-device face recognition (People panel on
`p` — local-only, deletable, never written to your files). Single-user,
local-only, keyboard-first.

**SPEC.md** is the product spec; **CLAUDE.md** has the working conventions.

## Setup (one-time)

```sh
brew install exiftool
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust stable
npm install
```

## Run / build

```sh
npm run tauri dev                 # development (perf HUD on backtick)
npm run tauri build               # release .app → src-tauri/target/release/bundle
cd src-tauri && cargo test        # pairing, journal, recipes, XMP safety
./scripts/checks.sh <photo-dir>   # tests + hidden-window regression gates
./scripts/ship.sh <dir> <msgfile> # gates → commit → debug build → swap
```

## Where things live

- Verdict journal (SQLite): `~/Library/Application Support/com.cleung.ember/ember.sqlite3`
- `keymap.toml`, `settings.toml`, `recipes.toml`: same folder (created with commented defaults; edit + relaunch, recipes hot-reload)
- Preview/thumbnail cache: `~/Library/Caches/com.cleung.ember/` (safe to delete; auto-pruned to `[cache] max_mb` from settings.toml, default 2GB, oldest folders first)

## Guarantees

- RAF files are never modified; RAF metadata goes to `.xmp` sidecars.
- JPEG XMP writes are atomic and leave image data byte-identical
  (`exiftool -ImageDataHash`-verified in tests).
- Ratings write through to embedded XMP, `.xmp` sidecars, and the
  `kMDItemStarRating` xattr (Capture One, Spotlight/Finder, and other Mac
  photo tools that read it).
- Every verdict lands in an append-only journal before the UI acknowledges;
  trash operations are pair-atomic against the macOS system Trash.

Press `?` in the app for the full shortcut list.

## License & support

[MIT](LICENSE). Free forever — no subscription, no account, no telemetry.
If Ember saved your evening, you can
[buy me a roll of film](https://github.com/sponsors/CarsonLMH).
