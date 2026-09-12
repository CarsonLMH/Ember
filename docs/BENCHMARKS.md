# Benchmarks

**Status: measured reference evidence, not a portable hardware claim.** The
performance budgets are the current product contract in [`SPEC.md`](../SPEC.md)
and [`AGENTS.md`](../AGENTS.md). This page defines the metrics and records the
latest accepted local gate output without pretending a private fixture and an
unrecorded host are a universal benchmark.

## Budgets

| Path | Contract |
|---|---:|
| Preloaded keypress to rendered flip, steady state | p99 ≤ 50 ms |
| Steady-state flip serves | 0 cache misses |
| Folder-open request to first image draw | ≤ 1,000 ms |

Rating durability is measured alongside the storm as the command round trip to
an fsync-backed SQLite commit. It is diagnostic evidence; the current product
contract does not assign it a separate hard threshold.

## What the metrics actually mean

### Flip latency

`perf.recordFlip` starts from the keyboard event timestamp, so queueing delay is
included. The sample is first recorded when `viewer.setPhoto` completes its
synchronous canvas draw.

For a visible, unoccluded window, two `requestAnimationFrame` callbacks may
upgrade that sample to an approximation of frame commit (“glass time”). WebKit
throttles animation frames for hidden/occluded windows, so hidden runs retain
the synchronous draw-complete measurement rather than interpreting a delayed
animation-frame callback as rendering latency.

Therefore:

- the hidden real-photo gate enforces the 50 ms threshold against
  **event-to-canvas-draw**, plus the separate zero-miss requirement;
- it does **not** prove that the physical display presented the frame within
  the same number; and
- `npm run e2e:perf` is the explicit visible path that can collect double-rAF
  approximations, but its embedded-driver/debug instrumentation makes it a
  150 ms regression tripwire rather than the product gate.

This distinction is intentional. Reporting a hidden 1 ms draw as 1 ms
photons-on-glass would be a lovely number and a false claim.

### Folder-open time

The `coldOpenMs` field starts when `session.openFolder(path)` begins and ends
after the selected photo has been decoded to an `ImageBitmap` and drawn to the
canvas. It includes folder scan/IPC and the image load path. It excludes process
launch, the native folder-picker interaction, and physical display commit.

The field name is historical. A result is only a truly cold first import when
the reporting protocol also clears Ember's disk cache and records OS cache
conditions. The reference run below did not record such a reset, so its values
are described as **folder-open-to-first-draw**, not disk-cold launch numbers.

### Cache misses and face activity

A steady-state flip is a miss when it is served by preview decode or original
fallback rather than the owned `ImageBitmap` cache. The gate requires zero.

The faces scenario is accepted only when backend counters change during the
measured window and the log says `facesSpike=ACTIVE`. Merely enabling faces is
not evidence that inference competed with the culling path.

## Reference run — 2026-09-12

Run context:

- Host: maintainer reference Mac; exact model, chip, RAM, macOS version, power
  mode, and thermal state were not captured. No exact hardware is inferred.
- Build path: `tauri dev` through the strict hidden gate configuration.
- Fixture: a private, disposable workspace copy containing 31 logical photos.
  The corpus is not committed or redistributable.
- Sample size: 30 actual forward flips were recorded in each storm. The viewer
  stops at the end of a 31-photo set rather than wrapping.
- Preflight: zoom test passed.
- The person-filter phase was explicitly skipped for this bounded run with
  `EMBER_SKIP_PEOPLETEST=1`; these numbers are not evidence for that phase.
- Git revision and cache-reset state were not captured with the result. Treat
  this as a local baseline, not a fully reproducible cross-machine record.

| Scenario | Recorded flips | Flip p99 | Non-cache serves | Folder open → first draw | Rating-ack p99 | Gate evidence |
|---|---:|---:|---:|---:|---:|---|
| Normal gate; face work not forced | 30 | **1.0 ms** | **0 / 30** | **86 ms** | 15 ms | `stormOk=true` |
| Face inference pinned active | 30 | **2.0 ms** | **0 / 30** | **591 ms** | 15 ms | `facesSpike=ACTIVE`, `stormOk=true` |

Both recorded scenarios are below the harness thresholds. The table does not
claim glass time, a disk-cold import, statistical confidence beyond 30 actual
flips, or performance on hardware that was not recorded.

## Reproducing the same gate shape

Prerequisites are the pinned Node/npm and Rust toolchains plus ExifTool. Close
heavy foreground work, make an approved disposable copy of a representative
photo folder, and use its absolute path:

```sh
EMBER_SKIP_PEOPLETEST=1 ./scripts/gate.sh /absolute/path/to/disposable-photo-copy
```

That command matches the bounded 2026-09-12 phase selection. Leave the
environment variable unset to run the full default gate, including the people
phase:

```sh
./scripts/gate.sh /absolute/path/to/disposable-photo-copy
```

The gate is off-screen and non-activating, but it uses Ember's production app
identifier. It may update the local database and write ratings, sidecars,
xattrs, and face records for the supplied copy. Never point it at an
irreplaceable library, and never publish a private fixture path or raw output.

For a result intended to be compared or cited, record:

1. Git commit and whether the working tree was dirty.
2. Mac model/chip, RAM, macOS version, power mode, and whether the machine was
   otherwise idle; omit serial numbers.
3. Fixture count and file mix without publishing private filenames or EXIF.
4. Whether Ember's preview cache and OS file cache were warm or reset.
5. Every gate phase run or skipped.
6. Flip count, p50/p95/p99/max, miss serves, folder-open time, rating-ack
   samples, and `facesSpike` state.
7. Whether samples were hidden draw-complete or visible double-rAF estimates.

Only compare results that use the same fixture shape, cache state, build mode,
visibility mode, and sample count. Otherwise the numbers are useful smoke
signals, not a speed ranking.

## Visible regression tripwire

`npm run e2e:perf` uses 320 generated 6000×4000 JPEGs and an instrumented E2E
debug build. It warms the cache, sends a real-key segment, runs an internal
300-flip storm without WebDriver polling, and reads the saved aggregate report.
Its assertions are at least 299 flips, zero misses, p99 ≤ 150 ms, and
folder-open ≤ 1,000 ms when reported.

Use it to catch broken cache paths or order-of-magnitude regressions. Do not
substitute its 150 ms line for the product's 50 ms contract, and warn before
running it because the window is intentionally visible and may take focus.
