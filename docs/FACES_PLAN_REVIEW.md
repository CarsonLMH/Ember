# Review: Faces feature plan

Reviewed plan: initial private implementation plan (not published)

Publication note: a real person label used in the original private review was
replaced with the neutral placeholder `ExamplePerson`; the technical evidence
and recommendations are unchanged.

## Overall assessment

The feature fits Ember well, and the People panel plus an AND-combinable person filter is the right product shape. The plan is thoughtful and unusually concrete, but it should not be implemented unchanged. Several lifecycle and matching decisions could undermine Ember's two highest priorities: culling performance and trustworthy state.

## Must resolve before implementation

### 1. "Not ExamplePerson" is not durable

The proposed correction sets `person_id = NULL`, but a later global sweep can assign the same face to `ExamplePerson` again. `ignored` is not equivalent because it suppresses the face entirely.

Add a negative-association table such as:

```sql
CREATE TABLE face_person_rejections (
    face_id INTEGER NOT NULL REFERENCES faces(id) ON DELETE CASCADE,
    person_id INTEGER NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (face_id, person_id)
);
```

Every automatic matcher must exclude rejected face/person pairs. Assigning a face to somebody else should also reject its former automatic identity.

### 2. Automatic matches must not train future automatic matches

If a person's centroid includes automatically assigned faces, one false positive moves the centroid and can cause a recognition cascade. The proposed global sweep makes that particularly dangerous.

Build matching prototypes from user-confirmed faces only. Prefer several representative embeddings per person rather than one global mean, and require both:

- similarity above a calibrated threshold;
- a meaningful margin over the second-best person.

The proposed `0.40` threshold is too consequential to accept from a published benchmark alone. Calibrate it on representative X-T50 photos: different ages, profiles, glasses, lighting, and several known non-matches.

### 3. Cache deletion would permanently break face chips

Ember explicitly promises that its preview cache is safe to delete (`CLAUDE.md`, Data locations). Under the plan, chip images disappear with the cache, but `face_scan` still says the photo is finished, so they are never regenerated.

Keep face rows and scan completion in SQLite, but add a cheap chip-repair path: when a chip is missing and the preview exists, regenerate it from the stored rectangle without rerunning ONNX. This should also repair partial writes after a crash.

### 4. The worker is not actually "lowest priority"

A dedicated CPU inference thread still competes with six preview workers, protocol reads, WebKit, and synchronous SQLite work. Ember also has one mutex-protected SQLite connection (`Store { conn: Mutex<Connection> }`). A clustering command or global transactional sweep can delay a rating write, directly conflicting with the project's priority order.

Require:

- lazy model initialization only after the first eligible preview exists;
- pausing inference briefly after cursor or keyboard activity;
- all embedding comparisons and clustering outside the store mutex;
- short face-write transactions only;
- no long global transaction holding Ember's primary connection;
- latency measurements for rating acknowledgement, not only visual flip p99.

Consider a separate SQLite connection for the face worker, while still keeping write transactions short so it does not hold the SQLite writer lock during user actions.

### 5. Refresh and folder switching need a generation guard

`PreviewState::entries_by_distance()` returns a snapshot, while `set_entries()` can replace the active folder independently. A face worker can finish old work after a refresh invalidates the same photo, or emit progress for a folder no longer displayed.

Capture a scan generation plus preview/source fingerprint before inference, and verify both immediately before committing. Frontend events should always contain `folderId` and changed photo IDs and should be ignored when stale.

### 6. Failures must not be recorded as "zero faces"

The plan's "errors -> zero-face scan row" behavior turns transient decode, resource, or inference failures into permanent successful results.

Distinguish:

- completed successfully with zero detections;
- retryable failure with attempt count and backoff;
- terminal model initialization failure.

A terminal failure should produce a visible, non-blocking People-panel error rather than only a log line.

### 7. Rescanning should preserve manual work

Face index is not a stable identity. Deleting and recreating faces during threshold or model changes will discard the only copy of user assignments.

Preserve user-confirmed assignments by matching old and new detections using rectangle IoU plus embedding similarity. Ambiguous cases should return to Unnamed rather than silently inheriting or losing a name.

## Schema and model recommendations

- Add a case-insensitive unique name constraint, preferably using a normalized name column. `ExamplePerson`, `exampleperson`, and `" ExamplePerson "` should not create separate people.
- Constrain `assigned_by` with a `CHECK` constraint.
- Add `ON DELETE CASCADE` from `faces` and `face_scan` to photos, even if Ember does not currently delete photo rows.
- Record the detector model hash, recognizer model hash, and preprocessing version. A schema migration is too coarse to protect against accidentally replacing a bundled model.
- Define whether `hidden` is only a UI property. Hidden people should probably continue being recognized; otherwise hiding someone unexpectedly disables future matching.
- Specify whether named-person counts are library-wide or current-folder counts. The panel likely wants current-folder counts while the person record remains global.
- Pin the exact YuNet asset and checksum, not merely "YuNet." Fixed-input and dynamic-input model exports have different runtime requirements.
- Confirm the supported Mac architectures. `ort` 2.0.0-rc.13 drops Intel macOS prebuilts; that is fine if Ember explicitly supports Apple Silicon only, but should be documented.

## Testing gaps

The proposed self-similarity test is not a preprocessing tripwire. A normalized embedding compared with itself will have cosine similarity approximately 1 even if preprocessing is completely wrong.

Use:

- two different photographs of the same person;
- at least one different-person negative;
- golden detector boxes and landmarks within tolerance;
- an embedding checksum or selected golden values for one fixed aligned crop;
- a test proving "not ExamplePerson" survives subsequent naming, scanning, and global sweeps;
- cache deletion followed by automatic chip repair;
- refresh and folder switching during active inference;
- rating acknowledgement latency while clustering and face writes are active;
- a crash between chip creation and the face-row transaction;
- active person-filter behavior as newly scanned matches arrive.

The performance gate should ensure inference is genuinely active during the measured storm rather than relying only on timing assumptions.

## Product feedback

The People panel is a strong fit. Ubiquitous `face?` HUD badges are less convincing: group photographs could make Ember feel like an annotation tool during its primary culling loop. Show named badges by default and put unnamed-face affordances behind the People panel or an explicit hover/action.

Naming a cluster is potentially a large mutation based on only four sample chips. At minimum, show the cluster size and provide an immediate "undo this naming" action. It does not need to join the verdict journal, but synchronous durability is not a substitute for recoverability when one action can label dozens of faces.

Ship "Delete all face data" and a clear indexing-disabled state in the first usable slice. Embeddings and names are sensitive local data even though they never leave the device.

The frontend also needs explicit cache/event semantics:

- `faces-changed` should identify affected photos so HUD caches can be invalidated.
- If a person filter is active, newly scanned matches should enter the filtered view in bounded batches rather than only after the entire folder completes.
- Folder opening must reset `personFilter` and its map cache explicitly.
- Panel event listeners must clean up their Tauri unlisten functions on unmount.

## Recommended slice order

1. Inference and packaging spike with the exact bundled models, benchmarked during a flip storm.
2. Schema with model provenance, negative associations, and cache repair.
3. Detection and unnamed clusters only.
4. Manual naming and correction plus persistence tests.
5. Conservative auto-recognition after threshold calibration.
6. Person filtering and HUD integration.
7. Merge, hide, rescan, and privacy polish.

This preserves the good architecture in the plan while postponing its riskiest behavior, automatic global assignment, until detection, embeddings, persistence, and performance are proven.

## Parts of the original plan worth keeping

- Preview-space normalized rectangles are the correct coordinate contract because Ember's previews already apply orientation.
- Detection and recognition should remain entirely preview-derived; no full-resolution decode belongs in this path.
- Person labels should remain separate from verdicts and XMP for the initial version.
- Person filtering should compose with stars, recipe, and tags using AND semantics.
- Cursor-prioritized background work, pre-baked chips, protocol path validation, and explicit model-license bundling are sound choices.
- The order-hint fix is valid and should be extracted into a tested pure helper as proposed.
