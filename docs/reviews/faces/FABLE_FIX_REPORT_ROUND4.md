# Faces repair pass — response to `REVIEW_ROUND3.md` (round 4)

- **Date:** 2026-08-11
- **Base commit:** `e9ee8f0ac0d04ffc0e604a04a7963f8ae6c03469` (`faces`), unchanged — nothing committed
- **Inputs:** private implementation plan (Rev 4; not published), `REVIEW.md`, `REVIEW_FOLLOWUP.md`,
  `FABLE_FIX_REPORT.md` (treated as hypotheses), `REVIEW_ROUND3.md` (work queue),
  `docs/FACES_DEVIATIONS.md`, `SPEC.md`, `CLAUDE.md`, the complete worktree diff
- **Scope:** repaired the uncommitted working tree in place; no resets, no rewrites of unrelated
  work, no edits to any review file. Both verified privacy protocols (settings two-phase intent,
  in-memory chip staging behind the guarded publish) are preserved unweakened — the chip-identity
  fix *adds* guards to `publish_chips`; it removes none.

---

## 1. Finding-by-finding disposition of `REVIEW_ROUND3.md`

| # | Finding | Status |
|---|---|---|
| B1 | Settings mirror race (previously closed) | **Preserved**; test strengthened (real `settings.toml`, plus a writer-lock contention test) |
| B2 | Post-delete chip staging (previously closed) | **Preserved**; the sequential bake test is now genuinely thread-and-barrier |
| 1 | Chip files without scan identity can masquerade as current chips | **Fixed** — revisioned artifact identity, end to end |
| 2 | A never-before-seen older model can take ownership | **Fixed** — monotonic model release rank |
| 3 | Errors declared terminal mid-schedule; never retried after relaunch | **Fixed** — session retry object + truthful retrying/terminal status |
| 4 | XMP stat-refresh failures drop the queue guard | **Fixed** — fallible refresh; row survives failure; `xmp_done` errors handled |
| 5 | Partial encode/publication leaves stale or orphaned chip files | **Fixed** — all-or-nothing encode + all-expected-artifacts publish contract + temp cleanup on every failure path |
| 6 | PeoplePanel clears old-folder state after render | **Fixed** — panel remounted by `key={folderId}`; extracted, unit-tested load guard; rendering-level DOM coverage remains an acknowledged gap (see §7) |
| T1 | `a_bake_parked_before_publication…` sequential despite description | **Fixed** — real thread, rendezvous barrier |
| T2 | Settings race test: no real file, described as barrier test | **Fixed** — main interleaving test now uses the real `settings.toml` writer/loader; a NEW test parks the delete *inside* its writer-lock critical section and proves a concurrent enable cannot complete until it commits (and then yields). The two failure-injection variants (`disk full`) still use closures — a real file cannot be made to fail on demand — and their comments say only what they do |
| T3 | Carry-over rollback proves FK, not mid-insert UNIQUE | **Fixed** — TEMP-trigger injection fires UNIQUE(photo_id, face_index) on the second insert, mid-transaction; original FK test kept |
| T4 | Sequential predicate test described as queue/worker concurrency | **Fixed** — the XMP test now runs the production job on its own thread, parked mid-window, with the face claim interrogating concurrently on its own connection |
| T5 | Component rendering tests absent | **Partially fixed** — `makeLoadGuard` extracted + 3 vitest cases; no DOM harness added (reported, not claimed) |
| T6 | Runtime/photo-library gates unverified | **Not fixed — externally blocked** (needs GUI + real library; unchanged list in §7) |

Repair-decode-count and repair-rendezvous tests are kept (updated for revisioned names, not weakened).

---

## 2. Blocker 1 — chip scan identity

### Design

A chip artifact now names the exact detection it belongs to, in three places at once:

1. **DB**: new column `face_scan.chip_revision` (schema v6), set **only by `commit_scan`**, always to
   the same value as the new `face_revision` (values are drawn from the same strictly-increasing
   counter, so each commit's value is unique per photo). User edits still bump `face_revision` but
   leave `chip_revision` alone — the rects didn't move, so existing crops stay valid and a naming
   no longer costs a folder of rebakes.
2. **Filename**: chips are `{id}-f{n}r{rev}.jpg` (`preview::face_chip_name`). A file surviving from
   an older scan has a *different name*; "the path exists" and "the artifact is current" are now the
   same statement, decided by the filesystem without a DB read on the protocol's hot path.
3. **URL**: the route is `photo://face/{id}/{n}/{rev}`; `ChipRef`/`PersonOut` carry the revision to
   the frontend. After a re-detection the URL itself changes, so WebKit's image cache can never show
   the old crop under the new identity (the review's browser-cache concern).

`facestore::publish_chips` (still the only code path that may write a face crop, still
`BEGIN IMMEDIATE`, chips still arriving as in-memory bytes) now enforces, inside the critical
section:

- **Identity guard:** `face_scan.status = 'ok' AND chip_revision = revision` plus the epoch. (The
  old guard used `face_revision`, which also made a bake refuse over a mere user edit; the new one
  is tighter about detections and looser about edits — both correctly.)
- **All-expected-artifacts contract:** the DB's rects at this revision define the complete artifact
  set. Every expected path must be supplied or already exist on disk (a file at a `r{rev}` name can
  only have been written by a publish of that same revision — the name is the proof); a supplied
  path outside the set is an error; an incomplete set is an error, never `Published`.
- **Stale sweep under the lock:** every other chip artifact of the photo (older revisions, legacy
  unrevisioned names from pre-upgrade installs, temps of dead writers) is removed. A sweep failure
  fails the publish loudly.
- **Zero faces is a real publish:** empty expected set, empty payload — the publish is what sweeps
  the previous detection's files, under the same guards as any other.

Repair (`repair_photo_inner`) validates identity, not existence: *missing* = expected artifacts not
on disk; *stale* = photo-chip files whose names are not in the expected set. Missing → one preview
decode re-encodes them (one decode per photo and per-photo dedup preserved; the decode-count test
still counts). Stale-only → a guarded empty publish sweeps without any decode. `preview::publish_bytes`
now removes its temp when the initial `fs::write` fails, not just on rename failure.

### Ordering argument (why each failing interleaving is now impossible)

- **Crash after scan B commits, before B bakes** (the review's end-to-end path): A's files still
  exist — under A's names. The UI refetches rows, gets B's `chip_revision`, requests B's URL; the
  route can only ever open B's filename, which doesn't exist → 404 + repair enqueue. Repair reads
  `face_chip_source` (epoch + chip_revision + rects in one snapshot), sees A's files as stale and
  B's as missing, decodes once, publishes B's bytes and sweeps A's — `protocol.rs::
  a_crash_between_two_scans_never_serves_the_old_chips_as_new` drives exactly this through the
  production `handle()` and the production repair job, with visibly different crops, and asserts
  A's bytes are never served as B's and B's correct bytes eventually are.
- **Live commit-to-bake interval:** same mechanism, no crash needed — from the commit on, current
  URLs name B's files; A's file cannot answer them. (A *stale UI* holding A's URL can still read A's
  bytes until the sweep — but explicitly as revision-A artifacts, never as B's chip; the panel
  refreshes on the same `faces-progress` event that announces the commit.)
- **Old bake racing a new scan:** publish and `commit_scan` both take SQLite's single cross-process
  writer lock. Whichever commits first, the later one decides against the committed state: a bake
  whose `chip_revision` no longer matches refuses without touching a file (it does not even sweep).
- **Partial publication:** a mid-set failure removes the chips it wrote and its temps and returns
  `Err`; whatever state the sweep left, no remaining file can satisfy a current URL wrongly, because
  wrong-revision names can't collide with current ones. There is no "old path that appears valid."
- **Privacy protocol:** untouched — publish still refuses after a wipe (scan rows gone, epoch
  moved), staging still never touches disk, `chip_sweep_pending` still fails closed, and
  `is_face_chip_file` now *also* matches legacy names so pre-upgrade chips are swept too.

**Files:** `facestore.rs` (schema, `commit_scan`, `publish_chips`, `face_chip_source`, DTO queries),
`preview.rs` (`face_chip_name`, `face_chip_path`, `is_photo_chip_file`, `delete_face_chips`,
`publish_bytes`), `faces.rs` (`encode_chips`, `bake_chips`, `repair_photo_inner`, `BakeTarget`),
`protocol.rs` (route), `store.rs` (v6 migration), `lib.rs` (call sites), `src/lib/ipc.ts`,
`src/components/PeoplePanel.tsx` (revision-carrying chip refs/URLs).

**Tests added/updated:** the production-path regression above; `chip_repair_removes_stale_artifacts_
even_with_nothing_missing`; `a_publish_that_fails_midway_rolls_back_and_cleans_its_temps` (rename
failure part-way through multiple outputs + temp-write failure via an unwritable cache dir);
`an_incomplete_chip_set_cannot_publish` (contract, both directions); `a_bake_from_a_superseded_scan_
refuses_to_publish` (stale + re-detected); `face_route_never_serves_another_revisions_chip`;
reworked facestore publish tests to the expected-path contract; filename-shape tests. On
resize/encode failure: no input constructible from a decodable preview can make them fail (crops are
clamped; even a synthetic 0×0 image resizes cleanly — verified while writing the test), so that leg
is covered by `encode_chips` propagating with `?` plus the incomplete-set publish refusal; this is
stated in the test comment rather than faked with an impossible input.

---

## 3. Blocker 2 — model release rank

### Design

`faces::MODEL_RELEASE: i64` is a monotonic rank of the bundled (detector, recognizer, prep) set,
bumped whenever any of the three changes (documented at the constant and in CLAUDE.md).
`face_model_gens` gains a `release` column; the owner is the row with the highest release (gen
tie-breaks only pre-rank legacy rows, which all read 0 — for ranked rows the register gate keeps
releases strictly increasing, so rank alone decides).

`claim_state(det, rec, prep, release)`:

- exact hash triple matches the owner → `Ready(gen)` **whatever the ranks** (identical models are
  the same embedding space; this is also what spares an upgraded install a pointless reindex);
- `release > owner.release` → `Unregistered` (may register);
- otherwise → `Park` — including a triple this DB has *never seen* (absence is not evidence of an
  upgrade) and an equal rank with different hashes (a build error must not win by arrival order).

`register_gen` re-checks all of it **inside `BEGIN IMMEDIATE`** before inserting, bumping the epoch
and stale-marking older-generation scans; the disabled-library guard is preserved (checked first in
both functions, and registration into a disabled library is refused). Generation row id and
registration time play no part in ordering.

### Why the failing interleavings are now impossible

- **New first, unseen old second** (the case the old gate could not express): the old binary's
  hashes are absent from history, but its rank is lower than the owner's → `Park` from both
  `claim_state` and a direct `register_gen`. Proven by `an_unseen_older_model_can_never_take_a_
  library_from_a_newer_one`.
- **Decision/registration race:** a binary that read "library empty → I may register" and stalls,
  while a newer one registers in the gap, is parked by its own in-lock re-check — registration
  order can never beat release order. Proven with a rendezvous in `registration_order_cannot_beat_
  release_order` (both orders).
- **Equal rank, different hashes:** parks whichever arrives second; the sitting owner stays.
- The old-first/new-upgrades order, disable/re-enable, and direct-re-register cases live in the
  reworked `the_claim_gate_parks_an_older_worker_and_refuses_a_disabled_library`.

**Files:** `facestore.rs` (`current_gen` → rank ordering, `claim_state`, `register_gen`,
`gen_of_models` removed, test-only `ensure_gen` auto-ranks), `faces.rs` (`MODEL_RELEASE`, worker
call sites), `store.rs` (migration), `CLAUDE.md`.

---

## 4. Retry semantics and truthful status (finding 3)

`SessionRetries` (faces.rs, public and directly testable) is the plan's contract as an object:
three tries per session at 5s/30s/180s; `eligible()` is true for a photo with **no session entry**
— which is precisely what makes a persisted `error` row retryable again after relaunch, closing the
"new session's empty backoff map never retries" defect; a photo waiting out its delay is skipped
(no hot loop; the worker's 500ms idle sleep is unchanged); `on_failure` reports when the budget is
spent; a **fresh claim** (stale/pixel/model change — e.g. the user's explicit rescan of an exhausted
photo) resets the budget, without which a rescan after exhaustion would sit pending forever.

Status: `face_scan_status(folder, exhausted)` splits error rows into `retrying` (counted inside
`pending`, so `isScanning` stays true and the panel shows "N retrying") and `errors` (terminal
only). The exhausted set is the worker's session view (`faces::exhausted_errors()`), display-only —
never a correctness guard; each process reports what its own worker knows. The lifetime
`face_scan.attempts` diagnostic is untouched (asserted at 3 after three failures).

**Test:** `retries_run_three_per_session_then_terminal_and_reset_on_relaunch` walks first failure →
waiting backoff (claim refused, status retrying/pending) → scheduled retries at the exact
boundaries → terminal exhaustion (status errors=1/pending=0) → relaunch reset (fresh `SessionRetries`
claims again, status retrying) → success after retry → rescan-resets-budget, all through
`claim_photo` + `SessionRetries` + `record_scan_error`/`commit_scan` + `face_scan_status` in the
worker's exact order under a controlled clock. Honest caveat: the loop *thread* itself is not spun
up (it needs a Tauri `AppHandle`); every decision it makes routes through the functions exercised.
The panel-side predicate keeps its vitest coverage (`isScanning` with a retrying photo added).

**Files:** `faces.rs`, `facestore.rs` (`FaceScanStatus` + query), `lib.rs` (command), `src/lib/ipc.ts`,
`PeoplePanel.tsx` (retrying display), `src/lib/ipc.test.ts`.

---

## 5. XMP coordination failure paths (finding 4)

`refresh_stats_after_rewrite` returns `Result`: metadata stat failure, preview-stat lookup/update
failure, and `face_scan` stat failure all propagate. The queue worker's per-job body is extracted to
`process_job_inner` (with a test seam between rewrite and refresh): the queue row — the token that
makes `claim_photo` refuse the photo — is removed **only after the refresh succeeded**; on refresh
failure the row goes back through the queue's existing policy via `xmp_error` (retried while
attempts < 5, then parked visibly as `failed` with its message, re-armable by the existing retry
command — the rewrite is idempotent so re-running it is harmless); an `xmp_done` failure is logged
and leaves the row for the next pass instead of vanishing. A metadata-only rewrite still never
causes a face reindex — that is the property the guard preserves and both tests end by proving,
with the pixel-change control claim.

**Tests:** `a_rating_write_never_costs_a_face_reindex` reworked — the production job runs on its own
thread, a rendezvous parks it *after* exiftool has really rewritten the file and *before* the stat
refresh, and a face worker's claim gate on its own connection refuses at that instant (plus queued,
drained, and control instants). `a_failed_stat_refresh_keeps_the_queue_guard_until_a_retry_lands`
injects the refresh failure through the seam (the file is whisked away mid-window), proves the row
and the claim guard survive, then proves a successful retry finally releases them.

---

## 6. PeoplePanel folder ownership (finding 6)

App mounts the panel with `key={state.folderId}`: a folder change **replaces the component
instance**, so every piece of folder-scoped state (rows, selections, prompts, toasts, in-flight
loads) belongs to exactly one folder for the instance's whole life — there is no render frame where
old rows can appear, or be acted on, under the new folder id. The passive reset `useEffect` is gone.
The load-freshness logic is extracted to `src/lib/loadguard.ts` (`makeLoadGuard`): each `begin()`
supersedes older loads, `dispose()` on unmount kills every outstanding probe, and the probe gates
**failure notifications as well as state writes** — an old folder's error toast is as stale as its
data. Explicit user-action toasts ("Naming failed") still report the action itself.

`loadguard.test.ts` covers supersession, dispose, and an old promise failing both its state write
and its failure toast after the transition. **Honest gap:** there is still no DOM-render test of the
component; what the unit tests cannot pin is React's own remount-on-key contract. This repo has no
jsdom/testing-library harness and adding one was judged disproportionate here — the gap is reported,
not claimed closed.

---

## 7. Remaining product decisions, gates, and known costs

1. **`delete_person` UI exposure** — untouched; product owner's call (unchanged from round 3).
2. **Clear-auto behavior vs. its copy** — untouched; product owner's call.
3. **Cache janitor** — not expanded; recommendation to split it into its own branch stands. (Its
   filename grouping already covers the new chip names — verified, no change needed.)
4. **Runtime gates, still needing the GUI + real library:** `scripts/gate.sh` storm/zoom/peopletest,
   cold open, RSS/app-size, rating-ack latency — note the chip publish transaction now also reads
   the cache directory (stale sweep) while holding the writer lock: small (one readdir per publish)
   but it is on the same lock as rating acks and belongs in the rating-ack measurement.
5. **Upgrade behavior worth knowing:** existing installs' chips are all under legacy names after
   this change; they are served to no one and rebake on first request (panel-open), one decode per
   photo. The `migration` sets `chip_revision = face_revision` so URLs and repair agree from the
   first launch.
6. **Rank bootstrap:** pre-rank generation rows read release 0, so the *first* ranked binary to
   arrive may claim a pre-upgrade DB whatever its models (there is genuinely no ordering information
   in pre-rank rows). For the real library this is the shipped models meeting their own row —
   identical hashes short-circuit to `Ready` with no reindex (tested).
7. **Externally blocked as before:** positive/negative identity fixture pair, golden embeddings,
   DOM component tests (see §6), `CARRY_MARGIN` calibration.

## 8. Migration (schema v6)

`store.rs` bumps `user_version` 5 → 6: tolerated-failure `ALTER TABLE ADD COLUMN` for
`face_scan.chip_revision` (backfilled from `face_revision`) and `face_model_gens.release`
(default 0), so fresh DBs (whose v5 schema already contains both) and existing v5 DBs both land
identical. Verified by `migration_v6_ranks_legacy_generations_and_stamps_chip_identity` (drops the
columns, rewinds to 5, reopens; asserts backfill, rank-0 legacy rows, same-hash `Ready` with **zero**
stale-marking, ranked-release bootstrap) and the updated v4→v6 test.

## 9. Validation (final tree state, this session)

| Command | Result |
|---|---|
| `npx tsc --noEmit` | **pass** |
| `npm test -- --run` | **pass** — 5 files, 37 tests (was 4/33) |
| `cd src-tauri && cargo fmt --check` | **pass** |
| `cd src-tauri && cargo test` | **pass** — 126 passed, 0 failed, 2 ignored (was 114) — including the macOS Trash round-trip, which ran and passed in this session (no sandbox denial to report) |
| `cd src-tauri && cargo clippy --all-targets -- -D warnings` | **pass** |
| `cargo test -- --ignored detects_known_face_and_embeds_deterministically` | **pass** |
| `git diff --check` | **pass** |

`HEAD` is still `e9ee8f0`; nothing committed. The complete diff was re-inspected after the final
format pass; no TODO/FIXME/placeholder was introduced, and no existing test was deleted or weakened
(reworked tests assert strictly more than before).

## 10. Verdict

**Not yet mergeable — but no known code blocker remains open.** Both round-3 blockers are closed by
design changes with regressions that reproduce the reviewed failure paths on production code paths,
and every should-fix and test-discrepancy finding is addressed or honestly reported above.

Minimum remaining before merge:

1. **Another independent review of this pass** — round 3 requires it, and two new load-bearing
   claims deserve adversarial reading: "a chip file's revisioned name is sufficient proof of its
   identity" (§2) and "release rank ordering with the hash-equality short-circuit" (§3).
2. **Runtime gates on the real library** (§7.4), now also covering the publish-time sweep's writer-
   lock cost.
3. **The standing product decisions** (`delete_person` UI, clear-auto copy, janitor split).

The automated suite passing is necessary, not sufficient; this report does not claim more than the
tests demonstrate.
