# Faces feature branch review

- **Specification:** `/Users/cleung/.claude/plans/mutable-finding-blum.md` (Rev 4)
- **Base:** `92283e44cceaa5ae130db57e185a3f7580114af1` (`main`)
- **Reviewed head:** `e9ee8f0ac0d04ffc0e604a04a7963f8ae6c03469`
- **Implementation head:** `2756b447456eaa9f0128346543ce3354ccf0774c` (`e9ee8f0` is a documentation-only follow-up)
- **Method:** report-only review in four passes; no implementation fixes made

## Pass 1 — Plan conformance

Status meanings are those requested in the review brief. “Fully” describes the code present at the reviewed head; acceptance claims that require real photos, a release build, or historical gate output are called out separately as unverifiable.

| Plan item | Status | Evidence / deviation |
|---|---|---|
| Slice 0 packaging: pinned `ort`, bundled YuNet/SFace models and licences, fixed-shape YuNet strategy | Implemented fully | `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json:29`, `src-tauri/src/facedet.rs:8`, `src-tauri/src/faces.rs:28` |
| Slice 0 measurement: inference-active storm, flip p99, cold open, RSS/app-size and rating-ack review | Implemented partially | The harness proves inference activity and records flip/ack/RSS data (`scripts/gate.sh:48`, `src/lib/devharness.ts:263`), but the gate only fails on flip p99/misses; no committed result establishes the plan's cold-open, app-size, rating-ack, or user-review acceptance. Runtime history cannot be verified from this worktree. |
| Schema v5 (`persons`, gens, state, faces, rejections, scan/revision/status) | Implemented fully | `src-tauri/src/facestore.rs:25`, migration from `src-tauri/src/store.rs:194`; vestigial `hidden` matches the plan's post-implementation note. |
| Migration tests: fresh **and existing** DB | Implemented partially | `migration_v5_fresh_and_reopen` only creates a fresh current-schema DB and reopens it (`src-tauri/src/facestore.rs:1661`); there is no v4 fixture upgraded to v5. |
| Embedding compatibility rule: current generation **and `face_scan.status='ok'` in every embedding computation** | Implemented partially | Clustering and sweep candidate discovery apply both filters, but prototype construction and calibration do not join `face_scan` (`src-tauri/src/facestore.rs:421`, `src-tauri/src/facestore.rs:577`), and `sweep_photo` reads embeddings without rechecking scan status (`src-tauri/src/facestore.rs:510`). |
| DB-backed `index_epoch` / `index_enabled`; enabled checked before claim and in commit | Implemented fully | `src-tauri/src/faces.rs:419`, `src-tauri/src/facestore.rs:310`; control rows survive delete-all. |
| `face_revision` protects user edits from scans/sweeps | Implemented fully | User mutation paths bump revisions and both scan and sweep commits compare them (`src-tauri/src/facestore.rs:315`, `src-tauri/src/facestore.rs:554`). |
| Source/model conditional commit: reverify display stat and current model inside final commit | Implemented partially | Model generation is checked in the transaction, but source stat is checked before `commit_scan` (`src-tauri/src/faces.rs:598`) and is not part of the transaction guard (`src-tauri/src/facestore.rs:310`), leaving a check-to-commit race. |
| In-process preview-generation early-discard optimization | Not at all | `PreviewState` still has no generation/token and the face snapshot records no preview generation; only the DB guards and a source stat check are present. |
| Progressive model registration / stale marking | Implemented fully | `ensure_gen` registers hashes, bumps epoch, and stale-marks prior generations (`src-tauri/src/facestore.rs:130`). |
| Unified carry-over for pixel change, model change, and manual rescan; assignments/ignored/rejections transfer; delete then insert | Implemented fully | All three paths converge on `commit_scan`; state and rejection transfer plus delete-before-insert are present (`src-tauri/src/facestore.rs:215`, `src-tauri/src/facestore.rs:320`). |
| Confident correspondence; same-gen IoU+embedding, cross-gen geometry only; ambiguous matches lapse | Implemented partially | Generation rules are correct, but the matcher greedily accepts any threshold-clearing edge and has no ambiguity/margin check (`src-tauri/src/facedet.rs:469`). Its “ambiguous” test only covers two old rows contending for one new row, not one old row with multiple plausible replacements. |
| Confirmed-only prototypes, max five diverse exemplars, threshold+margin, absolute rejections, no cascade | Implemented partially | The core matcher is present, with an extra outlier filter and one-person-per-photo rule; prototype status filtering is missing as noted above. |
| Slice A naming affects selected faces only; Slice B enables embed matching and post-name sweep | Implemented fully | `face_set_name` changes only supplied IDs; the command requests the Slice B sweep (`src-tauri/src/lib.rs:876`). |
| Corrections write durable rejection for displaced manual or auto identity | Implemented fully | Both `face_set_name` and `face_assign` reject displaced identities; explicit assignment retracts a contradictory rejection (`src-tauri/src/facestore.rs:1015`, `src-tauri/src/facestore.rs:1123`). |
| Exact-prior-state, session-scoped undo naming; later edits win | Implemented partially | Assignment state and per-photo revisions are captured, but undo removes operation-created rejection rows even when that photo's revision no longer matches (`src-tauri/src/facestore.rs:1092`), so a later re-creation of the same rejection can be lost. |
| Worker: one thread/connection, preview-only, cursor priority, detect→align→embed, short commit, chips, batched progress | Implemented fully | `src-tauri/src/faces.rs:390` and `src-tauri/src/faces.rs:570`; it never decodes the full-resolution source. |
| Lazy model init, one intra-op thread, session retry/backoff, visible terminal engine error | Implemented fully | `src-tauri/src/faces.rs:481`, `src-tauri/src/faces.rs:509`; status exposes `engineError`. |
| Dedicated, deduplicated chip repair; miss returns 404; one preview decode repairs all missing chips | Implemented fully | `src-tauri/src/faces.rs:298`, `src-tauri/src/protocol.rs:34`; protocol workers only enqueue. |
| Pixel invalidation stale-marks and drops chips; XMP-only rewrite refreshes scan stat; trash rows persist | Implemented fully | `src-tauri/src/lib.rs:96`, `src-tauri/src/xmp.rs:327`; reads exclude trashed rows. |
| Privacy delete: atomic DB disable/epoch/wipe, chips removed, worker parks/drops sessions, durable settings mirror | Implemented partially | DB semantics are present, but the DB transaction commits before the fallible TOML write (`src-tauri/src/lib.rs:1075`). If that write fails, the still-true TOML wins at next launch and re-enables indexing. |
| Live enable/disable plus `[faces]` settings | Implemented fully | Settings are loaded and mirrored; calibrated shipping values differ intentionally from provisional defaults (`src-tauri/src/settings.rs:15`). |
| Planned backend commands | Implemented fully | All listed commands exist and are registered. `set_face_ignored` became a batch `set_faces_ignored`; additional rename/person-face helpers support the specified panel. |
| Person filter AND-combines with stars/recipe/tag; resets on folder open; bounded live refresh; fixed order hint | Implemented fully | `src/lib/session.ts:246`, `src/lib/session.ts:902`, `src/lib/order.ts:46`; pure filter/order tests exist. |
| People panel: progress/error, named/unnamed clusters, naming/undo, correction, delete/toggle, cleanup | Implemented fully | `src/components/PeoplePanel.tsx`; Tauri listener cleanup is present at line 160. |
| Person switcher, `Shift+p`, and `@name` HUD | Implemented fully | `src/components/PersonSwitcher.tsx`, `src/App.tsx:329`, `src/App.tsx:574`. |
| Named-only badges, unnamed affordance, correction/name/ignore menu, post-display per-photo cache | Implemented differently than specified | UX is present, but backend `face_assign` is bypassed: reassignment types a name and calls `face_set_name` (`src/components/FaceBadges.tsx:168`); there is no backend `faces-changed` event, only local `peopleChanged()` plus worker `faces-progress`. |
| Keys and overlay behavior (`p`, `Shift+p`, Escape; panel/switcher join overlay-open state) | Implemented partially | Keys and Escape exist, but `showPeople` is omitted from `overlayOpenRef` (`src/App.tsx:227`), so background culling keys remain active while the panel is open. |
| Slice D merge, ignore, rescan; hide removed per post-note; delete-person/clear-auto acceptance additions | Implemented partially | Merge/ignore/rescan and hide removal are present. `delete_person` has no product UI caller (only the gate harness), while `clear_auto_assignments` is wired but immediately requests the sweep that recreates the cleared guesses (`src-tauri/src/lib.rs:1087`). |
| Documentation amendments | Implemented fully | `SPEC.md`, `docs/METADATA.md`, `README.md`, and `CLAUDE.md` are updated. |
| Rust store/math/protocol/integration test matrix in PLAN | Implemented partially | Good unit coverage exists, but required cases are missing or weaker than specified: existing-v4 migration; source-change commit guard; worker refusing to claim while disabled; rollback under the UNIQUE constraint; actual XMP rating-write path; positive/negative identity fixture pair; golden embedding; repair decode count; real refresh-mid-inference; and several two-process guarantees. |
| TS tests for filter/order, stale event dropping, and per-photo invalidation | Implemented partially | Filter/order tests exist; there are no tests for stale face events or per-photo face-cache invalidation (the only TS test files are `order.test.ts` and `keys.test.ts`). |
| Slice A cache-delete + open-panel + concurrent-storm gate and Slice C live-filter gate | Implemented partially | Slice C has `peopletest`; there is no cache-delete/chip-repair-under-storm phase in `scripts/gate.sh:45`. |

### Out-of-plan implementation / scope drift

- **Should-fix — unrelated cache product feature.** `src-tauri/src/janitor.rs:1` adds a 217-line cache budget/eviction subsystem plus settings, store APIs, startup thread, and documentation. Cache pruning is not in the faces plan and landed between Slice A and B. Suggested remedy: split it into a separately reviewed branch/commit series before merging Faces.
- **Should-fix — singleton-face rescue workflow.** `src/components/PeoplePanel.tsx:583` and its backing `ClustersOut.loose` data path add selection, naming, dismissal, and navigation for faces seen once, whereas the plan limits Unnamed to recurring clusters of at least two. Suggested remedy: either amend/approve the plan explicitly or remove this workflow from the branch.
- **Should-fix — recognition-policy additions.** `src-tauri/src/facedet.rs:356` adds exemplar outlier filtering and `src-tauri/src/facedet.rs:431` enforces one face per person per photo; neither rule appears in the source plan. Suggested remedy: document calibration evidence and add them to the approved design, or keep the planned matcher unchanged.
- **Nit — extra key/UX surface.** `src-tauri/src/keymap.rs:35` adds `Shift+f` for face-badge visibility although the plan reserves only `p` and `Shift+p`. Suggested remedy: record this as an accepted product decision or drop the extra binding.

### Pass-1 findings

- **Blocker — privacy deletion can be undone by a config-write failure.** `src-tauri/src/lib.rs:1075`: `delete_face_data()` commits the DB wipe/disable before writing `settings.toml`; if that write fails, startup sync at `src-tauri/src/lib.rs:1224` treats the old `enabled=true` file as authoritative and starts indexing again. Suggested remedy: make persistence ordering/failure semantics fail closed so a completed privacy delete can never relaunch enabled.
- **Should-fix — stale scans can supply recognition prototypes.** `src-tauri/src/facestore.rs:426`: the prototype query omits the required `face_scan.status='ok'` join; calibration and direct sweep reads have the same pattern. Suggested remedy: centralize the current-generation/scan-ok predicate and use it in every embedding computation.
- **Should-fix — source-change guard has a race.** `src-tauri/src/faces.rs:598`: the final filesystem stat check happens before prototype computation and before the write transaction, while `commit_scan` does not validate source identity. Suggested remedy: carry a source/version token into the conditional commit and validate it at the last possible point, with a regression test for a change in that window.
- **Should-fix — ambiguous carry-over candidates are accepted instead of lapsing.** `src-tauri/src/facedet.rs:469`: every threshold-clearing old/new edge enters a greedy one-to-one match, so one old face with two similarly plausible replacements is assigned to whichever has slightly higher IoU. Suggested remedy: require an explicit confidence margin/mutual-best criterion and test the one-old/two-new ambiguity case.
- **Should-fix — the planned preview-generation early-discard token is absent.** `src-tauri/src/faces.rs:570`: `process_photo` snapshots DB state and source stat but no preview generation, so work against a superseded in-memory/cache preview cannot be discarded before inference as specified. Suggested remedy: add a monotonic per-photo preview generation to `PreviewState`, snapshot it at claim, and check it before inference/commit.
- **Should-fix — undo can delete a later rejection.** `src-tauri/src/facestore.rs:1092`: rejection cleanup is unconditional even for photos whose revision check failed, so a later edit that recreates the same `(face_id, person_id)` row can be erased. Suggested remedy: condition rejection rollback on the same per-photo revision eligibility as assignment restoration.
- **Should-fix — required verification is incomplete.** `src-tauri/src/facestore.rs:1661`, `src-tauri/src/faces.rs:848`, `scripts/gate.sh:45`: multiple explicitly required migration, concurrency, real-model, cache-repair, TS event/cache, and gate cases are absent. Suggested remedy: add the missing tests and attach/retain acceptance outputs before treating the plan as complete.

## Pass 2 — Seam analysis

### Chronology used

There are 22 implementation commits from the pinned base to the reviewed head. I split them after commit 11, `2d8d687` (`feat: exemplar outlier filter`):

- **Early half (1–11):** Slice A backend/frontend and docs, acceptance fixes, the out-of-plan cache janitor, Slice B recognition/calibration, cluster correction work, and exemplar filtering.
- **Late half (12–22):** singleton/“loose” face work, Slice C filter/badges/corrections, Slice D, threshold and recognition-policy reversals, hide removal, and final clippy/protocol cleanup.

The user-described model handoff at roughly 60% is therefore most likely near the loose-face revisions / start of Slice C (`4a1cee7`–`4080c8b`), but Git has no author metadata that identifies the model switch precisely. Findings below are based on the observable chronological seam, not an assumed exact commit.

### Comparison

| Seam risk | Result |
|---|---|
| Duplicate helper/manager/service | No second face store, worker, model engine, repair queue, or state manager appeared. Late Rust work generally extends `facestore` and reuses `commit_scan`, which is the right architectural direction. One functional duplication did appear at the command/UI boundary: the late FaceMenu uses cluster naming for reassignment and leaves the earlier dedicated `face_assign` path unused by the product. |
| Naming/pattern drift | Rust naming remains mostly consistent. The notable drift is event/mutation semantics: early code has distinct `face_set_name` (bulk naming + undo + sweep) and `face_assign` (single correction), while late UI funnels both through `face_set_name`. The final docs also retain the late, reverted `detect at 0.5` value while settings/code returned to 0.8. |
| Architecture bypass | Late frontend code refreshes local state correctly, but bypasses the single-face correction command and its narrower semantics. Late `clear_auto_assignments` also invokes the global sweep at the command boundary, defeating the advertised clear operation under unchanged settings. Core DB conditional-commit/carry-over architecture was otherwise respected. |
| Test-density change | The early half established 42 Rust tests across the four face core files. The late half added roughly 1,464 net implementation lines (including 638 frontend lines in the new/changed face surfaces) but only three non-diagnostic Rust tests and six pure order/filter TS cases. No tests cover the late FaceBadges, PeoplePanel, event invalidation, overlay, or command wiring. This is materially lower coverage density exactly where the seam occurs. |

### Pass-2 findings

- **Should-fix — “clear auto-labels” immediately recreates the labels.** `src-tauri/src/lib.rs:1094`: after clearing every auto assignment, the command unconditionally calls `faces::request_sweep()`. With unchanged prototypes/thresholds—the normal button-click case—the same faces are eligible for the same assignments as soon as the worker idles. Suggested remedy: make clear remain clear, or require an explicit separately named re-evaluate action after settings/prototypes change.
- **Should-fix — late FaceMenu bypasses the earlier correction API.** `src/components/FaceBadges.tsx:168`: typing an existing person's name calls bulk `faceSetName`, which creates an undo-registry entry and requests a folder-wide sweep, while the purpose-built `faceAssign` wrapper at `src/lib/ipc.ts:261` has no production caller. Suggested remedy: resolve existing autocomplete choices to `face_assign` and reserve `face_set_name` for creating/naming clusters, or remove the duplicate semantics and document one authoritative mutation path.
- **Should-fix — post-seam behavior lacks proportionate tests.** `src/components/FaceBadges.tsx:1`, `src/components/PeoplePanel.tsx:583`, `src/lib/session.ts:153`: no component/session tests exercise late on-photo corrections, stale event dropping, per-photo cache invalidation, singleton selection, panel overlay behavior, or clear-auto behavior. Suggested remedy: add focused tests around the late command/UI paths at least to the density of the early store/math work.
- **Should-fix — late threshold reversal was not propagated to the product spec.** `SPEC.md:121` still says detection ships at 0.5, while `src-tauri/src/settings.rs:41` and the plan ship 0.8; `docs/FACES_DEVIATIONS.md:57` also confirms 0.8. Suggested remedy: make the accepted threshold consistent in every user/developer-facing source.

`docs/FACES_DEVIATIONS.md` was added in the final docs-only commit, `e9ee8f0`. It explicitly records most scope additions as user-driven decisions, which is evidence against accidental context loss, but it does not change PLAN conformance because the review brief names PLAN as the source of truth.

## Pass 3 — Integration gaps

### End-to-end traces

| Feature marked complete | Entry → effect | Result |
|---|---|---|
| Automatic indexing | App setup (`lib.rs`) → face worker → preview decode / ONNX → `commit_scan` → SQLite faces/scan rows → `faces-progress` → session/panel refresh | Wired |
| Chip serving and self-repair | `<FaceChip>` URL → `photo://face` protocol → cache read or 404+enqueue → dedicated repair thread → preview decode → atomic chip write → image retry | Wired |
| Cluster/manual naming | PeoplePanel input → `faceSetName` IPC → command → store transaction + undo registry → sweep request → panel/session reload | Wired |
| Naming undo | Toast → `undoNaming(opId)` → session registry → revision-aware DB restoration → reload | Wired, subject to the rejection rollback bug in Pass 1 |
| Auto-recognition | Scan-time proposals and post-name sweep → confirmed prototypes → threshold/margin/rejection match → conditional DB update → progress event → badges/filter membership | Wired, subject to scan-status filtering gaps |
| Corrections and dismissal | Badge/panel menu → reject/name/ignore IPC → DB mutation + face revision → `session.peopleChanged()` → badge/person-map/panel refresh | Wired |
| Person filter | `Shift+p` → PersonSwitcher → `setPersonFilter` → folder `person_map` fetch → fourth AND predicate in `rebuild` → HUD chip | Wired |
| Named badges / unnamed reveal | `afterShow` → per-photo `faces_for_photo` cache → fit-mode rect mapping → FaceBadges → correction/name actions | Wired |
| Rename and merge | Named row → inline rename → collision DTO → merge prompt → `merge_persons` transaction → reload | Wired |
| Privacy delete/toggle | Panel controls → IPC → DB enabled/epoch/delete transaction → chip cleanup/config mirror → worker observes DB and parks/restarts | Wired, but delete durability is not fail-closed (Pass 1 blocker) |
| Rescan | Panel button → `rescan_faces` → folder scans stale + chips deleted → normal worker/carry-over path → new chips/events | Wired |
| Calibration report | Panel button → current-gen calibration query → JSON file in app data → notification | Wired |
| XMP-only stat refresh | Rating/tag queue success → fresh JPEG metadata stat → preview and `face_scan` stat updates → worker sees no pixel change | Wired |
| Clear auto assignments | Panel button → DB clears `assigned_by='auto'` → command immediately requests sweep | Wired but behaviorally self-defeating under unchanged inputs |
| Delete person | Registered command/store method → only `EMBER_PEOPLETEST` cleanup calls the frontend wrapper | Backend/harness only; no product UI path |
| Direct `face_assign` correction | Registered command/store method and IPC wrapper | Dead in the product; no frontend caller |
| Per-photo/terminal error notice | Worker records `face_scan.status='error'` or sets `ENGINE_ERROR` | Partially wired; no event refreshes a panel that is already open |

No TODO/FIXME markers, `todo!`/`unimplemented!`, placeholder returns, fatal errors, or commented-out implementation blocks were found in the changed files. The ignored model tests are intentional integration tests, not stubs.

### Pass-3 findings

- **Should-fix — worker failures can remain invisible in an open panel.** `src-tauri/src/faces.rs:534`: the error branch records the failure but does not add the photo to the progress batch or emit an error/status event; terminal model-registration/init failures at lines 503–514 also return without an event. `PeoplePanel` only reloads on its existing listener (`src/components/PeoplePanel.tsx:148`) or local edits, so a last/only failure can leave “Scanning…” stale indefinitely. Suggested remedy: emit a status-change event for per-photo and terminal failures and consume it through the same debounced refresh path.
- **Should-fix — `delete_person` is not exposed to the user.** `src/lib/ipc.ts:336`: the wrapper is only imported by `devharness.ts`; the panel has no delete-person action despite the post-implementation rationale calling it the answer to an accidentally created person. Suggested remedy: wire a confirmed delete action from the named-person UI or explicitly declare the command harness-only.
- **Should-fix — direct assignment is registered but dead.** `src/lib/ipc.ts:261`: `faceAssign` has no production caller, while the FaceMenu duplicates the operation through bulk naming. Suggested remedy: wire the existing command for existing-person reassignment or remove the dead command/wrapper after consolidating semantics.
- **Should-fix — People panel does not suppress background culling keys.** `src/App.tsx:227`: `overlayOpenRef` includes the person switcher but not `showPeople`, contrary to the plan; arrow/rating/trash keys still affect the photo behind the open panel whenever focus is not in an input. Suggested remedy: include the panel in the overlay-open gate while retaining its Escape close behavior.
- **Should-fix — stale panel requests can overwrite a newly opened folder.** `src/components/PeoplePanel.tsx:130`: three unguarded asynchronous loads write state directly; when `folderId` changes, slower results from the old folder can land after the new requests. The event path checks folder IDs, but the request path does not. Suggested remedy: guard each load with a generation/current-folder check or cancellation before committing state.

## Pass 4 — Standard review

### Correctness and concurrency

The inference, preview decode, chip repair, and SQLite work all run off the React/WebKit thread; I found no obvious main-thread violation in the Rust wiring. The more important concurrency defects are at the DB/filesystem boundary and across app processes:

- **Blocker — in-flight writers can recreate biometric cache files after privacy deletion returns.** `src-tauri/src/faces.rs:656`: a worker commits before baking chips, and repair similarly snapshots DB rows before writing at `src-tauri/src/faces.rs:349`; `delete_face_data` wipes then performs a one-time filename sweep, but neither writer rechecks enabled/epoch nor drains before/after that sweep. `src-tauri/src/preview.rs:385` also publishes through a temp file that `delete_all_face_chips` does not match. Suggested remedy: coordinate deletion with both chip-writing paths (cancel/drain plus a final enabled/epoch guard and temp cleanup) and add a deterministic delete-during-bake/repair test.
- **Should-fix — undoing a name can erase auto-labels that existed before the operation.** `src-tauri/src/facestore.rs:1101`: undo clears *every* auto assignment to `op.person_id`; that assumption is only valid for a newly created person, not when naming into an existing person that already had auto matches. Suggested remedy: capture and restore only auto assignments created by the operation/sweep, or limit blanket cleanup to a newly created person.
- **Should-fix — naming stale face IDs can create a persistent empty person while reporting success.** `src-tauri/src/facestore.rs:980`: the person is created before face IDs are resolved, and missing IDs are silently skipped at line 1011; a concurrent rescan can therefore turn a valid UI action into `affected_face_ids=[]` plus a ghost person. Suggested remedy: resolve/lock at least one live face before creating the person and return an explicit stale-selection error when none remain.
- **Should-fix — an older-model process can infer and discard the same queue forever after another process registers a generation.** `src-tauri/src/faces.rs:458`: the worker keeps its startup `gen`; after `commit_scan` rejects it against `current_gen`, the row remains stale/different-gen and is immediately claimable again, while the discard path at line 529 never reloads the engine/generation or backs off. Suggested remedy: detect current-generation/epoch drift before claiming or after a discard, then rebuild/re-register the worker session instead of hot-looping stale work.
- **Should-fix — a self-merge destructively deletes the person.** `src-tauri/src/facestore.rs:1258`: there is no `source_id != target_id` guard, so a malformed/direct IPC call reaches the final person deletion after no-op reassignment. Suggested remedy: reject identical IDs at the command/store boundary and cover the case with a rollback test.

### Error handling and performance boundaries

- **Should-fix — person-filter IPC failure is presented as a valid empty result.** `src/lib/session.ts:434`: failure is caught as `{}`, after which enabling the filter hides every photo; live refresh and person-list failures are also swallowed at lines 188 and 443. Suggested remedy: preserve the previous membership, surface a non-blocking error, and only apply/refresh the filter after a successful response.
- **Should-fix — fit-only face badges still trigger React renders for every zoom/pan frame.** `src/components/FaceBadges.tsx:26`: the component subscribes to every viewer change and increments state, while gesture drawing calls `notifyChange()` at 60–120 Hz (`src/lib/viewer.ts:89`); the badges may return no boxes while zoomed, but React work still occurs. Suggested remedy: expose/subscribe to discrete fit/resize/flip changes only, and validate the inference-active gesture gate after the change. The actual frame-time impact could not be quantified without the runtime gate fixture.
- **Should-fix — the out-of-plan janitor can evict previews belonging to the open folder's trash panel.** `src-tauri/src/janitor.rs:110`: protection uses only `PreviewState.current_ids()`, which excludes trashed photos, although `TrashPanel` serves their otherwise-unrecoverable cached previews directly (`src/App.tsx:201`, `src-tauri/src/protocol.rs:65`). Suggested remedy: protect every photo ID for the active folder, including trashed rows, or provide a regeneration/source path before eviction.

### Tests and maintainability

The test-density regression at the authorship seam and the missing required matrix are reported in Passes 1 and 2. In particular, none of the blocker races above has a concurrency test, and the late React/session paths have no component-level coverage. I found no systematic style issue worth elevating to a nit: naming and core module boundaries are generally consistent, and the main maintainability concern is behavioral duplication at the correction command boundary already reported in Pass 2.

### Verification

- **Passed:** `tsc --noEmit`.
- **Passed:** `npm test` — 2 files, 23 tests.
- **Passed:** `cargo fmt --check`.
- **Passed:** `cargo clippy --all-targets -- -D warnings`.
- **Passed:** `cargo test` — all 80 non-ignored library tests. The macOS Trash round-trip was the only sandbox failure (permission denied by the managed sandbox) and passed when rerun with Trash access.
- **Passed:** ignored real-model fixture `faces::tests::detects_known_face_and_embeds_deterministically`.
- **Not verified:** `scripts/gate.sh` zoom/storm/storm-faces/peopletest phases, cold-open/RSS/app-size acceptance, and subjective real-library review require a supplied photo folder and running GUI environment. No claim is made from their presence in the script.

## Verdict

**Not mergeable.** The minimum code blockers are: (1) make privacy deletion fail closed when settings persistence fails, and (2) prevent or clean up every in-flight chip bake/repair write so no biometric cache file can appear after deletion. After those fixes, the missing plan-mandated tests and runtime acceptance gates still need to pass before the branch can be considered conformant; the should-fix items above remain material correctness/integration risks rather than cosmetic cleanup.
