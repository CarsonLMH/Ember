# Testing

**Status: current verification contract.** Commands and scope here match the
checked-in package scripts, Tauri overlays, CI workflow, and native test specs.
Historical review reports may explain why a test exists, but they are not a
substitute for running the current suite.

Ember deliberately separates deterministic checks from tests that need a
native window, a quiet Mac, or private real photos.

## Test tiers

| Tier | Command | Data and visibility | What it proves |
|---|---|---|---|
| Agent harness | `npm run test:harness` | No photos; terminal-only | Claude/Codex guidance, MCP parity, privacy-safe design-audit rules, hidden gate config, and screenshot-fixture invariants |
| Frontend unit | `npm test` | No native app | Session/order/key behavior and React/TypeScript logic under Vitest |
| Frontend builds | `npm run build` and `npm run storybook:build` | No native window | Strict TypeScript plus production and component-catalog bundles |
| Rust unit/integration | `cd src-tauri && cargo test` | Temporary files/SQLite; selected tests use local ExifTool | Persistence, migrations, pairing, XMP safety, trash/restore, previews, protocol, focus, and face concurrency rules |
| Rust static checks | `cd src-tauri && cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` | Terminal-only | Formatting and warning-free Rust across targets |
| Bundled-model inference | `cd src-tauri && cargo test -- --ignored detects_known_face_and_embeds_deterministically` | Checked-in licensed fixture and bundled models | Real YuNet/SFace load, detection, alignment, and deterministic embedding path |
| Hidden native E2E | `npm run e2e:build && npm run e2e` | Generated synthetic photos; strictly hidden | Native launch/open, real key paths, culling, kill-9 replay, and accessibility |
| Visible visual regression | `npm run e2e:visual` | Synthetic photos; **visible and focus-taking** | Stable app chrome and reviewed screenshot baselines |
| Visible perf tripwire | `npm run e2e:perf` | 320 generated JPEGs; **visible and focus-taking** | Instrumented debug-build regression tripwire, not the 50 ms product gate |
| Maintainer real-photo gate | `./scripts/gate.sh /absolute/path/to/disposable-copy` | Private disposable copy; hidden but uses production identifier | Zoom integrity and the hard flip/cache budget, including pinned-active face inference |

`cargo test` is non-visual, but it is not completely isolated from macOS: the
`trash_and_restore_roundtrip` test moves one generated temporary text file into
the real system Trash and immediately puts it back. It never selects a personal
file. An interrupted or failed run may leave that synthetic temp artifact in
the Trash.

The screenshot maintenance command, `npm run docs:screenshot`, is a separate
hidden E2E path. It regenerates a fixed privacy-safe fixture and writes
`docs/assets/ember-culling.jpg`; it is not a general screenshot tool. Like the
visual and performance commands, it reuses the native E2E binary, so run
`npm run e2e:build` first.

## What CI enforces

The macOS CI workflow in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)
runs on pushes to `main` and pull requests. It uses the checked-in Node/npm and
Rust declarations, read-only repository permissions, and pinned action SHAs.
The job currently performs:

1. Inert `npm ci`, registry-signature verification before the one allowlisted
   install script, production and critical full-graph audits, and pull-request
   dependency review that rejects new high or critical findings.
2. Agent-harness validation.
3. Frontend unit tests, production build, and Storybook build.
4. RustSec dependency audit, formatting, strict Clippy, the default Rust suite
   (including the bundled-model SHA pins), the optional MCP feature, and the
   targeted ignored bundled-model inference test, all locked to `Cargo.lock`.
5. The unsigned debug `.app` bundle used for local installs.
6. Hidden synthetic native E2E, including kill-9 journal replay and automated
   accessibility checks.

CI does **not** run visible visual comparisons, the instrumented visible perf
tripwire, the 50 ms private real-photo gate, or maintainer-library acceptance.
A green badge is therefore evidence for the deterministic merge checks—not a
claim that every hardware- and fixture-dependent gate ran on GitHub
infrastructure.

## Hidden synthetic native E2E

Build once, then run the suite:

```sh
npm run e2e:build
npm run e2e
```

The build includes the `e2e` Cargo feature and embedded WebDriver plugins; those
plugins are absent from normal app builds. The Tauri overlay:

- uses application identifier `com.cleung.ember.e2e`;
- starts with `create: false`, `visible: false`, and `focus: false`;
- disables background throttling for deterministic hidden work; and
- receives `EMBER_HIDDEN=1`, which applies a non-activating macOS policy before
  the event loop creates the hidden WebView.

Before a run, the harness removes only the E2E identifier's Application Support
and cache directories. It regenerates twelve synthetic 6000×4000 JPEG fixtures
with deterministic capture timestamps. It never opens the real Ember database
or a personal library.

The default hidden suite covers:

- folder open, navigation boundaries, filmstrip readiness, immersion, native
  Tab traversal, and zoom preservation;
- rating-in-place, clearing, single-JPEG trash/restore, exact/minimum star
  filters, and sort/reverse behavior (pair rollback is covered in Rust tests);
- a deliberate `kill -9` after journal-acknowledged ratings, followed by a
  fresh-process replay check; and
- automated WCAG A/AA checks plus shortcut-dialog focus containment.

The durability pair depends on the ordered `03` then `04` specs and a single
WDIO instance. Do not parallelize those files without redesigning their shared
test state.

## Explicitly visible checks

These commands can open a native window, take focus, and pull a user out of a
full-screen workspace. Warn the person using the Mac, agree on a time, and
batch the run. Build their shared native binary first with
`npm run e2e:build`.

### Visual regression

```sh
npm run e2e:visual
```

This runs the visible synthetic E2E path and compares stable chrome states
against `e2e/visual-baselines/`. If a deliberate UI change is correct, inspect
`e2e/.visual-output/actual/` before replacing baselines:

```sh
npm run e2e:visual:update
```

Never update baselines merely to make a failure green.

### Instrumented performance tripwire

```sh
npm run e2e:perf
```

Run it on a quiet machine. It uses a visible E2E debug build, an embedded
WebDriver server, and 320 generated JPEGs. The app dispatches its own 300-flip
storm after a real-key warm-up, then asserts at least 299 recorded flips, zero
non-cache serves, p99 at or below 150 ms, and folder-open time at or below one
second when present.

The 150 ms threshold catches order-of-magnitude regressions. It does not own
the product's 50 ms budget because the E2E instrumentation adds substantial
tail latency. See [Benchmarks](BENCHMARKS.md) for metric semantics.

## Maintainer-only real-photo gates

The real-photo gate is local because its evidence depends on a real decode
workload and a reference Mac:

```sh
./scripts/gate.sh /absolute/path/to/approved-disposable-photo-copy
```

It runs four hidden phases by default:

1. zoom integrity;
2. a normal flip storm;
3. a flip storm with face inference forced to remain active; and
4. a person-filter/name/storm/cleanup flow.

The hidden gate uses `src-tauri/tauri.gate.conf.json`, isolated Vite port 14210,
and no visible/focused window. Unlike synthetic E2E, it retains the production
application identifier and can write database state, XMP, xattrs, sidecars, and
face data for the supplied folder. The fixture must therefore be an approved
disposable copy, never an irreplaceable photo library. Keep its path and output
private.

`EMBER_SKIP_PEOPLETEST=1` may explicitly omit the fourth phase for a bounded
run, but that is a recorded skip—not a full gate pass.

The aggregate command below also formats Rust and may terminate matching debug
development processes before it starts the gate:

```sh
./scripts/checks.sh /absolute/path/to/approved-disposable-photo-copy
```

Confirm the fixture and warn before running it on somebody's active Mac.

## Which checks a change needs

- Documentation-only: link/command validation and `git diff --check`.
- Frontend logic: focused Vitest, full `npm test`, and `npm run build`.
- React chrome: add/update a stable Storybook story; build Storybook; run the
  hidden native flow; schedule visible visual review when appearance changed.
- Rust persistence, filesystem, concurrency, preview/protocol, XMP, or face
  work: focused regression first, full Rust suite, fmt, strict Clippy, hidden
  native durability where relevant, and the real-photo gate for hot-path work.
- Agent guidance, skills, or MCP configuration: `npm run test:harness` and a
  fresh Claude/Codex session for discovery.
- Release candidate: all applicable deterministic checks, hidden E2E, planned
  visible checks, real-photo gates, and owner acceptance on real photos.

Every skipped check belongs in the handoff with a reason. “It probably still
works” is not a test tier.

## Fixture privacy

Generated and clearly licensed public fixtures are the default. A real-photo
test is an explicit local exception: use a disposable copy, do not publish its
path or output, and do not retain personal names, EXIF, face labels, screenshots,
or database contents in issues or artifacts. See [`PRIVACY.md`](PRIVACY.md).
