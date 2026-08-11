# Faces repair pass — response to `REVIEW_ROUND4.md` (round 5)

- **Date:** 2026-08-11
- **Base commit:** `e9ee8f0ac0d04ffc0e604a04a7963f8ae6c03469` (`faces`), unchanged — nothing committed
- **Scope, per the owner's directive:** fix the chip-identity reset; make the three small
  robustness fixes; adopt the legacy owner's rank and **declare concurrent pre-rank binaries
  unsupported** (it is not a product requirement); move the pre-existing XMP completion race and
  the product decisions to follow-up work; final focused pass, then stop. Both privacy protocols
  remain untouched.

## 1. Disposition of `REVIEW_ROUND4.md`

| # | Finding | Status |
|---|---|---|
| 1 | Chip identity reused after privacy deletion | **Fixed** — identity minted from a durable counter; exact regression added |
| 2 | Rank gate incomplete on migrated DBs / pre-rank binaries | **Fixed (adoption)** + **declared unsupported (pre-rank concurrency)** — see below |
| 3 | Migration can record v6 without the `release` column | **Fixed** — verify-then-alter; real failures propagate and block the version bump |
| 4 | Stale sweep silently skips enumeration failures | **Fixed** — publish fails loudly; repair treats "couldn't look" as stale-work-present |
| 5 | `FaceChip` carries an exhausted retry budget across identities | **Fixed** — retry counter and timer reset on `(photoId, faceIndex, revision)` change |
| 6 | `xmp_done` (photo_id, rating) token can delete a newer tags-only edit | **Deferred to follow-up by owner decision** — pre-existing defect, outside the faces branch; marked with a KNOWN FOLLOW-UP comment at `store.rs::xmp_done` describing the fix (per-job monotonic token) |

## 2. Blocker 1 — durable chip identity

`chip_revision` is no longer drawn from the per-photo `face_revision` counter (which dies with its
`face_scan` row in a privacy delete and restarts at 1) but from a new `face_state['scan_seq']`
control row — control rows survive delete-all **by design**; that durability is the existing
privacy mechanism, reused. `commit_scan` increments it inside its guarded transaction and stamps
the result into `face_scan.chip_revision`; filenames, URLs, `ChipRef`, the route, `publish_chips`
and repair are unchanged in shape — the *values* simply can never repeat across a wipe.
`face_revision` keeps its per-photo semantics (undo eligibility, commit guards) untouched.

Why the review's path is now impossible: pre-delete chips wear identities ≤ the counter's value at
the wipe; the counter is not reset by the wipe; every post-re-enable commit mints a strictly larger
value. A survivor of a failed (or crashed) sweep therefore can never share a name — or a URL — with
any later detection, of any photo. Migration seeds the counter at
`MAX(face_scan.face_revision)` so adopted legacy identities are also strictly below every future one.

**Regression (the review's exact lifecycle, on the production route):**
`protocol.rs::a_chip_surviving_a_failed_delete_sweep_never_serves_a_later_scan` — publish scan A;
privacy delete commits with `chip_sweep_pending` left set and the chip file surviving; explicit
re-enable (nothing gates on the pending sweep, unchanged); scan B commits; death before bake. The
test asserts `rev_b != rev_a` (under the old code both were 1 — the test fails against it for the
review's reason), the route 404s B's URL without leaking the survivor's bytes, and repair publishes
B's own crop and finally removes the survivor.

## 3. Blocker 2 — rank adoption, and the support boundary

**Adoption (fixed in code):** when a ranked binary meets an owner row with its *own exact hashes*
at a lower stored rank (the migrated rank-0 legacy row), `claim_state` routes it through
`register_gen` once, which — under the writer lock — promotes the row's `release` in place: same
generation, same embedding space, **no epoch bump, no stale-marking, no reindex**, in-flight
commits unaffected. From then on the DB enforces the equal-rank rule: a different-hash binary at
the same rank parks instead of reading rank 0 as a strict upgrade. The migration test now drives
exactly this: legacy row at rank 0 → matching binary adopts (`Ready`, rank 1, epoch unchanged,
zero stale rows) → unseen different-hash rank-1 binary **parks** (it registered as an upgrade
before this fix) → a genuinely newer rank still upgrades.

**Pre-rank binaries (declared unsupported):** a pre-v6 executable never calls the rank gate at all;
no new-code check can stop a writer that predates it, and a schema-level barrier would be built for
a scenario this product does not have — one user, one installed `.app`, and a gate harness that
spawns the *same* binary. CLAUDE.md now states explicitly: running a pre-rank (schema ≤v5) Ember
binary concurrently with a v6+ one against the same DB is unsupported; the supported two-process
reality is two instances of the same installed build. If that ever becomes a real requirement, the
remedy is the review's DB-enforced barrier, as separate work.

## 4. The three robustness fixes

- **Migration (store.rs):** `add_column_if_absent` checks `pragma_table_info` and skips only a
  *verified* duplicate; any real `ALTER` failure propagates and `user_version` stays 5 for a retry
  — a v6 stamp can no longer be recorded over a swallowed error. The interrupted-rerun case
  (columns present, version still 5) is tested (`migration_v6_rerun_after_interruption_verifies_
  and_completes`).
- **Enumeration failures (facestore.rs / faces.rs):** `publish_chips` now fails loudly when the
  cache dir or an entry cannot be enumerated (NotFound stays benign — nothing can be stale in a
  missing dir, and the writes report their own failure); repair treats an enumeration failure as
  stale-work-present so the guarded publish surfaces the error rather than repair silently deciding
  there is nothing to do. Regression: `an_unenumerable_cache_dir_fails_the_publish` (0o311 dir).
- **FaceChip (PeoplePanel.tsx):** the retry counter and any pending timer now belong to one
  artifact identity — an identity change resets the budget and cancels the old timer, so an
  exhausted predecessor can't starve (or a stray timer advance) its successor. No DOM harness
  exists to test the component lifecycle; this remains part of the acknowledged rendering-coverage
  gap and is on the real-library checklist below.

## 5. Deferred to follow-up (owner's decision)

1. **`xmp_done` exact-job token** (review §6; pre-existing): per-job monotonic token compared at
   completion, plus the two-tags-one-rating barrier test. Marked in code at `store.rs::xmp_done`.
2. **Product decisions:** `delete_person` UI exposure; clear-auto copy vs. behavior; janitor split.

## 6. Validation

| Command | Result |
|---|---|
| `npx tsc --noEmit` | **pass** |
| `npm test -- --run` | **pass** — 5 files, 37 tests |
| `cd src-tauri && cargo fmt --check` | **pass** |
| `cd src-tauri && cargo test` | **pass** — 129 passed, 0 failed, 2 ignored (was 126) |
| `cd src-tauri && cargo clippy --all-targets -- -D warnings` | **pass** |
| `cargo test -- --ignored detects_known_face_and_embeds_deterministically` | **pass** |
| `git diff --check` | **pass** |

`HEAD` is still `e9ee8f0`; nothing committed. The round-5 diff was re-inspected hunk by hunk after
the final format pass.

## 7. What remains before merge

1. **Real-library drive** (the owner's): suggested checklist — delete all face data, re-enable,
   let the folder reindex, and confirm every chip in the panel shows the *current* crop; "rescan
   faces" on a folder and confirm named-person representative chips heal (this also exercises the
   FaceChip identity-reset fix); a rating burst during indexing for rating-ack feel (the publish
   sweep now holds the writer lock for one cache readdir per scanned photo).
2. The deferred follow-ups in §5, as separate work.
3. Freeze and run the release-candidate gates (`scripts/gate.sh` storm/zoom/peopletest, cold open,
   RSS/app-size).

Per the directive, this pass stops here: no further scope beyond the fixes above.

---

## Addendum — post-round-5 migration omission (Codex)

A database stamped v6 by the **round-4** working-tree build predates `scan_seq` and skips the
v5→v6 seeding on reopen; its next commit would restart identities at 1 and could re-mint an
existing chip name. Fixed with the bounded remedy: an **idempotent guard on every `Store::new`**
(after the migration chain) — `INSERT OR IGNORE` of `scan_seq` at `MAX(face_scan.chip_revision)`,
a no-op wherever the row already exists (fresh DBs, v5→v6 migrations). The hunk to inspect is the
tail of the migration section in `store.rs`; the regression is
`facestore.rs::a_v6_database_without_the_counter_is_seeded_above_existing_identities` (reopens a
v6-stamped DB holding identities but no counter row, with a chip identity deliberately far above
`face_revision` to prove the seed reads `chip_revision` itself, and asserts the next commit mints
strictly higher). Suite after the fix: 130 Rust tests, clippy/fmt/`git diff --check` clean; the
TypeScript side is untouched.
