# Follow-up review: Faces plan Revision 2

Reviewed plan: `~/.claude/plans/mutable-finding-blum.md`, Revision 2

## Assessment

Revision 2 is substantially stronger and addresses the original seven must-resolve findings well. In particular, the rejection model, confirmed-only exemplars, cache repair, failure taxonomy, generation guard, assignment-preserving rescan, conservative slice order, and expanded tests are good changes.

The decision not to pause inference by default is reasonable. Proving the performance budget with inference genuinely active is stronger; activity backoff can remain the fallback if Slice 0 fails.

Four issues should still be resolved before implementation.

## 1. Model provenance is not safe during progressive reindexing

The proposed singleton `face_models` row cannot prove which model produced each stored embedding. Reindexing uses short per-photo transactions, so a crash or interruption can leave old- and new-model embeddings in the database simultaneously while the global record implies only one version. Those incompatible embeddings could then be combined in clustering or person prototypes.

Add a model/preprocessing generation or fingerprint to every `faces` row or `face_scan` row. All clustering, exemplar, and matching queries must select only compatible generations.

A safe progressive migration should work like this:

1. Register the new model generation.
2. Keep the old face rows available while each photo is reprocessed.
3. Commit a photo's replacement faces and generation atomically.
4. Exclude stale generations from new matching calculations.
5. Remove old-generation rows only after their replacements are committed or when they are no longer needed for assignment carry-over.

This preserves short transactions without ever silently mixing embedding spaces.

## 2. "Delete all face data" will not stay deleted

If face indexing remains enabled, an in-flight worker can commit rows immediately after deletion. On the next launch, the worker will also index the library again. That conflicts with the privacy meaning of a Delete-all action.

The action should:

- persistently disable face indexing;
- increment a worker/index epoch so in-flight commits fail their final guard;
- delete persons, faces, rejections, scan state, and chips;
- release or discard loaded recognition state where practical;
- clearly explain how the user may enable indexing again.

If the intended behavior is immediate automatic reindexing, label the action **Reset face index** rather than **Delete all face data** and provide a separate privacy deletion action.

## 3. The generation guard is process-local

`PreviewState` generations protect refresh and folder switching within one process. They do not cover another Ember instance because each process has its own generation counter. A generation guard also does not prevent a rescan from overwriting a user correction made after the rescan captured its old face snapshot.

Add a database-backed face revision or indexing epoch. Before committing, a worker should verify that:

- its global indexing epoch is still current;
- the photo's face revision has not changed since its snapshot;
- the source and model generations still match;
- no user edit occurred after the work began.

The final write should use an optimistic conditional update/transaction and discard results if any revision changed.

Also revise the concurrency wording: a separate SQLite connection avoids Ember's in-process `Mutex<Connection>`, but it still competes for SQLite's single writer lock. Short transactions and rating-acknowledgement measurements remain necessary.

## 4. Synchronous chip repair can clog the protocol pool

After cache deletion, several chip requests for one photo may arrive together. If each request independently decodes the 2600px preview, the shared protocol workers can perform redundant expensive work while ordinary preview/original requests wait.

Repair should be bounded and deduplicated:

- use an in-flight repair map keyed by photo ID;
- decode a preview once and regenerate all missing chips for that photo;
- preferably run repair through a separate one-worker queue rather than directly on the normal protocol pool;
- let requesting images receive the existing 404/retry behavior while repair is pending;
- keep normal preview/original protocol work higher priority.

Add an acceptance test that deletes the cache, opens a People panel containing many chips, and runs the flip-storm gate concurrently.

## Smaller clarifications

### Undo naming must restore exact prior state

Returning only `affectedFaceIds` is insufficient. A face might already have belonged to that person, or might have had a different assignment, `assigned_by`, or similarity value.

Capture an undo token containing each changed row's prior assignment state. Undo should restore only rows whose revision still matches the naming operation. If naming created a new person, remove that person on undo only if nothing else now references it.

### Preserve assignments during ordinary invalidation

The lifecycle section says that a changed photo has its assignments reset, while explicit rescan preserves assignments. Since the database is the only persistence for this work, use the same IoU-plus-embedding assignment carry-over for ordinary pixel invalidation when possible. Ambiguous matches should become Unnamed.

### Clarify Slice A versus Slice B behavior

Slice A says no automatic assignment, while the general `face_set_name` description includes a global automatic sweep. State explicitly that Slice A only applies the name to the selected cluster and that the post-naming sweep is enabled in Slice B.

### Define retry state precisely

The plan stores `attempts` in SQLite but describes a retry cap of three per session. Specify whether the persisted value is lifetime diagnostic state while an in-memory counter controls the current session, or whether attempts are reset at startup. Backoff should use an explicit `next_retry_at` or a clearly defined in-memory schedule.

### Reject every identity corrected as wrong

When a user says a named face is not that person, record the rejection regardless of whether the former assignment was automatic or manual. Reassignment or clearing should not allow a later sweep to restore an identity the user explicitly corrected.

## Readiness

After the four main issues are incorporated and the smaller semantics are clarified, the plan is implementation-ready. Slice 0 remains the correct go/no-go starting point.

