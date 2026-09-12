# Faces repair pass — independent round 3 review

- **Date:** 2026-08-11
- **Branch head:** `e9ee8f0ac0d04ffc0e604a04a7963f8ae6c03469` (`faces`)
- **Reviewed state:** the uncommitted implementation described by `FABLE_FIX_REPORT.md`
- **Inputs:** private implementation plan (Rev 4; not published), `REVIEW.md`, `REVIEW_FOLLOWUP.md`, `FABLE_FIX_REPORT.md`, the full worktree diff, and the tests
- **Method:** report only; this review made no implementation changes

## Verdict

**Not mergeable yet.** The two original privacy-deletion blockers are now closed for cooperating instances of this code, and the replacement designs are materially stronger. This review found two different blockers, plus several should-fix correctness and test gaps that the fix report incorrectly marked complete.

Minimum blockers:

1. Existing face-chip files have no scan identity, so a crash after re-detection commits but before baking can permanently serve chips from the previous detection.
2. An older model triple that has never appeared in this particular DB is indistinguishable from an upgrade and can still displace the newer generation.

The runtime/photo-library gates and the product decisions listed in `FABLE_FIX_REPORT.md` remain required after the code blockers are fixed.

## Original privacy blockers

### B1 — settings mirror race: closed

**Files:** `src-tauri/src/facestore.rs:1126-1286`, `src-tauri/src/lib.rs:1106-1127`.

The phase-1 untrusted marker plus phase-2 `BEGIN IMMEDIATE` critical section closes the original stale-file-writer interleaving:

- every app-driven DB/file transition is ordered by SQLite's cross-process writer lock;
- a crash or failed file write leaves `enabled_mirror_ok = 0` from phase 1;
- `wipe_seq` prevents an enable whose intent predates a completed delete from applying later;
- a superseded writer mirrors the current DB value rather than its stale intent.

I found no same-version cooperating-process ordering that can leave a stale `enabled=true` file trusted after the later delete. Holding the writer lock over filesystem I/O still needs the planned rating-latency/runtime measurement, but that is a performance gate rather than a correctness hole.

The new test uses the exact phase ordering and two Stores. It is not literally a channel-barrier test as the report says, and its file is a mutex-backed stand-in rather than `settings.toml`, but it exercises the protocol defect that mattered.

### B2 — post-delete chip staging: closed

**Files:** `src-tauri/src/faces.rs:261-321`, `src-tauri/src/facestore.rs:594-644`, `src-tauri/src/preview.rs:497-506`, `src-tauri/src/lib.rs:1106-1127`.

Encoding into `Vec<u8>` and doing the first filesystem write only after `publish_chips` obtains the SQLite writer lock closes the original privacy race. Without a later explicit re-enable:

- a publish that wins the lock precedes the wipe and its files are visible to the subsequent sweep;
- a wipe that wins deletes the rows and bumps the epoch, so a later publisher fails its guards before writing anything;
- a writer paused or killed before publication holds only RAM, not a disk temp;
- sweep failures are returned and leave a durable retry marker.

The repair-path test genuinely uses a thread and rendezvous. `a_bake_parked_before_publication_cannot_outrun_a_delete` at `src-tauri/src/faces.rs:1202` is still sequential despite its description, but both paths share the same in-memory encoder and guarded publisher, so this is a coverage overclaim rather than evidence that the privacy invariant is false.

## Blockers

### 1. Existing chip files can survive a new scan and masquerade as current chips

**Files:** `src-tauri/src/faces.rs:290-320`, `src-tauri/src/faces.rs:412-444`, `src-tauri/src/faces.rs:871-885`, `src-tauri/src/protocol.rs:33-57`.

`face_revision` guards a *new write*, but the on-disk filename remains only `{photo_id}-f{index}.jpg`. Neither the protocol route nor repair knows which scan produced an existing file. Repair explicitly treats existence as validity.

A failing end-to-end path is:

1. Scan A has face rows and published chips.
2. A model change or another re-detection commits scan B, replacing its rects and bumping `face_revision`.
3. The process dies after `commit_scan` but before `bake_chips`, the exact crash gap repair is documented to heal.
4. Scan A's chip filenames still exist.
5. The protocol serves those bytes immediately without consulting the DB revision.
6. `repair_photo_inner` filters to missing paths, finds none, and never regenerates them.

This can show a crop belonging to the previous face geometry indefinitely. There is also a live interval between commit and bake in which the route can serve the old file; using the same URL means a UI element is not guaranteed to reload after the overwrite.

The test at `src-tauri/src/faces.rs:1134` starts with committed rows **and an empty cache**, so it proves cache-miss repair, not crash-between-two-scans repair.

**Required remedy:** attach the scan identity to the cached artifact (revisioned filename/manifest or equivalent), or invalidate the complete previous chip set in a way serialized with the row transition so an old file can never satisfy a new scan. Make the protocol and repair validate that identity rather than equating “path exists” with “current.”

**Required regression test:** publish visibly distinguishable chips for scan A, commit scan B with moved rects, abort before B's bake, request the chip through the production route/repair path, and prove that A's bytes are never served as B's.

### 2. A never-before-seen older model can still take ownership from a newer one

**Files:** `src-tauri/src/facestore.rs:178-280`, `src-tauri/src/faces.rs:597-608`; incomplete test at `src-tauri/src/faces.rs:1441-1505`.

The gate knows only whether the worker's exact hash triple has ever appeared in this DB:

- a differing triple found in history becomes `Park`;
- a differing triple absent from history becomes `Unregistered` and may register as the newest generation.

Absence does not establish that a model is newer. On a DB first indexed by version 2, a version-1 binary whose hashes have never appeared is classified as an upgrade, registers generation 2, stale-marks the real newer scans, and parks the newer worker. The same problem makes two previously unseen versions racing on a fresh DB “last registrar wins,” regardless of actual version order.

The new test registers the old triple first, then the new triple, then revisits the already-known old triple. It never tests **new first, unseen old second**, which is the case the implementation cannot distinguish.

**Required remedy:** store and compare a monotonic model/preprocessing release rank (or an explicit ordered registry of supported hashes). A differing equal/older rank must park; only a strictly newer rank may register over the current owner. Re-check it inside `BEGIN IMMEDIATE` as the current code does.

**Required regression tests:** initialize a fresh DB with the new model and then present a never-registered old model; also run both registration orders with barriers. The old model must never become current.

## Should-fix correctness findings

### 3. Scan errors are declared terminal while retries remain, and are never retried after relaunch

**Files:** `src-tauri/src/faces.rs:453-475`, `src-tauri/src/faces.rs:650-669`, `src-tauri/src/faces.rs:727-737`, `src-tauri/src/facestore.rs:1289-1308`, `src/lib/ipc.ts:228-245`.

After the first failure, `record_scan_error` writes `status='error'`, while `pending` excludes every error row. The panel therefore says the run is finished during the 5s/30s/180s retry schedule, even though the worker will touch that photo again.

There is a second defect: after relaunch the in-memory `backoff` map is empty, and `needs_scan` permits an error row only when `retryable=true`. A persisted error is therefore never retried in the new session, contrary to the plan's “cap 3 tries per session, reset on relaunch.”

The new status test calls `record_scan_error` directly and assumes that one call is terminal; it does not exercise the worker or its backoff state.

**Suggested remedy:** distinguish retrying from terminal state (persisted status or an explicit worker/status channel), set terminal only when the session cap is exhausted, and seed persisted errors for a fresh session retry. Add a controlled worker test covering first failure, scheduled retry, exhausted retry, and relaunch.

### 4. XMP coordination drops the queue guard even when stat refresh fails

**Files:** `src-tauri/src/xmp.rs:319-365`; test at `src-tauri/src/xmp.rs:410-470`.

`refresh_stats_after_rewrite` returns `()` and silently ignores:

- metadata lookup failure;
- `set_preview_stat` failure;
- `face_scan_refresh_stat` failure.

The worker then calls `xmp_done` regardless, also ignoring its result. If exiftool succeeded but the face-stat refresh failed, removing the queue row exposes the rewritten mtime against the old `face_scan` stat, reopening the needless-reindex path the guard was added to prevent.

The test is a sequential call of `write_meta`, the claim predicate, the refresh helper, and `xmp_done`; it does not run the queue worker or inject a refresh failure despite the report calling it a queue/worker race test.

**Suggested remedy:** make refresh return `Result`, retain/retry or mark the queue job failed unless all required stat updates succeed, and propagate `xmp_done` errors. Add a barrier-controlled worker test plus an injected refresh-failure case proving the queue row remains.

### 5. Partial encoding/publication can leave stale or orphaned chip files

**Files:** `src-tauri/src/faces.rs:261-279`, `src-tauri/src/faces.rs:303-320`, `src-tauri/src/facestore.rs:631-643`, `src-tauri/src/preview.rs:497-505`.

`encode_chips` silently skips a crop whose resize or JPEG encoding fails. If at least one other crop succeeds, the subset is reported as `Published`; an older file at a skipped index remains and repair will treat it as valid. If publication fails part-way, only destinations written in this attempt are removed; untouched old destinations remain. Finally, `publish_bytes` does not remove its temp when `std::fs::write` itself returns an error.

This compounds blocker 1. Encoding and publication should have an all-expected-artifacts contract, and any failure should leave no path that can be mistaken for the current revision. Add failures at resize/encode, temp write, rename, and the second of multiple outputs.

### 6. PeoplePanel's old-folder state is cleared after render, not atomically with the folder change

**File:** `src/components/PeoplePanel.tsx:137-176`.

The state reset is a passive `useEffect`. On the render where `folderId` changes, React can still render the previous persons/clusters under the new prop until the effect runs. The load freshness predicate is only a sequence number, and that number is not advanced until the new effect invokes `load`, leaving a small window for an old response to pass as fresh.

This is much narrower than the original unguarded async behavior, and the session-level post-await guards are sound, but the report's claim that the panel can no longer render or act on old rows is stronger than the implementation.

**Suggested remedy:** key/remount the folder-scoped panel state by `folderId`, or store the state's owning folder and render/action-gate on an exact match. Add a component test with old rows visible while the folder prop changes and old promises resolve.

## Test/report discrepancies

- `a_bake_parked_before_publication_cannot_outrun_a_delete` is sequential: it manually encodes, runs deletion, then calls publish. Only the repair variant uses the rendezvous.
- The settings race uses two Stores but no channel barrier and no real settings file. Its exact phase ordering is still useful, but the report should not call it a real-thread/barrier test.
- `carry_over_is_delete_then_insert_and_rolls_back_intact` proves UNIQUE rejection in a separate statement, then proves transactional rollback using a **FOREIGN KEY** failure. It still does not force the requested mid-insert UNIQUE failure (`src-tauri/src/facestore.rs:3420-3477`).
- Component rendering tests remain absent, as the report acknowledges.
- GUI/photo-library, stress, cold-open, RSS/app-size, rating-latency, identity-fixture, and subjective carry-over gates remain unverified.

## Fixes that did verify cleanly

- The settings-mirror and in-memory chip-staging privacy designs close the original two blockers.
- Publication now checks epoch, successful scan status, and the exact revision before a filesystem write.
- Re-detection safely invalidates an undo operation rather than applying it to replacement row IDs.
- Preview tokens are nonzero for listed photos and change across removal/re-entry.
- The janitor receives this process's actual active folder instead of inferring it from live photo IDs.
- Existing-person on-photo correction reaches `face_assign`.
- The revised TypeScript cache invalidation and folderless-event tests now exercise observable effects.
- Stale/error scans are excluded from prototype and sweep inputs; self-merge, stale selections, and source re-probe fixes remain sound.

## Independent verification

- `npx tsc --noEmit` — pass
- `npm test -- --run` — pass, 4 files / 33 tests
- `cargo fmt --check` — pass
- `cargo clippy --all-targets -- -D warnings` — pass
- `cargo test` — 113 passed in the sandbox, one macOS Trash permission failure, 2 ignored; the Trash test passed when rerun outside the sandbox, yielding 114 non-ignored passes overall
- `cargo test -- --ignored detects_known_face_and_embeds_deterministically` — pass
- `git diff --check` — pass

No GUI or real-library runtime gate was run in this review.

## Handoff

Fix the two blockers first and add the exact regressions described above. Then address the retry-state and XMP failure-boundary findings before asking for another mergeability review. Do not weaken the new privacy protocols while fixing chip cache identity; filesystem writes of face crops must remain behind the SQLite publication guard.

After the next independent code review is clean, freeze the candidate and run the real-library release-candidate validation listed in `FABLE_FIX_REPORT.md`.
