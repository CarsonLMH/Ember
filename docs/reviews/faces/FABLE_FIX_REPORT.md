# Faces repair pass — response to `REVIEW_FOLLOWUP.md`

- **Date:** 2026-08-11
- **Base commit:** `e9ee8f0ac0d04ffc0e604a04a7963f8ae6c03469` (`faces`), unchanged — nothing committed
- **Inputs:** private implementation plan (Rev 4; not published), `REVIEW.md`, `REVIEW_FOLLOWUP.md`,
  `docs/FACES_DEVIATIONS.md`, `SPEC.md`, `CLAUDE.md`
- **Scope:** repaired the previous session's uncommitted working tree in place; no resets, no rewrites
  of unrelated work, no edits to either review file

---

## 1. Finding-by-finding

### Blockers

| # | Finding | Status |
|---|---|---|
| B1 | Settings mirror can be made stale-but-trusted by another process | **Fixed** — design replaced |
| B2 | A face-chip temp can be created after deletion's final sweep | **Fixed** — design replaced |

### Should-fix (correctness / concurrency)

| Finding | Status |
|---|---|
| Chip publication not tied to the exact successful scan | **Fixed** |
| Ignored rename failures in `publish_chips` | **Fixed** |
| Exact-state undo breaks across re-detection | **Fixed** |
| A newly started older-model worker can displace a newer generation | **Fixed** |
| Per-photo terminal failures leave the panel permanently "Scanning" | **Fixed** |
| Preview generation zero-value collision + missing invalidation bump | **Fixed** |
| Janitor misses an active folder containing only trashed photos | **Fixed** |
| Folder-safe async frontend handling is partial | **Fixed** |
| XMP acceptance test does not cover the real queue/worker race | **Fixed** (test **and** the underlying coordination) |

### Test findings

| Finding | Status |
|---|---|
| `invalidates per photo` never caches then revisits | **Fixed** |
| `accepts the folderless event` is vacuous | **Fixed** |
| Privacy tests simulate seams sequentially | **Fixed** — real threads + barriers |
| Repair decode-count coverage feasible, not blocked | **Fixed** |
| Worker-refuses-claim-while-disabled feasible | **Fixed** |
| Component-level coverage described as blocked | **Partially fixed** — predicates extracted and unit-tested; no DOM-render harness added (see §3) |
| Rollback test proves FK, not the plan's UNIQUE case | **Fixed** |
| GUI / photo-library gates, positive-negative identity fixture | **Not fixed — externally blocked** (see §3) |

### Product decisions

| Item | Status |
|---|---|
| Existing-person correction path | **Fixed** — routed to `face_assign` as the follow-up specified |
| `delete_person` UI exposure | **Not fixed — needs the product owner** |
| Clear-auto behavior vs. its UI copy | **Not fixed — needs the product owner** |
| Cache-janitor branch scope | **Reported** (§3); its active-folder defect is fixed |

---

## 2. What changed, and why the old interleavings are impossible

### B1 — Settings mirror: two-phase intent, serialized by the SQLite writer lock

**Files:** `src-tauri/src/facestore.rs` (`begin_enabled_intent`, `commit_enabled_intent`,
`set_faces_enabled`, `delete_face_data`, `sync_faces_enabled_from_settings`, `mirror_now`,
`MirrorOutcome`, new `face_state` row `wipe_seq`), `src-tauri/src/lib.rs`
(`delete_face_data`, `set_faces_enabled`, startup sync).

**The old design and why a boolean could not work.** `enabled_mirror_ok` was set to 0 by the DB
transaction, the file was written afterwards, and `mark_settings_mirrored()` then set the flag to 1
*unconditionally*. Two separate steps with no ordering between processes: a writer that paused
before its file write could resume after a later delete, write its stale value, and re-mark the
mirror trusted. A CAS on the *marking* alone would not have fixed it either — the damage is the
stale **file write**, which happens before any marking.

**The new protocol.**

- **Phase 1** (`begin_enabled_intent`) commits `enabled_mirror_ok = 0` in its own transaction and
  returns the current `wipe_seq`. From this instant, any crash, kill, or failure leaves the mirror
  durably untrusted — the DB decides at launch.
- **Phase 2** (`commit_enabled_intent`) opens one `BEGIN IMMEDIATE` and, inside it, applies the DB
  change (and, for delete-all, the wipe) **and writes `settings.toml`** **and** sets
  `enabled_mirror_ok = 1` only if that write succeeded. `BEGIN IMMEDIATE` takes SQLite's single
  cross-process writer lock before the block and holds it until COMMIT.
- **Launch** (`sync_faces_enabled_from_settings`) reads the file **inside** its own transaction, so
  it cannot adopt a value another process has already superseded.

**Ordering argument.**

1. Every app-driven enabled change performs its file write and its DB write inside one writer-lock
   critical section. WAL allows exactly one writer at a time across processes, so these sections are
   **totally ordered**. There is no "finish the file later" step for a stale writer to perform.
2. Therefore the last section to run leaves the file and the DB in agreement, and
   `enabled_mirror_ok = 1` is only ever set by a section that just made them agree.
3. The only remaining gap is *between* a writer's phase 1 and phase 2 (no I/O, two lock
   acquisitions). A privacy delete landing there bumps `wipe_seq`; phase 2 compares it, and an
   **enable** whose intent predates that delete refuses (`superseded`) rather than applying. Disable
   and delete never need to yield — they cannot resurrect anything.
4. A superseded writer mirrors **the DB value it found**, not its own intent, so it cannot leave a
   divergent file behind a trusted flag if someone legitimately re-enabled after the delete.
5. If the file write fails at any point, phase 1's committed `enabled_mirror_ok = 0` stands, and the
   next launch rewrites the file from the DB instead of adopting it.

**Tests** (`facestore.rs`, two `Store`s + independent connections on one DB file, a shared
`settings.toml` stand-in recording what each writer wrote):

- `a_stale_enable_cannot_survive_a_privacy_delete_that_lands_first` — the exact interleaving from
  the follow-up: A's phase 1, then B's *entire* delete on another connection (on its own thread),
  then A's phase 2. Asserts A yields, the file is not left saying `enabled = true`, and a fresh
  `Store` doing the real launch sync stays disabled with nothing scanned.
- `a_stale_writer_that_cannot_write_the_file_leaves_the_mirror_untrusted` — the same ordering with
  both file writes failing; the relaunch still refuses the stale `enabled = true`.
- `a_superseded_writer_mirrors_the_db_it_found_not_its_own_intent` — delete, then a legitimate
  re-enable, then the parked writer completes; file and DB agree.
- `delete_all_survives_a_failed_settings_mirror` — rewritten to drive a genuinely failing mirror
  closure instead of "never call the marker".
- `a_missing_mirror_row_defaults_to_trusting_the_file` — kept (upgrade path).

### B2 — Chips: encoded bytes stay in memory until a serialized publish

**Files:** `src-tauri/src/faces.rs` (`encode_chips`, `bake_chips`, `repair_photo_inner`,
`purge_chips`; `stage_chips`/`discard_staged` removed), `src-tauri/src/preview.rs` (`encode_jpeg`,
`publish_bytes`, `write_jpeg`, `delete_all_face_chips`), `src-tauri/src/facestore.rs`
(`publish_chips`, `PublishOutcome`, `chip_sweep_pending`), `src-tauri/src/lib.rs`.

**What was wrong.** Serializing only the final rename left JPEG *staging* outside the lock, so a
writer paused before staging could create `{id}-fN.tmp…` after the delete's sweep, and death there
left it forever. The sweep also returned `()` and ignored both `read_dir` and `remove_file` failures.

**The new design.**

- `encode_chips` produces `Vec<(PathBuf, Vec<u8>)>` — nothing touches the filesystem.
- `facestore::publish_chips` is the **only** code path that may write a face crop. It opens
  `BEGIN IMMEDIATE`, checks the guards, and only then writes each chip (unique temp + rename via
  `preview::publish_bytes`), still holding the lock.
- Cleanup (`purge_chips` → `delete_all_face_chips`) runs after the wipe commits, is fallible, and
  the wipe sets `face_state['chip_sweep_pending']`, cleared only by a sweep that removed everything
  and retried at the next launch (`lib.rs` startup).
- `delete_face_data` reports a sweep failure as an error with recovery instructions rather than
  returning `Ok(())`.

**Why each listed case is now impossible.**

- *Writer paused before disk staging* — there is no disk staging. It holds bytes; when it resumes,
  the guards refuse and it writes nothing.
- *Another process* — the guarantee is the SQLite writer lock plus committed DB state, both shared
  across processes; nothing here is process-local.
- *Failure or death between staging and publication* — that interval has no filesystem state at all.
  (Death *inside* the publish section can leave a temp, but that section is mutually exclusive with
  the wipe, so it necessarily precedes the wipe's commit and hence its sweep — and temps match
  `is_face_chip_file`, so the sweep takes them.)
- *Enumeration / removal failure* — surfaced as an error; the delete does not report success and the
  DB keeps the cleanup owed.

**Tests** (`faces.rs`, real threads + `crossbeam` rendezvous, no sleeps):

- `a_repair_parked_before_publication_cannot_outrun_a_delete` — a repair is parked on its own thread
  at exactly "decoded and encoded, not yet published". While parked, the test asserts the cache is
  chip- and temp-free, runs the **whole** delete (wipe + sweep) to completion, asserts again, then
  releases the writer and asserts a third time.
- `a_bake_parked_before_publication_cannot_outrun_a_delete` — same seam for the index worker's bake,
  driven from the second connection.
- `a_writer_that_dies_before_publication_leaves_no_biometric_file` — the parked thread panics at the
  barrier; nothing on disk before or after.
- `a_failed_chip_sweep_keeps_the_cleanup_owed` — a directory placed where a chip file belongs makes
  `remove_file` fail on any platform as any user; the sweep errors, `chip_sweep_pending` stays set,
  and only a clean sweep clears it.
- `preview.rs::a_chip_that_cannot_be_removed_fails_the_sweep` — the same at the `PreviewState` level,
  including that removable chips still go and the count is reported.
- `chips_bake_normally_and_the_delete_sweeps_them` — kept; now asserts the sweep count (chips **and**
  a leftover temp).

### Chip publication tied to the exact successful scan (+ rename failures)

**Files:** `facestore.rs` (`commit_scan`, `CommitOutcome::Committed(revision)`, `publish_chips`,
`face_chip_source`, `ChipSource`), `faces.rs` (`BakeTarget`, `repair_photo_inner`).

`face_revision` is generalized from "a human edited this photo" to "**which set of face rows this
photo has**": every committed scan bumps it and `commit_scan` returns it. Publication requires
`face_scan.status = 'ok'` **at exactly that revision**, plus the epoch. Because the commit guard
already compares the snapshot revision, one detection per revision is the rule — a second worker
that snapshotted the same revision cannot commit — so the value is an exact scan identity, not a
heuristic. `publish_chips` now returns `Result<PublishOutcome, String>`; a failed write is reported
and the chips that already landed are rolled back. The repair queue reads epoch + revision + rects
as one snapshot (`face_chip_source`) instead of three unrelated statements.

**Tests:** `faces.rs::a_bake_from_a_superseded_scan_refuses_to_publish` (pixel invalidation with an
unchanged epoch, then a second successful scan in the same epoch, then the correct bake succeeding);
`facestore.rs::a_failed_chip_write_is_reported_and_rolled_back`.

### Undo across re-detection

**Files:** `facestore.rs` (`NamingOp.prior_auto_revisions`, `face_set_name`, `undo_naming`).

The revision bump above makes a re-detected photo **ineligible**, so undo no longer "succeeds" by
updating zero rows — and cannot write into a rowid SQLite reused for a replacement row (the new test
demonstrates that reuse actually happens). The assignment restore additionally matches `photo_id`.
Pre-existing auto labels now carry the revision of the photo that held them, so a carried-over auto
with a new id is left alone instead of being cleared as "created by this naming".

**Tests:** `undo_is_ineligible_for_a_photo_re_detected_since_the_naming`,
`undo_keeps_a_pre_existing_auto_that_was_re_detected`. Existing undo tests unchanged and still pass.

### Older-model worker can no longer displace a newer generation

**Files:** `facestore.rs` (`claim_state`, `register_gen`, `gen_of_models`, `ClaimState`;
`ensure_gen` is now a `#[cfg(test)]` shorthand), `faces.rs` (`model_shas`, worker loop;
`gen_drift`/`GenDrift` removed, `FaceEngine` no longer hashes or registers).

Ownership is decided from the **model file hashes**, computed before any job is claimed and before
any registration — so a freshly started old binary has exactly the information a long-running one
has. `claim_state` returns `Park` when our triple is registered but is not the newest generation,
and `Unregistered` (→ may register) only when it has never been registered, i.e. a genuine upgrade.
`register_gen` re-checks all of it under the writer lock, so a racing old binary still gets `Park`,
and a disabled library cannot be registered into at all. The steady-state check is two indexed
SELECTs with no writer lock, so it does not compete with rating acks.

**Test:** `faces.rs::the_claim_gate_parks_an_older_worker_and_refuses_a_disabled_library` — two
connections on one DB: fresh register, upgrade by the newer binary, then the older one parking and
failing to take ownership back even when `register_gen` is called directly; plus `Disabled` for both
workers while indexing is off, and resumption after re-enable. This replaces the pure `gen_drift`
decision test.

### Panel no longer scans forever after a terminal failure

**Files:** `facestore.rs` (`FaceScanStatus.pending`), `lib.rs`, `src/lib/ipc.ts` (`isScanning`),
`PeoplePanel.tsx`, `devharness.ts`.

`pending` counts photos that are unscanned or stale. A photo parked in `error` is finished, not
pending, so `isScanning` goes false once retries are exhausted; the done line now reads
"N of M photos indexed (E failed)".

**Tests:** `facestore.rs::scan_status_separates_pending_work_from_terminal_failures` (including the
all-photos-fail and last-photo-fails cases and a rescan restoring pending);
`src/lib/ipc.test.ts::isScanning` (including an explicit assertion that the *old* predicate would
have said "scanning").

### Preview generation

**Files:** `preview.rs` (`next_preview_gen`, `set_entries`, `preview_gen`), `lib.rs` (`scan_folder`).

Every listed photo gets a non-zero token on `set_entries`; 0 now means "not listed" and is a value no
snapshot of a listed photo can ever have held. The untrusted-preview branch of `scan_folder` also
invalidates, matching the pixel-change branch.

**Tests:** `leaving_a_folder_discards_a_never_invalidated_photos_work` (exactly the case named in the
follow-up), plus the updated `preview_generations_move_only_on_invalidation`.

### Janitor active folder

**Files:** `janitor.rs` (`run_once` takes `active_folder: Option<i64>`, `spawn` takes an
`Arc<AtomicI64>`), `lib.rs` (`AppState.active_folder`, written by `scan_folder`).

The folder is passed, not inferred from live ids.
**Test:** `the_open_folders_trashed_photos_are_never_evicted` extended with the all-trashed case
(`protected` empty), which the inference could not survive.

### Frontend folder safety and the correction path

**Files:** `src/lib/session.ts`, `src/components/PeoplePanel.tsx`, `src/components/FaceBadges.tsx`,
`src/lib/ipc.ts`.

- `setPersonFilter`, `loadPersons` and `peopleChanged` all capture the folder and re-check it after
  every await before committing state or caches.
- The People panel blanks its folder-scoped state (`status`, `persons`, `clusters`, expansion,
  selection, exclusions, prompts, toast) on `folderId` change, so it can no longer render — or act
  on — the previous folder's face ids under the new folder id.
- `facesEventApplies` and `isScanning` are shared pure predicates used by both the session and the
  panel, so the two cannot disagree.
- **FaceBadges**: a typed name that resolves (normalized) to an existing person now calls
  `face_assign`; only a genuinely new name calls `face_set_name`. This is the follow-up's stated
  plan-conformant split — no dead command, no unreachable undo entry, no folder-wide sweep for one
  correction.

### XMP rewrite coordination

**Files:** `xmp.rs` (stat refresh moved **before** `xmp_done`), `facestore.rs`
(`xmp_write_pending`), `faces.rs` (`claim_photo`).

The coordination is now visible at job claim: the queue row spans the whole window (exiftool rewrite
→ stat refresh → row removed), and `claim_photo` refuses any photo with a queued write. The whole
per-photo eligibility rule lives in one function the worker actually calls.

**Test:** `xmp.rs::a_rating_write_never_costs_a_face_reindex` — real exiftool, real queue row; asks
`claim_photo` at each interesting instant *in the worker's own order*, including the previously
unexercised window after the rewrite and before the refresh, where the file's mtime has already
moved. Ends with a control assertion that a genuine pixel change **is** claimed, so the guard is not
vacuous. exiftool's absence now fails the test instead of skipping it silently (it is a documented
prerequisite). The two pre-existing exiftool tests keep their old skip behavior — out of scope here.

### Carry-over rollback under UNIQUE

**Test:** `facestore.rs::carry_over_is_delete_then_insert_and_rolls_back_intact` — asserts the
UNIQUE(photo_id, face_index) constraint really rejects a coexisting row (so the ordering is
load-bearing, not cosmetic), then forces a failure **part way through the inserts** (a carried
`person_id` whose person row was deleted after the snapshot, on the *second* face, so the delete and
one insert have already happened inside the transaction) and asserts the photo is restored to
exactly its previous rows with its revision unmoved.

---

## 3. Remaining product decisions and external gates

1. **`delete_person` UI exposure** — still backend + gate-harness only. This is a genuine product
   decision (placement, copy, confirmation) and I did not invent one. Either add a confirmed
   destructive action to the named-person row or document the command as harness-only.
2. **Clear-auto behavior vs. copy** — unchanged. `clear_auto_assignments` still requests the sweep,
   which is documented as intentional in `FACES_DEVIATIONS.md`, but the button/toast still promise a
   persistent clear. Rename the copy or change the behavior — the owner's call.
3. **Cache-janitor branch scope** — my recommendation: **split it**. It is a genuinely useful
   pre-existing-hole fix, but it is unrelated to faces (its own settings key, store APIs, startup
   thread and docs), and it would review far better on its own. I fixed only its correctness defect
   and added the missing test; I did not extend it otherwise.
4. **`facedet::CARRY_MARGIN = 0.10`** — introduced by the previous pass and still an uncalibrated
   policy number. It is conservative (ambiguity lapses to Unnamed), but it should be checked against
   real re-detections on the user's library before it is trusted.
5. **Not verified here (needs a GUI + the real photo folder):** `scripts/gate.sh` zoom / storm /
   storm-faces / peopletest phases, cold-open, RSS, app-size, rating-ack latency **with the new
   per-photo chip publish transaction** (that transaction now holds the writer lock for a few small
   file writes — short, but it is a new writer-lock holder on the same DB as the journal and should
   be measured), and subjective recognition/carry-over quality.
6. **Still absent, genuinely externally blocked:** the positive/negative identity fixture pair
   (needs CC0 photographs of the same and different people) and golden embedding values.
7. **Component-level DOM tests** for `FaceBadges`/`PeoplePanel` are still not present. I extracted
   and unit-tested the decision logic they depend on (`isScanning`, `facesEventApplies`) and added
   session-level coverage, but rendering these components needs a jsdom/testing-library setup this
   repo does not have. That is a real remaining gap, not an external blocker.

---

## 4. Verification

All run from the worktree at the final state of the tree.

| Command | Result |
|---|---|
| `npx tsc --noEmit` | **pass** |
| `npx vitest run` (`npm test`) | **pass** — 4 files, 33 tests (was 3 files, 27) |
| `cd src-tauri && cargo fmt --check` | **pass** |
| `cd src-tauri && cargo clippy --all-targets -- -D warnings` | **pass** |
| `cd src-tauri && cargo test` | **pass** — 114 passed, 0 failed, 2 ignored (was 101/2) |
| `cd src-tauri && cargo test -- --ignored detects_known_face_and_embeds_deterministically` | **pass** |
| `git diff --check` | **pass** |

The macOS Trash round-trip test passed here (this session had Trash access), so all 114 non-ignored
tests are green in one run.

Diff inspection: no `TODO`/`FIXME`, no `todo!`/`unimplemented!`, no placeholder returns, no
commented-out implementation. Removed rather than left dangling: `stage_chips`, `discard_staged`,
`preview::stage_jpeg`, `Store::face_rects`, `Store::face_index_epoch`, `Store::mark_settings_mirrored`,
`faces::gen_drift`/`GenDrift`, and `FaceEngine`'s `det_sha`/`rec_sha` fields (model hashing moved out
of engine init, which also takes a 38MB read off the lazy-init path). `facestore::ensure_gen` survives
only as a `#[cfg(test)]` shorthand over `register_gen`. New error paths that are not merely logged —
chip publish failure, chip sweep failure, settings mirror failure — each have a test.

---

## 5. Verdict

**Not yet mergeable, but for reasons that are no longer code blockers.**

Both privacy blockers are closed by design changes (not by weakened tests), each with a deterministic
two-connection or two-thread regression that reproduces the exact interleaving the follow-up
described, and each with an ordering argument that rests on SQLite's cross-process writer lock rather
than on process-local state. Every should-fix item is addressed, and the weak tests named in the
follow-up now exercise what they claim.

Minimum remaining blockers before merge:

1. **An independent review of this pass** — the follow-up explicitly asks for it, and I have not
   claimed anything the tests do not demonstrate. In particular a reviewer should re-derive the
   phase-1/phase-2 ordering argument and the "chips never touch disk outside `publish_chips`"
   invariant, since both are now load-bearing.
2. **Runtime gates on the real library** — `scripts/gate.sh` storm/peopletest, cold open, and
   especially rating-ack latency with the new chip-publish write transaction. None of it is
   verifiable from this worktree.
3. **The two product decisions** (`delete_person` UI, clear-auto copy) and the **janitor split**
   decision.

I have not committed anything; `HEAD` is still `e9ee8f0`.
