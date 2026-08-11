# Faces repair pass — independent round-4 review

- **Date:** 2026-08-11
- **Base:** `e9ee8f0ac0d04ffc0e604a04a7963f8ae6c03469`; reviewed changes remain uncommitted
- **Reviewed:** the actual worktree described by `FABLE_FIX_REPORT_ROUND4.md`, with emphasis on every round-3 finding and its new regression
- **Method:** report only; no implementation files were changed

## Verdict

**Not mergeable yet.** Round 4 correctly fixes the normal scan-A/scan-B chip collision, session retries, the XMP stat-refresh guard, and PeoplePanel folder ownership. However, two load-bearing identity claims still fail at lifecycle/upgrade boundaries.

Minimum code blockers:

1. Chip revisions reset after privacy deletion, so a chip that survived a failed sweep can later satisfy a new scan's supposedly revision-identified URL.
2. Model release rank is not established on migrated owners and is not enforceable against a pre-rank binary, so the downgrade protection is incomplete on the upgrade path.

The real-library/runtime gates and the existing product decisions remain separate requirements after these blockers are fixed.

## Blockers

### 1. Chip identity is reused after privacy deletion

**Files:** `src-tauri/src/facestore.rs:315-322`, `src-tauri/src/facestore.rs:550-580`, `src-tauri/src/facestore.rs:1262-1273`, `src-tauri/src/preview.rs:424-429`, `src-tauri/src/protocol.rs:36-65`.

Round 4 makes `chip_revision` monotonic only while a photo's `face_scan` row exists. Privacy deletion removes every `face_scan` row. The next scan snapshots missing state as revision 0 and commits revision 1 again, producing the same `{photo}-f{index}r1.jpg` path and `/face/{photo}/{index}/1` URL used before deletion.

A concrete failing path is:

1. The pre-delete scan publishes revision-1 chips.
2. Privacy deletion commits, but its fallible sweep cannot remove one chip; the command reports the failure and leaves `chip_sweep_pending = 1`.
3. The user explicitly re-enables indexing before cleanup succeeds; `set_faces_enabled` does not gate on the pending sweep.
4. The new scan commits as revision 1, then the process dies before baking.
5. The production route serves the surviving pre-delete revision-1 bytes as the new detection. Repair also sees the expected path as present.

Thus the filename is not proof of scan identity across the very reset/failure lifecycle the privacy protocol supports. The new crash-between-two-scans test never deletes `face_scan`, so it cannot catch this reuse.

**Suggested remedy:** use a durable scan/chip token that survives face-data deletion (for example include the persistent epoch or a monotonic control-row sequence in the DB identity, filename, and URL), and add the exact delete-sweep-fails → re-enable → scan-commits → pre-bake-death regression.

### 2. The model-rank gate is incomplete on migrated databases and for old binaries

**Files:** `src-tauri/src/facestore.rs:179-190`, `src-tauri/src/facestore.rs:222-290`, `src-tauri/src/store.rs:200-221`; the migration test explicitly preserves the hole at `src-tauri/src/facestore.rs:2468-2490`.

A v5 owner migrates with `release = 0`. When the matching current binary presents its hashes at release 1, `claim_state` returns `Ready` without promoting that owner, and `register_gen` does the same. The DB therefore remains rank 0 indefinitely. A different release-1 model is then classified as a strict upgrade and may take ownership, even though the advertised equal-rank/different-hash rule says it must park. The test calls this bootstrap unavoidable, but after the matching ranked binary has identified the legacy row, the ambiguity is no longer unavoidable.

There is a second compatibility problem if “older binary” literally includes a pre-round-4 executable: it does not call `claim_state` at all. Its old insert omits `release`, receives the column default of 0, and orders ownership by `gen`; application-only checks in the new binary cannot prevent that writer from registering or committing. Protecting concurrent old/new app versions therefore needs a DB-enforced compatibility barrier, not only new-code checks.

**Suggested remedy:** atomically adopt a matching legacy owner into `MODEL_RELEASE` without stale-marking/reindexing, and—if concurrent pre-rank executables are in scope—enforce non-downgrade at the database/schema level so an old default-rank insert is rejected. Test legacy migration → matching ranked launch → unseen equal/older writer in both new-code and pre-rank SQL shapes.

## Should-fix

### 3. Migration can record schema v6 without the `release` column

**File:** `src-tauri/src/store.rs:208-221`.

Both `ALTER TABLE` errors are discarded indiscriminately. The subsequent update happens to verify `chip_revision`, but nothing verifies `face_model_gens.release` before `user_version` is set to 6. A non-duplicate failure can therefore leave a DB permanently marked v6 while every ranked-generation query fails.

**Suggested remedy:** ignore only a verified duplicate-column condition, perform/verify the migration transactionally, and set `user_version = 6` only after `PRAGMA table_info` confirms both columns.

### 4. The promised stale-chip sweep still silently skips enumeration failures

**Files:** `src-tauri/src/facestore.rs:674-690`, `src-tauri/src/faces.rs:542-549`.

`publish_chips` ignores a failing `read_dir` and drops individual entry errors with `filter_map(Result::ok)`, despite the report's claim that any sweep failure fails publication loudly. Repair uses the same silent pattern and can decide there is no stale work when it simply could not enumerate the cache.

**Suggested remedy:** treat `NotFound` as empty where appropriate and propagate every other directory/entry error from both stale detection and the guarded sweep; add an unreadable-directory/entry regression.

### 5. A `FaceChip` carries an exhausted retry budget into a new artifact identity

**File:** `src/components/PeoplePanel.tsx:57-84`.

The retry counter and pending timers live for the React component's lifetime, not for `(photoId, faceIndex, revision)`. A named-person row is keyed by person ID, so its representative can move to a new revision without remounting `FaceChip`. If the previous artifact exhausted 15 misses, the new URL receives one request but no retry after its repair enqueue; old timers can also advance the new artifact's counter.

**Suggested remedy:** reset and cancel retry state whenever the artifact identity changes, or key each `FaceChip` by the full identity; add a component/timer test for an exhausted old revision changing to a missing new revision.

### 6. XMP completion can delete a newer tag update with the same rating

**Files:** `src-tauri/src/xmp.rs:337-364`, `src-tauri/src/store.rs:742-768`.

The round-4 stat-refresh ordering is sound, but `xmp_done` uses only `(photo_id, rating)` as its compare-and-delete token. If a tag job is in flight and the user changes tags again without changing the rating, the queue row is updated with the new tags but the old job's completion still deletes it. The file keeps the old tags and the acknowledged newer edit is lost. This weakness predates round 4, but it remains on the end-to-end path modified and claimed complete here.

**Suggested remedy:** give each queued payload a monotonic version/token (or compare the complete immutable payload) and delete only the exact job that was written; add a barrier test with two tag values at one rating.

## Round-3 dispositions that now verify

- The ordinary scan-A → scan-B crash gap uses distinct filenames/URLs, returns 404 for B rather than serving A, and repairs B from stored rects.
- Encoded chips remain in RAM until the guarded SQLite writer transaction; the bake and repair privacy tests now use real thread rendezvous.
- Publication validates successful scan state, epoch, revision, and the expected artifact set.
- New ranked databases correctly park unseen lower/equal ranks, including controlled two-connection registration races.
- `SessionRetries` now gives three timed attempts per session, retries persisted errors after relaunch, resets on explicit fresh work, and reports retrying versus terminal truthfully.
- The XMP queue row survives stat-refresh failure and blocks face claim throughout the rewrite window.
- `key={folderId}` remounts PeoplePanel; the load guard suppresses stale state and stale failure notifications. The acknowledged lack of a DOM rendering test remains a coverage gap, not a discovered contradiction.
- The requested real UNIQUE-conflict rollback and stronger cache/refetch tests now exercise what their descriptions claim.

## Independent validation

- `npx tsc --noEmit` — pass
- `npm test -- --run` — pass, 5 files / 37 tests
- `cargo fmt --check` — pass
- `cargo clippy --all-targets -- -D warnings` — pass
- `cargo test` — pass outside the sandbox, 126 passed / 2 ignored. The sandbox run had only the expected macOS Trash permission failure.
- `cargo test -- --ignored detects_known_face_and_embeds_deterministically` — pass
- `git diff --check` — pass

No GUI, storm, real-library, cold-open, RSS/app-size, or rating-ack-latency gate was run in this review.

## Handoff

Fix and regress the two lifecycle/upgrade blockers before another mergeability review. The four should-fixes are bounded and should be addressed in the same pass, especially the migration verification and XMP exact-job completion. Preserve the in-memory chip staging and SQLite writer-lock privacy ordering while changing artifact identity.

After code review is clean, resolve the standing product decisions (`delete_person` exposure, clear-auto copy/behavior, janitor split), freeze the candidate, and run the real-library release-candidate gates.
