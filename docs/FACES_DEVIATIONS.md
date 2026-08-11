# Faces: what shipped that the plan didn't say

The implementation plan (`~/.claude/plans/mutable-finding-blum.md`, rev 4) was
followed slice by slice, but the product owner drove every slice on real
X-T50 photos and that feedback changed the design. This file records
**everything in the shipped feature that is not in the plan**, so a reviewer
doesn't mistake a deliberate change for drift, and so a later session doesn't
"restore" something that was removed on purpose.

Nothing here weakened the plan's correctness rules: DB-only persistence,
conditional commits, absolute rejections, carry-over on re-detection, and the
privacy semantics are all as specified.

## Added — not in the plan at all

| What | Why it exists |
|---|---|
| **Cache janitor** (`janitor.rs`, `[cache] max_mb`, default 2GB) | The user noticed `~/Library/Caches/com.cleung.ember` at 430MB and growing forever. A pre-existing hole (it predates faces — ~1MB/photo for every folder ever opened, never cleaned). Evicts whole per-photo artifact groups, DB-orphans first then least-recently-opened folders, never the open folder. |
| **`delete_person`** | Not in the plan. Needed for guaranteed gate-harness teardown after test data ("HarnessPerson") leaked into the user's real DB; also the honest answer to "I created this person by mistake". |
| **`clear_auto_assignments(folder)`** → panel *"clear auto-labels"* | Recovery when recognition has made a mess: drops every machine guess, keeps user labels **and** rejections, re-runs the sweep. Added after a bad detection threshold flooded a folder with false matches. |
| **`facedet::one_face_per_person`** | The user found a photo showing "Nati" twice (neither was her). One person cannot be two faces in one photo — now enforced in **both** auto paths (embed-time and sweep): strongest candidate wins, never contests a user or carried assignment. |
| **`facedet::filter_exemplar_outliers`** | A confirmed face that resembles none of that person's other confirmed faces (a named back-of-head shot) is junk — and greedy max-min exemplar selection *loves* outliers. Below the same-identity floor (0.35 max-cosine vs siblings) it can't be a reference. Makes naming junk shots harmless. |
| **Loose-faces section** (faces seen only once) | The plan only surfaced clusters ≥2, so singletons were invisible and unreachable — 56 in one folder. Click-select → name. Deliberately **rescue-only**: no dismiss, because unlabeled is a loose face's resting state (the user's insight). |
| **Per-chip ✕ in clusters + "restore N" + "don't label removed"** | Mixed clusters happen (a stranger bridged into Nati's group by junk detections). ✕ removes a face from the group before naming. |
| **Chip click → jump the viewer to that photo** | The cheapest possible answer to "who *is* this?" — full context, zero new UI. |
| **`Shift+f` toggles face badges** | Requested; persisted in localStorage. |
| **`rescan_faces`** exposed in the panel | Planned as a Slice D command; pulled forward because a detection-setting change is invisible without it (already-'ok' photos are never re-examined). |
| **`stormOk` gate assertion + loaded-keybinding count** | A storm once passed having recorded **zero flips** (`misses=0/0`): the rating callback calls `session.rate()` directly, so acks looked healthy while the keyboard path was dead. Silence must never read as success. |
| **`peopletest` gate phase** (`EMBER_PEOPLETEST=1`) | The plan asked for "active-filter-during-scan" coverage in Slice C; this is that, automated: name a cluster, filter to the person, assert membership exactness, storm inside the filtered view, tear down. |
| **`detection_scores_for_env_image`** (`#[ignore]` diagnostic) | Prints every YuNet detection + score for one image. Built to answer "why was this face missed?" with data instead of a guess. |
| **`session.peopleVersion`** | The People panel only reloaded on its own edits, so a correction made on the photo left its counts stale. |

## Changed from the plan

- **Rename onto an existing name is a merge offer, not an error.** The plan
  had `merge_persons` as a bare Slice D command; the user's real case was a
  typo ("Nai" → "Nati"), so `rename_person` reports the collision and the
  panel offers to merge.
- **Cluster rows expose every member.** The plan said "up to 4 chips +
  explicit total size"; a hidden member can't be excluded before naming, so
  clusters return all faces and the panel previews 8 with "+N more".
- **The unnamed-face affordance names faces in place.** The plan said the
  "N unnamed" chip opens the People panel. After "Not X" un-names a face it
  loses its badge, so the chip now *reveals* dashed boxes you can click to
  name — otherwise a correction was a one-way trip.
- **FaceMenu is correction-only**: *Not X* · *This is someone else…* (name
  input with autocomplete) · *Not a person / don't label*. The plan listed a
  per-person "reassign" list; it grows with the roster, and typing a name
  already reassigns.
- **Naming someone retracts an existing "not them" on that face** (and
  `face_assign` does the same). The plan only specified that corrections
  *write* rejections; without the retraction, correcting a correction left a
  contradiction that silently blocked all future matching.
- **Shipped thresholds** come from calibration on the user's library, as the
  plan required, but the numbers differ from its placeholders: auto-assign
  **0.45** with margin **0.08** (plan: 0.40/0.05), cluster **0.50** (plan:
  0.45), detect **0.8** (plan's value; see below).
- **Dismissal ("not a person / don't label") and `set_faces_ignored` moved
  from Slice D into the working panel** as soon as a museum folder produced
  statue and photographed-photo clusters.

## Behavior a reviewer will flag as off-plan (don't "fix" it)

- **Culling keys stay LIVE while the People panel is open.** The plan said the
  panel should join `overlayOpenRef` and swallow background keys; the owner
  decided otherwise at Slice A acceptance: the panel is a dock beside the
  viewer, like the trash panel, and the whole point is naming faces *while
  culling* — arrows, ratings and trash keep working, panel inputs own their
  own keys, Escape closes. A review round "fixed" this to plan (a
  `panelOpenRef` gate that ate every hotkey) and the owner reported it as a
  bug the same day (2026-08-11) — it was reverted. Only the full-screen
  pickers/palettes (recipes, tags, person switcher) own the keyboard.

## Removed

- **`set_person_hidden` (hide a person).** Built in Slice D, then cut at the
  user's request before acceptance: its only use is decluttering a long
  People list, which never arrives in a two-or-three-person library. It came
  from iOS-Photos parity, not from the culling workflow. The `persons.hidden`
  column remains, unused and documented as vestigial (dropping it would need
  a migration for zero benefit). The plan file carries the same note.

## One regression worth knowing about

`min_det_score` was **0.8 → 0.5 → 0.8**. The drop was made on the strength of
a single photo whose real face scored 0.615. On the user's library that let
hands, ears and temple lettering through as faces, and those junk detections
then auto-matched to the person with the most exemplars (a wider exemplar
spread has a wider catchment). The revert was measured against the user's own
labels instead: all 158 user-confirmed faces score **≥0.816** (p25 0.917),
while 62% of the false auto-labels sat below 0.8. The lesson, recorded in the
`settings.rs` comment: never tune a global threshold from one sample.
