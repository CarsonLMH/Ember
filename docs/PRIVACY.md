# Privacy and local data

**Status: current privacy contract and threat boundary.** This page is grounded
in the current source, tests, `SPEC.md`, and the load-bearing face invariants in
`AGENTS.md`. Earlier findings under `docs/reviews/` are historical snapshots;
they explain why safeguards exist but do not override the current code.

Ember is a single-user, local-first macOS application. It has no account,
cloud service, collaboration backend, advertising SDK, or built-in telemetry.
Normal photo analysis, metadata extraction, focus scoring, and face recognition
run on the Mac.

## Data inventory

| Data | Where it lives | Why it exists | Sensitivity / deletion |
|---|---|---|---|
| Folder paths, photo paths and opaque IDs | `ember.sqlite3` | Resume, pairing, cache lookup, and ownership | Reveals library organization; retained until the database is removed |
| Ratings, tags, trash state and action history | SQLite journal/materialized rows; rating and tags also write to photo metadata | Durable culling and undo/redo | Durable user decisions; not removed by face deletion |
| Pending metadata jobs and errors | `ember.sqlite3` | Crash-safe XMP write-behind and visible retry | May contain source paths and the intended rating/tags |
| Cached MakerNotes and derived focus scores | `ember.sqlite3` | Metadata panel, recipes, AF inspection and focus comparison | May include camera/file metadata; not removed by face deletion |
| Person names, face rectangles, 128-float embeddings, assignments, rejections and per-photo scan rows | Face tables inside `ember.sqlite3` | Local people recognition and correction | Biometric/personal data; removed by **Delete all face data** |
| Face model-generation hashes/ranks and deletion control counters | `ember.sqlite3` face control tables | Keep embedding spaces compatible and make deletion durable across processes | Not identity labels; retained across face deletion by design |
| Face chips | `~/Library/Caches/com.cleung.ember/previews/` | People-panel thumbnails and on-demand repair | Biometric image crops; swept by face deletion and retried after a failed sweep |
| Previews, thumbnails, histograms, clipping masks and RAF display extracts | Same preview cache | Fast display and inspection | Derived photo content; safe for Ember to regenerate, but still private imagery |
| `settings.toml`, `keymap.toml`, `recipes.toml`, `tags.toml` | Application Support directory | User configuration | May include custom names or workflow vocabulary |
| UI preferences | WKWebView `localStorage` | Panel visibility, face badges, auto-advance, histogram and blinkies | Local preference state; not photo verdicts |
| Performance reports | `Application Support/.../perf-reports/` | Local performance evidence | Aggregate timings; retain or remove separately |
| Source photos and sidecars | User-selected folders | The library being culled | User-owned originals; see file mutations below |
| Trashed source files | macOS system Trash | Recoverable pair trash | Remain until the user empties Trash or puts them back |

The primary paths are:

- `~/Library/Application Support/com.cleung.ember/`
- `~/Library/Caches/com.cleung.ember/previews/`

SQLite may have `ember.sqlite3-wal` and `ember.sqlite3-shm` beside the main
database after an active or unclean session. They are part of the database
state, not disposable log files.

## What Ember reads and changes in a photo library

Ember recursively reads JPEG and RAF files in a folder the user chooses. It
extracts basic EXIF directly and uses a local ExifTool child process for full
metadata and Fujifilm MakerNotes.

It deliberately limits source-file changes:

- **RAF bytes are never modified.** Ratings and tags for a RAF go to a sibling
  `.xmp` sidecar.
- A JPEG receives `xmp:Rating` and `dc:subject` through an ExifTool
  `-overwrite_original` rewrite. Tests compare `ImageDataHash` before and after
  to ensure the encoded image payload is byte-identical.
- JPEG and RAF files receive the macOS
  `com.apple.metadata:kMDItemStarRating` extended attribute.
- Trashing moves the JPEG/RAF pair through the native macOS Trash API. If the
  second half fails, Ember attempts to restore the first; if macOS refuses that
  rollback, Ember reports both failures and manual recovery paths.
- Ember does not write face names, embeddings, rectangles, or identity labels
  to a photo or sidecar.

The field-level contract and adoption precedence are in
[`METADATA.md`](METADATA.md).

## Network and process boundaries

### Normal application runtime

No source module opens an internet connection or sends photo/face data to a
service. The `photo://` scheme is an in-process Tauri protocol that maps opaque
hexadecimal IDs to registered local sources and cache files; it is not an HTTP
upload endpoint.
YuNet and SFace run through the bundled ONNX Runtime on CPU.

Ember does start one local ExifTool subprocess on demand. Commands and metadata
travel over that child's stdin/stdout; ExifTool is not used as a network
service.

The source-build process is different from runtime: npm/Cargo obtain normal
dependencies, and the `ort` build can download its pinned native runtime unless
`ORT_LIB_PATH` supplies an offline copy.

### Development-only surfaces

- `npm run tauri:mcp` compiles the optional `mcp` feature and exposes the
  unauthenticated design-review bridge on loopback (`127.0.0.1`, port 9223).
  That feature is absent from ordinary dev/ship builds and should only run for
  an intentional local review session.
- Storybook binds to `127.0.0.1:6006`; its MCP endpoint is local and optional.
- The WebdriverIO server is compiled only with the `e2e` Cargo feature. The
  synthetic harness uses the separate `com.cleung.ember.e2e` application
  identifier and a dedicated loopback port.

Do not expose these loopback development tools through port forwarding or a
public interface. They are local testing capabilities, not authenticated
production APIs.

## Face-data deletion: what the command guarantees

**Delete all face data** is intentionally stronger than hiding the People
panel:

1. Phase one commits `enabled_mirror_ok = 0`, making an out-of-date
   `settings.toml` untrusted before further work begins.
2. Phase two takes SQLite's writer lock, disables indexing, increments the
   indexing epoch and wipe sequence, marks the chip sweep owed, deletes all
   person/face/rejection/scan rows, and writes `enabled = false` to
   `settings.toml` within the serialized critical section.
3. Any stale worker commit or chip publication then fails its epoch/row guards.
4. Ember sweeps every recognized face-chip artifact, including temporary chip
   files.
5. The sweep obligation is cleared only after a complete successful sweep.

If the settings write fails, the command reports it and the database remains
authoritative; launch reconciliation rewrites the file. If chip removal fails,
the command reports that the database data is gone and indexing is off, leaves
`chip_sweep_pending` set, and retries the sweep next launch. Cleanup failure is
never converted into a success message.

Deletion does **not** remove ratings, tags, action history, cached MakerNotes,
focus scores, general previews, performance reports, source photos, sidecars,
or files already in the macOS Trash. Re-enabling faces is a separate explicit
act and starts face indexing from scratch.

## Removing all Ember-local state

Ember currently has no single in-app command that erases every category in the
inventory. A full reset requires quitting every Ember process and deliberately
removing the application-support and cache directories listed above. Back up
anything needed first: removing `ember.sqlite3` permanently discards the local
journal, undo history, folder state, and face data. It does not reverse XMP or
xattr writes already made to photos, delete sidecars, or restore files from the
macOS Trash.

## Threat model

Ember is designed to protect against:

- process crashes between a verdict and later XMP work;
- stale background face work racing a user correction, model change, rescan,
  or privacy deletion;
- two instances of the same installed build sharing the database;
- partial face-chip cleanup and stale cache artifacts;
- accidental remote transfer through an application cloud/telemetry feature,
  because no such runtime feature exists.

Ember is **not** designed to protect against:

- another local account, administrator, malware, or backup system that can
  read the user's files;
- a compromised Ember binary, WebView, ExifTool, ONNX model, or dependency;
- forensic recovery from storage, backups, snapshots, or the macOS Trash;
- disclosure through screenshots, issue attachments, terminal output, or a
  development tool the user intentionally starts;
- concurrent use of a pre-rank/schema-v5-or-older Ember binary with a current
  one against the same database.

Ember does not encrypt its database or cache. Their confidentiality comes from
the macOS account and volume protections. The current Tauri configuration also
sets no Content Security Policy (`csp: null`); the application loads local
content today, but adding remote content or navigation requires a security
review and a restrictive CSP first. Bundled face-model hash mismatches are
currently logged as warnings and separated into a distinct model generation,
not treated as a hard launch failure.

## Safe development and reporting

- Use generated or clearly licensed public fixtures by default.
- Real-photo inspection requires explicit one-run consent and stays local.
- Use a disposable copy for any gate that can write ratings, sidecars, xattrs,
  face data, or Trash state.
- Never publish personal photos, face chips, person names, paths, EXIF, an
  Ember database, or raw real-photo gate output.
- The public README screenshot pipeline uses the generated fictional source in
  `docs/assets/`, a fixed synthetic folder, and the isolated E2E identifier.

Report a suspected privacy or security vulnerability through the private route
in [`SECURITY.md`](../SECURITY.md), not a public issue.
