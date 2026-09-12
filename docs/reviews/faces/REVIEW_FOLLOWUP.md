# Faces repair-pass follow-up review

- **Date:** 2026-08-11
- **Branch head:** `e9ee8f0ac0d04ffc0e604a04a7963f8ae6c03469` (`faces`)
- **Reviewed state:** Opus 5's uncommitted repair pass (14 modified files plus `src/lib/session.test.ts`)
- **Original review:** `REVIEW.md`
- **Source plan:** private implementation plan (Rev 4; not published)
- **Method:** report only; this follow-up review made no implementation changes

## Verdict

**Send the repair pass back for another iteration.** It substantially improves the branch, and many individual fixes are sound, but both original privacy blockers remain open under cross-process/filesystem interleavings that the new tests do not exercise.

Do not treat the new guarantees documented in `CLAUDE.md` as established until the two blockers below are closed with deterministic concurrency tests.

## Blockers

### 1. The settings mirror can still be made stale-but-trusted by another process

**Files:** `src-tauri/src/facestore.rs:919-987`, especially `mark_settings_mirrored` at line 961; `src-tauri/src/lib.rs:1087-1100` and `1148-1159`.

`enabled_mirror_ok` is a single Boolean, and `mark_settings_mirrored()` sets it to 1 without proving that the file contains the DB state that is current at that moment. The DB transaction and `settings.toml` write are still separated.

A violating interleaving is:

1. Process A begins `set_faces_enabled(true)`: DB becomes enabled, mirror flag becomes 0; A pauses before writing the file.
2. Process B runs privacy deletion: DB becomes disabled and empty, B writes `enabled=false`, then marks the mirror valid.
3. Process A resumes, writes its stale `enabled=true`, then unconditionally marks the mirror valid.
4. Final state: DB disabled/empty, file says enabled, mirror flag says the file is trustworthy.
5. On relaunch, startup adopts the file and starts indexing from scratch. Delete-all did not stay disabled.

**Required remedy:** serialize the DB intent and file persistence across processes, or use a versioned/token protocol in which a stale completion cannot mark the mirror valid and a stale writer forces it back to untrusted. A Boolean without an expected value/version is insufficient. Holding the shared SQLite writer lock across the small settings write is one possible design; a correct versioned protocol or explicit cross-process file lock is another.

**Required regression test:** use two stores/connections plus barriers to reproduce the exact stale-writer ordering above. Assert that the final startup sync cannot adopt `enabled=true` after the later privacy delete.

### 2. A face-chip temp can still be created after deletion's final sweep

**Files:** `src-tauri/src/faces.rs:270-340`, `src-tauri/src/faces.rs:399-427`, `src-tauri/src/preview.rs:99-112`, `src-tauri/src/lib.rs:1087-1090`.

The SQLite write lock correctly orders the **publish rename** against deletion, but JPEG staging still writes a biometric temp before acquiring that lock. Deletion performs one post-commit directory sweep and does not drain or coordinate writers.

A violating interleaving is:

1. Repair A snapshots epoch/rects and decodes the preview, then pauses before `stage_chips`.
2. Process B commits privacy deletion, sweeps all chips/temps, and returns success.
3. Repair A resumes and creates `{id}-f{n}.tmp...` after the sweep.
4. If A crashes before `publish_chips` refuses and `discard_staged` runs, the biometric temp persists after deletion.

Even without a crash, a temp exists after the delete command has returned. In addition, `PreviewState::delete_all_face_chips()` returns `()` and silently ignores both `read_dir` and `remove_file` failures, so the command may report success while existing biometric files remain.

**Required remedy:** stage encoded chip bytes in memory until the serialized publish step, or explicitly cancel/drain and join every chip writer before the final sweep. Make the sweep fallible and propagate or durably retry cleanup failures. The guarantee must cover other processes and process death, not just a healthy caller that eventually invokes `discard_staged`.

**Required regression tests:**

- Barrier-controlled repair/bake that does not begin disk staging until after the delete transaction and sweep.
- Simulated writer death between staging and publish/cleanup.
- Cleanup failure (unreadable cache directory or failed removal) must not produce a successful privacy-delete result.

## Should-fix correctness and concurrency findings

### Chip publication is not tied to the exact successful scan

**File:** `src-tauri/src/facestore.rs:429-451`.

`publish_chips` ignores every `rename` result and returns success after checking only that some face row exists and the epoch matches. Pixel invalidation/manual rescan can mark a photo stale without changing the epoch; a bake from the prior scan can then publish stale chips after invalidation. Another successful scan in the same epoch can replace the rows before an older bake publishes as well.

**Suggested remedy:** fail on any rename error and guard publication with the exact scan identity/token (including successful status and the source/scan generation that produced the rects). Add tests for invalidation or a second commit landing between scan commit and chip publish.

### Exact-state undo still breaks across re-detection

**File:** `src-tauri/src/facestore.rs:1139-1295`.

`NamingOp.prior`, `rejections`, and `prior_auto` are keyed by face row IDs. `commit_scan` deletes and reinserts those rows while preserving `face_revision`, because a worker rescan is not a user edit. Undo therefore considers the photo eligible even though the recorded IDs may no longer exist or may have been reused for replacement rows.

Consequences include:

- Assignment restoration and rejection rollback can update/delete zero rows after a rescan, leaving the operation's effects in place.
- A pre-existing auto assignment carried to a replacement face receives a new ID not present in `prior_auto`, so undo may clear it again.

**Suggested remedy:** give pending undo state stable face lineage/op provenance through carry-over, update pending ops when rows are replaced, or define and enforce a safe invalidation rule for pending undo on re-detection. Add naming -> re-detection -> undo tests for assignments, rejections, and pre-existing autos.

### A newly started older-model worker can still displace a newer generation

**File:** `src-tauri/src/faces.rs:525-593` and engine initialization at `634-659`.

The drift guard runs only after `my_shas` has been populated by this worker's first initialization. An older binary starting against a DB owned by a newer model generation has `my_shas=None`; it can claim a job, initialize, and register its older models as the newest generation before the guard ever runs. The already-running newer worker then parks, so the last initializer can downgrade ownership.

**Suggested remedy:** identify/hash local models and compare with the current generation before claiming work or registering a new generation. Add a worker-level two-connection startup test, not only a pure `gen_drift` decision test.

### Per-photo terminal failures still leave the panel in permanent “Scanning” state

**Files:** error batching at `src-tauri/src/faces.rs:686-697`; counts at `src-tauri/src/facestore.rs:990-1007`; UI state at `src/components/PeoplePanel.tsx:391`.

Emitting an event makes the error count visible, but `scanned` counts only `status='ok'`, while the panel defines scanning as `scanned < total`. Once all retries are exhausted, a one-photo failure remains `Scanning... 0/1 (1 failed)` forever.

**Suggested remedy:** expose pending/retrying/terminal counts or another explicit worker state so the panel can distinguish queued retry work from a terminal per-photo failure. Add the last-photo/all-photos-fail cases.

### Preview generation still has a zero-value collision

**Files:** `src-tauri/src/preview.rs:208-236`, generation retention at line 273; invalidation branches at `src-tauri/src/lib.rs:82-120`.

`preview_gen()` returns 0 for absent entries. Leaving a folder removes its generation entry, so an in-flight job that originally snapshotted 0 also reads 0 after removal and is not discarded. The “no validity row” branch also deletes an untrusted preview without incrementing its generation.

**Suggested remedy:** assign every active photo a nonzero token; invalidate departing entries before removing bookkeeping; bump the token on every branch that removes/replaces a preview. Test the never-previously-invalidated photo leaving a folder mid-inference.

### The janitor still misses an active folder containing only trashed photos

**File:** `src-tauri/src/janitor.rs:74-100`.

The active folder is inferred from live protected IDs. If the open folder is empty or every photo is trashed, `protected` is empty, no active folder is inferred, and its unregenerable Trash-panel previews can be evicted.

**Suggested remedy:** pass/store the actual active folder ID rather than infer it from live photos. Add an all-trashed active-folder test.

### Folder-safe async frontend handling is still partial

**Files:** `src/components/PeoplePanel.tsx:139-161`; `src/lib/session.ts:442-490`.

PeoplePanel rejects late responses, but it continues rendering the previous folder's persons/clusters until the new responses arrive. During that interval, an action can target old face IDs while using the new `folderId`. Session-level `setPersonFilter`, `loadPersons`, and `peopleChanged` also do not verify the captured folder after their awaited IPC calls.

**Suggested remedy:** clear or key panel state on folder change and guard every post-await state/cache commit with the captured folder ID or a generation token.

### The XMP acceptance test does not cover the real queue/worker race

**Files:** queue ordering at `src-tauri/src/xmp.rs:319-323`; test at `src-tauri/src/xmp.rs:367`.

The test calls `write_meta` and `refresh_stats_after_rewrite` sequentially. It does not exercise the queue worker or the window after exiftool rewrites the file but before preview/face stats are refreshed. The face worker can observe the new file stat in that window, snapshot, and later commit a needless reindex because refreshing `face_scan.mtime/size` does not alter any commit guard.

**Suggested remedy:** add a barrier-controlled queue/face-worker test and make the metadata-only rewrite coordination visible to job claim/commit (for example with a DB token/pending state). Also avoid silently passing the test when exiftool is unavailable; use an explicit ignored/integration-test contract or a hard dependency in the acceptance environment.

## Test findings

### New TS tests that do not prove their names

**File:** `src/lib/session.test.ts`.

- `invalidates per photo` at line 115 never caches and later revisits the invalidated photo. It proves only that touching photo B does not immediately refetch current photo A.
- `accepts the folderless event` at line 121 asserts that no face fetch occurred—the same result would occur if the event were incorrectly dropped. It is vacuous with respect to acceptance.

**Suggested remedy:** cache both photos, invalidate one, navigate/revisit both, and assert only the touched photo refetches. Test folderless behavior through an observable panel/status refresh or extract/test the event-routing predicate directly.

### Privacy tests simulate seams sequentially rather than exercising concurrency

**Files:** `src-tauri/src/faces.rs:1044-1120`, `src-tauri/src/facestore.rs:2288-2376`.

The tests stage before deletion and manually call publish afterward. They do not run a writer concurrently, do not stage after the sweep, and do not test a stale settings writer completing after another process's delete.

**Suggested remedy:** use threads/connections with barriers so the production operations themselves block and resume at the dangerous points.

### Remaining test gaps are not all externally blocked

- GUI/photo-library gates and positive/negative identity fixtures genuinely require external fixtures or user input.
- Repair decode-count coverage is feasible with a test-only decoder injection/counter; lack of an injection point is not a blocker.
- Worker-refuses-claim-while-disabled is feasible by extracting/testing the full eligibility predicate or building a channel-controlled worker harness.
- Component-level FaceBadges/PeoplePanel coverage may require test infrastructure, but it is implementable and should not be described as externally blocked.
- The added rollback test proves transaction rollback through a foreign-key failure, not specifically the plan's UNIQUE-constraint case.

## Product decisions

### Existing-person correction path

This does not need a new UX decision. PLAN defines the intended split and `FACES_DEVIATIONS.md` only changes the affordance:

- If the typed/autocompleted name resolves to an existing person, call `face_assign`.
- If it is a genuinely new name, call `face_set_name`.

This preserves the typed-name UX while avoiding a dead command, an unreachable undo-registry entry, and an unnecessary folder-wide sweep for a single correction.

### Delete-person UI

This is a genuine product decision. Either add a confirmed destructive action to the named-person row or explicitly document the command as harness-only. Do not guess placement/copy without direction.

### Clear-auto behavior

Re-running the sweep is documented as intentional, but the button/title/toast promise a persistent clear. If the operation is really “drop and re-evaluate machine guesses,” rename its UI copy accordingly; otherwise change the behavior. This needs a product decision.

### Cache-janitor branch scope

Whether to split the janitor from Faces remains a branch/process decision. If retained here, its active-folder defect above still needs fixing.

## Fixes that look sound in this pass

The following changes are directionally correct and passed inspection, subject to normal final regression review:

- `face_scan.status='ok'` predicates added to prototype/calibration/sweep embedding reads.
- Source-stat probe moved into the scan write transaction (it materially narrows the original race and matches the plan's requested final recheck).
- Stale selection no longer creates an empty person.
- Self-merge is rejected/no-op at both boundaries.
- SPEC detection threshold corrected to 0.8.
- Face badges subscribe to discrete layout changes instead of the gesture-frame stream.
- People/session IPC failures are surfaced rather than interpreted as empty data.
- Existing-v4 migration test and general rollback coverage.
- Ambiguous one-old/two-new carry-over now lapses, although the new IoU-only `CARRY_MARGIN=0.10` remains an uncalibrated policy choice and should be reviewed against real re-detections.

## Independent verification

The following were rerun against the uncommitted repair pass:

- `tsc --noEmit` — passed.
- `npm test` — passed: 3 files, 27 tests.
- `cargo fmt --check` — passed.
- `git diff --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test` — 100 passed in the managed sandbox; the macOS Trash round-trip was the only sandbox failure and passed when rerun with Trash access, for 101 non-ignored tests total; 2 ignored.
- `faces::tests::detects_known_face_and_embeds_deterministically` — passed.

Not verified: GUI/photo-library gates, cold-open/RSS/app-size acceptance, rating-ack behavior with the new per-photo SQLite publish transaction, and subjective real-library recognition/carry-over behavior.

## Handoff instructions

1. Re-read `CLAUDE.md`, PLAN, `docs/FACES_DEVIATIONS.md`, `REVIEW.md`, and this file before editing.
2. Fix the two privacy blockers first and prove their exact interleavings with barriers; do not rely on sequential seam simulations.
3. Do not weaken or delete existing tests. Add focused regressions for every issue resolved.
4. Treat the existing-person correction split above as the default plan-conformant implementation. Pause only for the genuine product decisions identified here.
5. After fixes, rerun all independent verification commands and report unverified runtime gates explicitly.
6. Do not declare the branch mergeable; return it for another independent review.
