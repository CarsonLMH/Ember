//! Face persistence (schema v5) — everything SQL for SPEC §14.
//!
//! Face data is DB-only and deliberately NOT verdicts: nothing here touches
//! the actions journal or Cmd+Z. Durability model (plan rev 4):
//! - `face_state` control rows (`index_epoch`, `index_enabled`) are
//!   DB-authoritative and survive Delete-all — they are what makes deletion
//!   stick across the two processes that share this DB during gate runs.
//! - Every embedding row carries its `model_gen`; embeddings from different
//!   generations never meet in a computation (compatibility rule).
//! - The worker commits through `commit_scan`, an optimistic conditional
//!   write: any epoch/enabled/generation/revision mismatch discards the work.
//! - Reprocessing (pixel change, rescan, model change) flows through ONE
//!   carry-over transaction that transfers assignments, ignored flags and
//!   rejections to confidently-matched replacement faces — without it, the
//!   ON DELETE CASCADE would silently drop every "not X" on rescan.
//!
//! `Store` methods here serve the command layer (mutex connection); the
//! worker calls the free functions on its own connection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;

use crate::facedet;
use crate::store::Store;

pub const SCHEMA_V5: &str = r#"
CREATE TABLE IF NOT EXISTS persons (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    name_norm TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    -- Vestigial: the hide-a-person feature was removed as unused (2026-08).
    -- The column stays because dropping it would need a migration for zero
    -- benefit; nothing reads or writes it.
    hidden INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS face_model_gens (
    gen INTEGER PRIMARY KEY,
    det_sha256 TEXT NOT NULL, rec_sha256 TEXT NOT NULL, prep_version INTEGER NOT NULL,
    -- Monotonic release rank of the bundled model set (faces::MODEL_RELEASE).
    -- Ownership ordering comes from THIS, never from gen/created_at: a
    -- never-before-seen triple is not evidence of an upgrade. Rows from
    -- before the column existed read 0 (any ranked release outranks them).
    release INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS face_state (
    key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS faces (
    id INTEGER PRIMARY KEY,
    photo_id TEXT NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
    face_index INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
    det_score REAL NOT NULL,
    embedding BLOB NOT NULL,
    model_gen INTEGER NOT NULL REFERENCES face_model_gens(gen),
    person_id INTEGER REFERENCES persons(id) ON DELETE SET NULL,
    assigned_by TEXT CHECK (assigned_by IN ('auto','user')),
    similarity REAL,
    ignored INTEGER NOT NULL DEFAULT 0,
    UNIQUE(photo_id, face_index));
CREATE INDEX IF NOT EXISTS idx_faces_photo ON faces(photo_id);
CREATE INDEX IF NOT EXISTS idx_faces_person ON faces(person_id);
CREATE TABLE IF NOT EXISTS face_person_rejections (
    face_id INTEGER NOT NULL REFERENCES faces(id) ON DELETE CASCADE,
    person_id INTEGER NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (face_id, person_id));
CREATE TABLE IF NOT EXISTS face_scan (
    photo_id TEXT PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
    mtime INTEGER NOT NULL, size INTEGER NOT NULL,
    model_gen INTEGER NOT NULL REFERENCES face_model_gens(gen),
    face_count INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ok','error','stale')),
    face_revision INTEGER NOT NULL DEFAULT 0,
    -- Which detection produced the CURRENT rects: set only by commit_scan,
    -- drawn from face_state['scan_seq'] — a control-row counter that, like
    -- every control row, SURVIVES delete-all. Chip files carry it in their
    -- names ({id}-f{n}r{rev}.jpg), so an artifact proves which detection it
    -- belongs to, including across a privacy wipe whose file sweep failed:
    -- a post-delete rescan can never mint an identity an old file already
    -- wears. (face_revision moves on user edits too and resets with its row —
    -- using it here would both rebake chips on every naming and reissue
    -- identities after a wipe.)
    chip_revision INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    scanned_at INTEGER NOT NULL);
INSERT OR IGNORE INTO face_state (key, value)
    VALUES ('index_epoch', 1), ('index_enabled', 1), ('enabled_mirror_ok', 1),
           ('wipe_seq', 0), ('chip_sweep_pending', 0), ('scan_seq', 0);
"#;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- control state (free functions: worker + Store share them) ----------

pub fn state_get(conn: &Connection, key: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT value FROM face_state WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
}

/// Control rows added after a DB was already at v5 are absent, not zero —
/// every read of an optional control row goes through here.
fn state_get_or(conn: &Connection, key: &str, default: i64) -> rusqlite::Result<i64> {
    Ok(state_get(conn, key).optional()?.unwrap_or(default))
}

/// Privacy-delete generation captured before preparing a calibration report.
/// Older v5 databases may not have materialized the row yet, so zero is the
/// backward-compatible value until the first delete bumps it.
pub fn wipe_seq(conn: &Connection) -> rusqlite::Result<i64> {
    state_get_or(conn, "wipe_seq", 0)
}

fn state_set(conn: &Connection, key: &str, value: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO face_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn bump_epoch(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE face_state SET value = value + 1 WHERE key = 'index_epoch'",
        [],
    )?;
    Ok(())
}

/// Increment a control row that may not exist yet (DBs created before the row
/// was added read as `default` and are materialized here).
fn bump_state(conn: &Connection, key: &str, default: i64) -> rusqlite::Result<i64> {
    let next = state_get_or(conn, key, default)? + 1;
    state_set(conn, key, next)?;
    Ok(next)
}

/// What an app-driven enabled change did, including whether settings.toml
/// now agrees with the DB.
#[derive(Debug, PartialEq)]
pub struct MirrorOutcome {
    /// The DB's `index_enabled` actually flipped.
    pub changed: bool,
    /// An enable was abandoned because a privacy delete landed while it was
    /// in flight. Indexing stays off; the delete wins.
    pub superseded: bool,
    /// `None` = settings.toml matches the DB. `Some(err)` = it does not, and
    /// `enabled_mirror_ok` has been left at 0 so the DB decides at launch.
    pub mirror_error: Option<String>,
}

/// Write settings.toml from inside an enabled-intent transaction, and record
/// whether it can be trusted at the next launch. Called with the writer lock
/// held — that is what orders it against the other process.
fn mirror_now(
    conn: &Connection,
    value: bool,
    mirror: &dyn Fn(bool) -> std::io::Result<()>,
) -> rusqlite::Result<Option<String>> {
    match mirror(value) {
        Ok(()) => {
            state_set(conn, "enabled_mirror_ok", 1)?;
            Ok(None)
        }
        // Leave the mirror flag at 0 (phase 1 committed it): the DB wins at
        // launch and the file gets rewritten from it then.
        Err(e) => Ok(Some(e.to_string())),
    }
}

pub fn index_enabled(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(state_get(conn, "index_enabled")? != 0)
}

/// (gen, det_sha256, rec_sha256, prep_version, release) of the owning row.
pub type GenRow = (i64, String, String, i64, i64);

/// The generation that owns the library: the highest-ranked release. `gen`
/// only tie-breaks legacy rows registered before ranks existed (all release
/// 0) — for ranked rows the registration gate keeps releases strictly
/// increasing, so rank alone decides.
pub fn current_gen(conn: &Connection) -> rusqlite::Result<Option<GenRow>> {
    conn.query_row(
        "SELECT gen, det_sha256, rec_sha256, prep_version, release FROM face_model_gens
         ORDER BY release DESC, gen DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .optional()
}

/// What a worker may do right now, decided from the DB alone — the whole
/// job-claim gate in one place so it can be tested against a real second
/// connection instead of being re-derived in the loop.
#[derive(Debug, PartialEq)]
pub enum ClaimState {
    /// `index_enabled = 0`: claim nothing, drop the ONNX sessions.
    Disabled,
    /// A generation that OUTRANKS ours (or equals our rank with different
    /// hashes) owns the DB — an older binary meeting a library another
    /// process already moved forward, or two builds mislabeled with one rank.
    /// Every commit we made would be discarded against `current_gen`, and
    /// re-registering would start a registration war, so this worker parks.
    /// **This is the state that stops an old worker from downgrading a newer
    /// generation — including one this DB has never seen before.**
    Park,
    /// Work under this generation.
    Ready(i64),
    /// `register_gen` has something to do before work can start: no owner,
    /// an owner strictly outranked by our release (a model *upgrade*), or an
    /// owner with OUR exact hashes still carrying a lower (legacy) rank —
    /// which `register_gen` promotes in place, no epoch bump, no reindex, so
    /// the DB can thereafter enforce the equal-rank rule against different
    /// hashes at the same release.
    Unregistered,
}

/// Read-only classification — no writer lock, so the worker can call it every
/// pass without competing with rating acks.
///
/// Ordering comes from `release`, the binary's monotonic model-set rank —
/// never from whether a hash has appeared in this DB before. "Absent from
/// history" proves nothing: a DB first indexed by v2 has never seen v1's
/// hashes, and v1 must still park.
pub fn claim_state(
    conn: &Connection,
    det_sha: &str,
    rec_sha: &str,
    prep_version: i64,
    release: i64,
) -> rusqlite::Result<ClaimState> {
    if !index_enabled(conn)? {
        return Ok(ClaimState::Disabled);
    }
    Ok(match current_gen(conn)? {
        // Identical models are the same embedding space whatever the label
        // says — keep working. An owner still carrying a LOWER rank than the
        // binary that owns those very hashes (a pre-rank legacy row met by
        // its own ranked build) routes through `register_gen` once to be
        // promoted in place: until that happens, a different-hash binary at
        // our rank would read the rank-0 row as a strict upgrade.
        Some((gen, det, rec, prep, owner_release))
            if det == det_sha && rec == rec_sha && prep == prep_version =>
        {
            if owner_release < release {
                ClaimState::Unregistered // adopt the legacy owner into our rank
            } else {
                ClaimState::Ready(gen)
            }
        }
        // A strictly newer release may take ownership. Equal rank with
        // different hashes is a build error, not a race to win: park.
        Some((.., owner_release)) if release > owner_release => ClaimState::Unregistered,
        Some(_) => ClaimState::Park,
        None => ClaimState::Unregistered,
    })
}

/// Register the bundled models as the newest generation (model upgrade). A new
/// generation bumps the epoch — killing in-flight commits from any process —
/// and stale-marks every older-generation scan so those photos re-enter the
/// queue for progressive reprocessing.
///
/// Every condition is re-checked inside the writer lock, so two processes
/// racing here cannot both register, and a process whose release was already
/// outranked between `claim_state` and this call gets `Park` instead —
/// registration order can never override release order.
pub fn register_gen(
    conn: &mut Connection,
    det_sha: &str,
    rec_sha: &str,
    prep_version: i64,
    release: i64,
) -> rusqlite::Result<ClaimState> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !index_enabled(&tx)? {
        return Ok(ClaimState::Disabled); // tx drops → rollback
    }
    if let Some((gen, det, rec, prep, owner_release)) = current_gen(&tx)? {
        if det == det_sha && rec == rec_sha && prep == prep_version {
            if owner_release < release {
                // ADOPTION, not upgrade: a legacy (pre-rank) owner row met by
                // the ranked build of the very same models. Promote it in
                // place so the equal-rank rule holds from here on — same
                // embedding space, so no epoch bump, no stale-marking, no
                // reindex, and in-flight commits keep their validity.
                tx.execute(
                    "UPDATE face_model_gens SET release = ?2 WHERE gen = ?1",
                    params![gen, release],
                )?;
            }
            tx.commit()?;
            return Ok(ClaimState::Ready(gen));
        }
        if release <= owner_release {
            return Ok(ClaimState::Park); // we are the older (or mislabeled) binary
        }
    }
    tx.execute(
        "INSERT INTO face_model_gens (det_sha256, rec_sha256, prep_version, release, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![det_sha, rec_sha, prep_version, release, now_ms()],
    )?;
    let gen = tx.last_insert_rowid();
    bump_epoch(&tx)?;
    tx.execute(
        "UPDATE face_scan SET status = 'stale' WHERE model_gen <> ?1",
        params![gen],
    )?;
    tx.commit()?;
    Ok(ClaimState::Ready(gen))
}

/// Test-only shorthand for "these models own the DB": registers with a rank
/// above every existing row. Production code always goes through
/// `claim_state` with the binary's fixed `MODEL_RELEASE`, so an older binary
/// can never get here.
#[cfg(test)]
pub fn ensure_gen(
    conn: &mut Connection,
    det_sha: &str,
    rec_sha: &str,
    prep_version: i64,
) -> rusqlite::Result<i64> {
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(release), 0) + 1 FROM face_model_gens",
        [],
        |r| r.get(0),
    )?;
    match register_gen(conn, det_sha, rec_sha, prep_version, next)? {
        ClaimState::Ready(g) => Ok(g),
        other => panic!("ensure_gen: {other:?}"),
    }
}

pub fn face_revision(conn: &Connection, photo_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT face_revision FROM face_scan WHERE photo_id = ?1",
        params![photo_id],
        |r| r.get(0),
    )
    .optional()
    .map(|v| v.unwrap_or(0))
}

/// Every user edit touching a photo's faces bumps this — the worker's
/// conditional commit verifies it, so a correction made mid-rescan wins.
///
/// A committed scan bumps it too (see `commit_scan`): the counter identifies
/// *which set of face rows* a photo currently has, not just "a human edited
/// it". That is what lets a pending naming-undo (whose face ids die with the
/// rows) and a chip publish (whose crops belong to one specific detection)
/// both tell "still the state I recorded" from "re-detected since".
fn bump_revision(conn: &Connection, photo_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE face_scan SET face_revision = face_revision + 1 WHERE photo_id = ?1",
        params![photo_id],
    )?;
    Ok(())
}

// ---------- worker snapshot / conditional carry-over commit ----------

#[derive(Debug, Clone)]
pub struct OldFace {
    pub rect: [f32; 4],
    /// None when the row is from another model generation — its embedding
    /// must not be compared with current-generation ones.
    pub embedding: Option<Vec<f32>>,
    pub person_id: Option<i64>,
    pub assigned_by: Option<String>,
    pub similarity: Option<f64>,
    pub ignored: bool,
    pub rejections: Vec<i64>,
}

#[derive(Debug)]
pub struct ScanSnapshot {
    pub epoch: i64,
    pub gen: i64,
    pub revision: i64,
    pub old: Vec<OldFace>,
}

pub struct NewFace {
    pub rect: [f32; 4],
    pub det_score: f32,
    pub embedding: Vec<f32>,
}

/// What `commit_scan` did. `Discarded` means a guard failed — the work is
/// simply dropped and the photo re-enters the queue naturally. `Committed`
/// carries the photo's NEW `chip_revision`: the durable identity of exactly
/// the detection this commit created, which the chip bake publishes against.
#[derive(Debug, PartialEq)]
pub enum CommitOutcome {
    Committed(i64),
    Discarded,
}

pub fn snapshot(conn: &Connection, photo_id: &str, gen: i64) -> rusqlite::Result<ScanSnapshot> {
    let epoch = state_get(conn, "index_epoch")?;
    let revision = face_revision(conn, photo_id)?;
    let mut old = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, x, y, w, h, embedding, model_gen, person_id, assigned_by,
                    similarity, ignored
             FROM faces WHERE photo_id = ?1 ORDER BY face_index",
        )?;
        let rows: Vec<(i64, OldFace)> = stmt
            .query_map(params![photo_id], |r| {
                let row_gen: i64 = r.get(6)?;
                let blob: Vec<u8> = r.get(5)?;
                Ok((
                    r.get::<_, i64>(0)?,
                    OldFace {
                        rect: [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                        embedding: if row_gen == gen {
                            facedet::blob_to_embedding(&blob)
                        } else {
                            None
                        },
                        person_id: r.get(7)?,
                        assigned_by: r.get(8)?,
                        similarity: r.get(9)?,
                        ignored: r.get::<_, i64>(10)? != 0,
                        rejections: Vec::new(),
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut rej_stmt =
            conn.prepare("SELECT person_id FROM face_person_rejections WHERE face_id = ?1")?;
        for (face_id, mut f) in rows {
            f.rejections = rej_stmt
                .query_map(params![face_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            old.push(f);
        }
    }
    Ok(ScanSnapshot {
        epoch,
        gen,
        revision,
        old,
    })
}

/// Old→new correspondence for one photo's reprocess, deterministic on its
/// inputs — computed lock-free by the worker (which also derives embed-time
/// match proposals from it) and recomputed identically inside `commit_scan`.
pub fn plan_carry_over(snap: &ScanSnapshot, new: &[NewFace]) -> HashMap<usize, usize> {
    let old_sides: Vec<facedet::CarrySide> = snap
        .old
        .iter()
        .map(|f| facedet::CarrySide {
            rect: f.rect,
            embedding: f.embedding.as_deref(),
        })
        .collect();
    let new_sides: Vec<facedet::CarrySide> = new
        .iter()
        .map(|f| facedet::CarrySide {
            rect: f.rect,
            // Cross-generation correspondence must be geometric only: withhold
            // new embeddings whenever the old side has none to compare.
            embedding: Some(&f.embedding),
        })
        .collect();
    facedet::match_carry_over(&old_sides, &new_sides)
        .into_iter()
        .map(|(o, n)| (n, o))
        .collect()
}

/// The one write path for scan results — pixel invalidation, manual rescan
/// and model change all commit through here. Guards checked INSIDE the
/// transaction; delete-then-insert (UNIQUE(photo_id, face_index) forbids
/// coexistence and this order is rollback-safe); rejections re-keyed to the
/// replacement faces so "not X" survives. `proposals` are the worker's
/// lock-free embed-time match results (Slice B), applied only to faces that
/// carried no assignment; empty slice = no auto-assignment.
/// `source_unchanged` re-verifies the display file's identity (mtime, size)
/// and is called INSIDE the transaction — the last possible point before the
/// write, so a file rewritten mid-inference can never leave rows describing
/// pixels that no longer exist.
#[allow(clippy::too_many_arguments)]
pub fn commit_scan(
    conn: &mut Connection,
    photo_id: &str,
    snap: &ScanSnapshot,
    new: &[NewFace],
    proposals: &[Option<(i64, f32)>],
    mtime: i64,
    size: i64,
    source_unchanged: &dyn Fn() -> bool,
) -> rusqlite::Result<CommitOutcome> {
    // Correspondence computed before the write, outside the transaction.
    let by_new = plan_carry_over(snap, new);

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Conditional guards — any mismatch: discard, never overwrite newer truth.
    let enabled = index_enabled(&tx)?;
    let epoch = state_get(&tx, "index_epoch")?;
    let gen_now = current_gen(&tx)?.map(|(g, ..)| g);
    let revision = face_revision(&tx, photo_id)?;
    if !enabled
        || epoch != snap.epoch
        || gen_now != Some(snap.gen)
        || revision != snap.revision
        || !source_unchanged()
    {
        return Ok(CommitOutcome::Discarded); // tx drops → rollback
    }

    tx.execute("DELETE FROM faces WHERE photo_id = ?1", params![photo_id])?;
    let mut new_ids: Vec<i64> = Vec::with_capacity(new.len());
    {
        let mut insert = tx.prepare(
            "INSERT INTO faces (photo_id, face_index, x, y, w, h, det_score, embedding,
                                model_gen, person_id, assigned_by, similarity, ignored)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for (i, f) in new.iter().enumerate() {
            let carried = by_new.get(&i).map(|&oi| &snap.old[oi]);
            // Embed-time auto-assignment fills only faces that carried
            // nothing: carried user/auto state and carried `ignored` win.
            let auto = match carried {
                Some(c) if c.person_id.is_some() || c.ignored => None,
                _ => proposals.get(i).copied().flatten(),
            };
            let (person, by, sim): (Option<i64>, Option<String>, Option<f64>) = match carried {
                Some(c) if c.person_id.is_some() => {
                    (c.person_id, c.assigned_by.clone(), c.similarity)
                }
                _ => match auto {
                    Some((pid, score)) => (Some(pid), Some("auto".into()), Some(score as f64)),
                    None => (None, None, None),
                },
            };
            insert.execute(params![
                photo_id,
                i as i64,
                f.rect[0],
                f.rect[1],
                f.rect[2],
                f.rect[3],
                f.det_score,
                facedet::embedding_to_blob(&f.embedding),
                snap.gen,
                person,
                by,
                sim,
                carried.map(|c| c.ignored as i64).unwrap_or(0),
            ])?;
            new_ids.push(tx.last_insert_rowid());
        }
        let mut rej = tx.prepare(
            "INSERT OR IGNORE INTO face_person_rejections (face_id, person_id, created_at)
             VALUES (?1, ?2, ?3)",
        )?;
        for (i, id) in new_ids.iter().enumerate() {
            if let Some(&oi) = by_new.get(&i) {
                for pid in &snap.old[oi].rejections {
                    rej.execute(params![id, pid, now_ms()])?;
                }
            }
        }
    }
    // The revision moves with the rows. Two consequences the design relies on:
    // a second worker that snapshotted the same old revision can no longer
    // commit (its guard above fails), so one detection per revision is the
    // rule; and anything holding face ids from the previous rows — a pending
    // naming-undo, a chip bake still encoding — can tell that they are gone.
    let revision = snap.revision + 1;
    // `chip_revision` moves with the rows and ONLY with the rows: it is the
    // scan identity chip files carry in their names. A later user edit bumps
    // `face_revision` but leaves this untouched — the rects didn't move, so
    // the crops on disk are still exactly the current detection's crops. It
    // is drawn from the durable `scan_seq` control row rather than the
    // per-photo counter: `face_scan` rows die with a privacy delete, and a
    // post-delete rescan restarting at revision 1 would re-mint the exact
    // name (and URL) of a pre-delete chip a failed sweep left on disk.
    let chip_revision = bump_state(&tx, "scan_seq", 0)?;
    tx.execute(
        "INSERT INTO face_scan (photo_id, mtime, size, model_gen, face_count, status,
                                face_revision, chip_revision, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'ok', ?6, ?7, ?8)
         ON CONFLICT(photo_id) DO UPDATE SET
           mtime = excluded.mtime, size = excluded.size, model_gen = excluded.model_gen,
           face_count = excluded.face_count, status = 'ok', last_error = NULL,
           face_revision = excluded.face_revision, chip_revision = excluded.chip_revision,
           scanned_at = excluded.scanned_at",
        params![
            photo_id,
            mtime,
            size,
            snap.gen,
            new.len() as i64,
            revision,
            chip_revision,
            now_ms()
        ],
    )?;
    tx.commit()?;
    Ok(CommitOutcome::Committed(chip_revision))
}

/// Whether a guarded face-artifact publish was allowed to happen.
#[derive(Debug, PartialEq)]
pub enum PublishOutcome {
    /// Files written.
    Published,
    /// A guard refused — a privacy delete landed, or these crops describe a
    /// detection the photo no longer has. Nothing was written.
    Refused,
}

/// Publish a face-calibration report under the same SQLite writer lock that
/// orders Delete-all across Ember processes.
///
/// Report bytes are computed in memory first. The captured `wipe_seq` is then
/// rechecked after `BEGIN IMMEDIATE`: if a delete committed in between (even if
/// someone has since re-enabled indexing), publication refuses. If publication
/// takes the lock first, Delete-all waits, commits next, and its artifact sweep
/// removes the report. This prevents a parked reporter from recreating personal
/// data after deletion has returned.
pub fn publish_calibration_report(
    conn: &mut Connection,
    expected_wipe_seq: i64,
    report_path: &Path,
    bytes: &[u8],
) -> Result<PublishOutcome, String> {
    publish_calibration_report_with(
        conn,
        expected_wipe_seq,
        report_path,
        bytes,
        |path, bytes| std::fs::write(path, bytes),
    )
}

fn failed_calibration_report(report_path: &Path, failure: String) -> String {
    match std::fs::remove_file(report_path) {
        Ok(()) => failure,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => failure,
        Err(cleanup) => format!(
            "{failure}; failed to remove incomplete face-calibration report {}: {cleanup}",
            report_path.display()
        ),
    }
}

fn publish_calibration_report_with<W>(
    conn: &mut Connection,
    expected_wipe_seq: i64,
    report_path: &Path,
    bytes: &[u8],
    write: W,
) -> Result<PublishOutcome, String>
where
    W: FnOnce(&Path, &[u8]) -> std::io::Result<()>,
{
    let valid_name = report_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("faces-calibration-") && name.ends_with(".json"));
    let in_report_dir = report_path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "perf-reports");
    if !valid_name || !in_report_dir {
        return Err(format!(
            "invalid face-calibration report path: {}",
            report_path.display()
        ));
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let enabled = index_enabled(&tx).map_err(|e| e.to_string())?;
    let current_wipe_seq = wipe_seq(&tx).map_err(|e| e.to_string())?;
    if !enabled || current_wipe_seq != expected_wipe_seq {
        return Ok(PublishOutcome::Refused);
    }
    if let Err(e) = write(report_path, bytes) {
        return Err(failed_calibration_report(
            report_path,
            format!("face-calibration report write failed: {e}"),
        ));
    }
    if let Err(e) = tx.commit() {
        return Err(failed_calibration_report(
            report_path,
            format!("face-calibration report commit failed: {e}"),
        ));
    }
    Ok(PublishOutcome::Published)
}

/// Write face chips to the cache, under the DB write lock — the cross-process
/// half of the privacy-delete guarantee.
///
/// Chips arrive as **encoded bytes held in memory**; this is the only place in
/// the app that may put a face crop on disk. `BEGIN IMMEDIATE` takes SQLite's
/// single writer lock, so this transaction and `delete_face_data`'s wipe can
/// never interleave, in this process or the other one sharing the DB:
///
/// - If the wipe committed first, the guards below (the scan row is gone, or
///   the epoch moved) refuse and **not one byte is written**. There is no
///   staged file to leak, because staging never touched the disk.
/// - If this critical section ran first, the wipe's chip sweep — which runs
///   after its commit, when no publish can create anything any more — removes
///   what we wrote.
///
/// `revision` pins the publication to the exact detection that produced the
/// crops: `face_scan` must still say `'ok'` at that `chip_revision`. A photo
/// marked stale by pixel invalidation, or re-detected since (every commit
/// moves `chip_revision`), refuses the older bake instead of publishing crops
/// of rects that are no longer in the DB.
///
/// **All-expected-artifacts contract**: the DB's rects at this revision define
/// exactly which files must exist. Every expected path must either be supplied
/// in `chips` or already exist on disk (a file at a `r{revision}` name can
/// only have come from an earlier publish of this same detection — the name
/// IS the proof). Supplying a path outside the expected set is an error. A
/// passing publish then **sweeps every other chip artifact of this photo**
/// (older revisions, pre-revision legacy names, dead temps) inside the same
/// critical section, so no leftover can sit beside the current set — and a
/// refused publish touches nothing at all.
///
/// `index_enabled` is deliberately NOT a guard: disabling indexing keeps the
/// data, so a chip for a row that still exists must still be repairable.
pub fn publish_chips(
    conn: &mut Connection,
    cache_dir: &Path,
    photo_id: &str,
    epoch: i64,
    revision: i64,
    chips: &[(PathBuf, Vec<u8>)],
) -> Result<PublishOutcome, String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let scan_ok: Option<i64> = tx
        .query_row(
            "SELECT chip_revision FROM face_scan WHERE photo_id = ?1 AND status = 'ok'",
            params![photo_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if scan_ok != Some(revision)
        || state_get(&tx, "index_epoch").map_err(|e| e.to_string())? != epoch
    {
        return Ok(PublishOutcome::Refused); // tx drops → rollback, nothing written
    }
    // The expected artifact set, from the rows this transaction can see.
    let expected: Vec<PathBuf> = face_rects(&tx, photo_id)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|(n, _)| cache_dir.join(crate::preview::face_chip_name(photo_id, *n, revision)))
        .collect();
    let supplied: std::collections::HashSet<&Path> =
        chips.iter().map(|(p, _)| p.as_path()).collect();
    for (p, _) in chips {
        if !expected.iter().any(|e| e == p) {
            return Err(format!(
                "chip publish for {photo_id}: {} is not an artifact of revision {revision}",
                p.display()
            ));
        }
    }
    for e in &expected {
        if !supplied.contains(e.as_path()) && !e.exists() {
            return Err(format!(
                "chip publish for {photo_id}: incomplete bake — {} neither supplied nor on disk",
                e.display()
            ));
        }
    }
    // Sweep every other chip artifact of this photo while we hold the lock:
    // older revisions, legacy unrevisioned names, temps of dead writers. No
    // publisher for a different revision can be running (this lock), and any
    // that arrives later is refused by the revision guard above. A cache dir
    // (or entry) we cannot ENUMERATE fails the publish loudly — "couldn't
    // look" must never read as "nothing stale" (a missing dir is the one
    // benign case: nothing can be stale in it, and the writes below will say
    // so themselves if it is genuinely gone).
    match std::fs::read_dir(cache_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry =
                    entry.map_err(|err| format!("cache dir entry unreadable in sweep: {err}"))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = entry.path();
                if crate::preview::is_photo_chip_file(&name, photo_id) && !expected.contains(&path)
                {
                    if let Err(err) = std::fs::remove_file(&path) {
                        if err.kind() != std::io::ErrorKind::NotFound {
                            return Err(format!("stale chip not removable ({name}): {err}"));
                        }
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cache dir unreadable for stale sweep: {e}")),
    }
    // Inside the critical section: a temp beside each destination, then
    // rename. A failure here is reported, never swallowed — a half-written
    // chip must not read as a published one.
    let mut written: Vec<PathBuf> = Vec::with_capacity(chips.len());
    for (out, bytes) in chips {
        match crate::preview::publish_bytes(out, bytes) {
            Ok(()) => written.push(out.clone()),
            Err(e) => {
                for done in &written {
                    let _ = std::fs::remove_file(done);
                }
                return Err(format!("chip write failed ({}): {e}", out.display()));
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(PublishOutcome::Published)
}

/// Per-photo failure: lifetime `attempts` counter + parked error text.
/// Session backoff lives in the worker's memory, not here.
pub fn record_scan_error(
    conn: &Connection,
    photo_id: &str,
    gen: i64,
    err: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO face_scan (photo_id, mtime, size, model_gen, face_count, status,
                                attempts, last_error, scanned_at)
         VALUES (?1, 0, 0, ?2, 0, 'error', 1, ?3, ?4)
         ON CONFLICT(photo_id) DO UPDATE SET
           status = 'error', attempts = face_scan.attempts + 1,
           last_error = excluded.last_error, scanned_at = excluded.scanned_at",
        params![photo_id, gen, err, now_ms()],
    )?;
    Ok(())
}

// ---------- auto-recognition data (Slice B) ----------

/// The compatibility rule (plan rev 4), in one place: an embedding may enter
/// a computation only from a photo whose last scan SUCCEEDED — a 'stale' or
/// 'error' row's faces describe pixels we are no longer sure about. Combine
/// with `f.model_gen = <current gen>`; queries alias `faces` as `f`.
const SCAN_OK_JOIN: &str = "JOIN face_scan s ON s.photo_id = f.photo_id AND s.status = 'ok'";

/// Matching prototypes: USER-CONFIRMED faces only, current generation, from
/// successfully scanned photos, up to `max_exemplars` per person picked for
/// diversity. Auto-assigned faces never train future matches — one false
/// positive must not move anyone's anchor.
pub fn person_prototypes(
    conn: &Connection,
    gen: i64,
    max_exemplars: usize,
) -> rusqlite::Result<Vec<facedet::PersonProtos>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT f.person_id, f.embedding FROM faces f
         {SCAN_OK_JOIN}
         WHERE f.person_id IS NOT NULL AND f.assigned_by = 'user'
           AND f.ignored = 0 AND f.model_gen = ?1
         ORDER BY f.person_id, f.det_score DESC"
    ))?;
    let rows: Vec<(i64, Vec<u8>)> = stmt
        .query_map(params![gen], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut out: Vec<facedet::PersonProtos> = Vec::new();
    for (pid, blob) in rows {
        let Some(emb) = facedet::blob_to_embedding(&blob) else {
            continue;
        };
        match out.last_mut() {
            Some(p) if p.person_id == pid => p.exemplars.push(emb),
            _ => out.push(facedet::PersonProtos {
                person_id: pid,
                exemplars: vec![emb],
            }),
        }
    }
    for p in &mut out {
        // Junk confirmed faces (a named back-of-head) must not become
        // references — drop outliers first, then pick for diversity.
        let sane: Vec<Vec<f32>> = facedet::filter_exemplar_outliers(&p.exemplars)
            .iter()
            .map(|&i| p.exemplars[i].clone())
            .collect();
        let picked = facedet::select_exemplars(&sane, max_exemplars);
        p.exemplars = picked.iter().map(|&i| sane[i].clone()).collect();
    }
    Ok(out)
}

/// face_id → rejected person ids, for one photo (sweep + panel exclusions).
pub fn photo_rejections(
    conn: &Connection,
    photo_id: &str,
) -> rusqlite::Result<HashMap<i64, std::collections::HashSet<i64>>> {
    let mut stmt = conn.prepare(
        "SELECT r.face_id, r.person_id FROM face_person_rejections r
         JOIN faces f ON f.id = r.face_id WHERE f.photo_id = ?1",
    )?;
    let mut map: HashMap<i64, std::collections::HashSet<i64>> = HashMap::new();
    let rows = stmt.query_map(params![photo_id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (fid, pid) = row?;
        map.entry(fid).or_default().insert(pid);
    }
    Ok(map)
}

/// Photos that still have unassigned, unignored, current-gen faces — the
/// post-naming sweep's work list.
pub fn sweep_candidates(conn: &Connection, gen: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.photo_id FROM faces f
         JOIN face_scan s ON s.photo_id = f.photo_id
         WHERE f.person_id IS NULL AND f.ignored = 0 AND f.model_gen = ?1
           AND s.status = 'ok'",
    )?;
    let rows = stmt.query_map(params![gen], |r| r.get(0))?;
    rows.collect()
}

/// Sweep one photo: snapshot → match lock-free → one short conditional
/// commit. A user edit between snapshot and commit (face_revision bump)
/// discards the whole photo's sweep — the human always wins. Auto
/// assignments do NOT bump face_revision themselves: they are machine state,
/// regenerable, and must never block the undo-naming toast.
pub fn sweep_photo(
    conn: &mut Connection,
    photo_id: &str,
    gen: i64,
    protos: &[facedet::PersonProtos],
    threshold: f32,
    margin: f32,
) -> rusqlite::Result<usize> {
    let revision = face_revision(conn, photo_id)?;
    let epoch = state_get(conn, "index_epoch")?;
    let rejections = photo_rejections(conn, photo_id)?;
    let faces: Vec<(i64, Vec<u8>)> = {
        // Scan status is re-checked here, not just in `sweep_candidates`: a
        // rescan can mark this photo stale between the two.
        let mut stmt = conn.prepare(&format!(
            "SELECT f.id, f.embedding FROM faces f
             {SCAN_OK_JOIN}
             WHERE f.photo_id = ?1 AND f.person_id IS NULL AND f.ignored = 0
               AND f.model_gen = ?2"
        ))?;
        let v: Vec<(i64, Vec<u8>)> = stmt
            .query_map(params![photo_id, gen], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    // Persons already on another face of this photo — one person cannot be
    // two faces in the same picture.
    let present: std::collections::HashSet<i64> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT person_id FROM faces
             WHERE photo_id = ?1 AND person_id IS NOT NULL AND ignored = 0",
        )?;
        let v: std::collections::HashSet<i64> = stmt
            .query_map(params![photo_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    // Comparisons outside any lock.
    let empty = std::collections::HashSet::new();
    let candidates: Vec<(usize, i64, f32)> = faces
        .iter()
        .enumerate()
        .filter_map(|(i, (fid, blob))| {
            let emb = facedet::blob_to_embedding(blob)?;
            let rejected = rejections.get(fid).unwrap_or(&empty);
            facedet::match_face(&emb, protos, rejected, threshold, margin)
                .map(|(pid, score)| (i, pid, score))
        })
        .collect();
    let matches: Vec<(i64, i64, f32)> = facedet::one_face_per_person(candidates, &present)
        .into_iter()
        .map(|(i, pid, score)| (faces[i].0, pid, score))
        .collect();
    if matches.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !index_enabled(&tx)?
        || state_get(&tx, "index_epoch")? != epoch
        || face_revision(&tx, photo_id)? != revision
        || current_gen(&tx)?.map(|(g, ..)| g) != Some(gen)
    {
        return Ok(0); // tx drops → rollback; the sweep re-runs later
    }
    let mut applied = 0;
    for (fid, pid, score) in &matches {
        // Conditional per face too: only fill still-empty slots.
        applied += tx.execute(
            "UPDATE faces SET person_id = ?2, assigned_by = 'auto', similarity = ?3
             WHERE id = ?1 AND person_id IS NULL AND ignored = 0",
            params![fid, pid, *score as f64],
        )?;
    }
    tx.commit()?;
    Ok(applied)
}

/// Calibration report over the CURRENT naming state (plan Slice B): how well
/// do user-confirmed identities separate on this library? Leave-one-out
/// positives (each confirmed face scored against its person's OTHER faces),
/// cross-person negatives, and the unassigned-face score field. Thresholds
/// get chosen from the gap between the positive and negative distributions.
pub fn calibration_data(conn: &Connection, gen: i64) -> rusqlite::Result<serde_json::Value> {
    let mut stmt = conn.prepare(&format!(
        "SELECT f.person_id, per.name, f.embedding FROM faces f
         JOIN persons per ON per.id = f.person_id
         {SCAN_OK_JOIN}
         WHERE f.assigned_by = 'user' AND f.ignored = 0 AND f.model_gen = ?1
         ORDER BY f.person_id, f.det_score DESC"
    ))?;
    let rows: Vec<(i64, String, Vec<u8>)> = stmt
        .query_map(params![gen], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut persons: Vec<(i64, String, Vec<Vec<f32>>)> = Vec::new();
    for (pid, name, blob) in rows {
        let Some(emb) = facedet::blob_to_embedding(&blob) else {
            continue;
        };
        match persons.last_mut() {
            Some((id, _, embs)) if *id == pid => embs.push(emb),
            _ => persons.push((pid, name, vec![emb])),
        }
    }

    let exemplars_of = |embs: &[Vec<f32>]| -> Vec<Vec<f32>> {
        facedet::select_exemplars(embs, 5)
            .iter()
            .map(|&i| embs[i].clone())
            .collect()
    };
    let score_vs = |emb: &[f32], exemplars: &[Vec<f32>]| -> f32 {
        exemplars
            .iter()
            .map(|e| facedet::cosine(emb, e))
            .fold(f32::NEG_INFINITY, f32::max)
    };

    let mut positives: Vec<f32> = Vec::new();
    let mut negatives: Vec<f32> = Vec::new();
    let mut person_reports = Vec::new();
    for (i, (pid, name, embs)) in persons.iter().enumerate() {
        let mut loo: Vec<f32> = Vec::new();
        if embs.len() >= 2 {
            for k in 0..embs.len() {
                let rest: Vec<Vec<f32>> = embs
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != k)
                    .map(|(_, e)| e.clone())
                    .collect();
                loo.push(score_vs(&embs[k], &exemplars_of(&rest)));
            }
            positives.extend(&loo);
        }
        let mut neg: Vec<f32> = Vec::new();
        for (j, (_, _, other)) in persons.iter().enumerate() {
            if i == j {
                continue;
            }
            let ex = exemplars_of(other);
            for e in embs {
                neg.push(score_vs(e, &ex));
            }
        }
        negatives.extend(&neg);
        person_reports.push(serde_json::json!({
            "personId": pid,
            "name": name,
            "confirmedFaces": embs.len(),
            "looPositives": loo,
            "negativesMax": neg.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        }));
    }

    // Unassigned field: what would auto-assignment see right now?
    let protos = person_prototypes(conn, gen, 5)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT f.embedding FROM faces f
         {SCAN_OK_JOIN}
         WHERE f.person_id IS NULL AND f.ignored = 0 AND f.model_gen = ?1"
    ))?;
    let blobs: Vec<Vec<u8>> = stmt
        .query_map(params![gen], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut unassigned: Vec<serde_json::Value> = Vec::new();
    for blob in blobs.iter().take(2000) {
        let Some(emb) = facedet::blob_to_embedding(blob) else {
            continue;
        };
        let mut scores: Vec<(i64, f32)> = protos
            .iter()
            .map(|p| (p.person_id, score_vs(&emb, &p.exemplars)))
            .collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        if let Some(&(pid, best)) = scores.first() {
            let second = scores.get(1).map(|s| s.1).unwrap_or(f32::NEG_INFINITY);
            unassigned.push(serde_json::json!({
                "bestPerson": pid,
                "best": best,
                "margin": if second.is_finite() { best - second } else { best },
            }));
        }
    }

    let pct = |v: &mut Vec<f32>, p: f64| -> Option<f32> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(f32::total_cmp);
        let idx = ((p / 100.0) * (v.len() - 1) as f64).round() as usize;
        Some(v[idx.min(v.len() - 1)])
    };
    let (mut pos, mut neg) = (positives.clone(), negatives.clone());
    Ok(serde_json::json!({
        "modelGen": gen,
        "persons": person_reports,
        "unassigned": unassigned,
        "summary": {
            "positives": positives.len(),
            "posMin": pct(&mut pos, 0.0),
            "posP5": pct(&mut pos, 5.0),
            "posMedian": pct(&mut pos, 50.0),
            "negatives": negatives.len(),
            "negP95": pct(&mut neg, 95.0),
            "negMax": pct(&mut neg, 100.0),
        },
    }))
}

/// Scan-time state of one photo, for the worker's job selection. (The DB row
/// also carries `face_count` for the UI; job selection doesn't read it.)
pub struct ScanRow {
    pub mtime: i64,
    pub size: i64,
    pub model_gen: i64,
    pub status: String,
}

pub fn scan_row(conn: &Connection, photo_id: &str) -> rusqlite::Result<Option<ScanRow>> {
    conn.query_row(
        "SELECT mtime, size, model_gen, status FROM face_scan WHERE photo_id = ?1",
        params![photo_id],
        |r| {
            Ok(ScanRow {
                mtime: r.get(0)?,
                size: r.get(1)?,
                model_gen: r.get(2)?,
                status: r.get(3)?,
            })
        },
    )
    .optional()
}

// ---------- command-layer DTOs ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonOut {
    pub id: i64,
    pub name: String,
    /// Faces of this person in the queried folder (trashed excluded).
    pub folder_count: i64,
    /// Representative face for the chip (photoId, faceIndex, and the
    /// chip_revision its baked file is named under): from the queried folder
    /// when the person appears in it, otherwise from anywhere — but never a
    /// trashed or missing photo, and only from a scan whose chips are
    /// servable. See `person_row`.
    pub rep_photo_id: Option<String>,
    pub rep_face_index: Option<i64>,
    pub rep_revision: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChipRef {
    pub photo_id: String,
    pub face_index: i64,
    pub face_id: i64,
    /// The photo's `chip_revision` — chip URLs carry it, so a crop from an
    /// older detection can never satisfy a current request (and the webview's
    /// image cache can never show one under the new identity).
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterOut {
    pub face_ids: Vec<i64>,
    /// EVERY member, in cluster order — the panel previews a few but must be
    /// able to offer per-face control over all of them (a hidden face can't
    /// be excluded from a naming).
    pub chips: Vec<ChipRef>,
    pub size: i64,
    pub photo_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClustersOut {
    /// Recurring groups (≥2 faces), largest first.
    pub clusters: Vec<ClusterOut>,
    /// Singletons — faces resembling nothing else in the folder (mostly junk
    /// detections and one-off strangers). Without this list they'd be
    /// invisible and undismissable forever.
    pub loose: Vec<ChipRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceOut {
    pub face_id: i64,
    pub photo_id: String,
    pub face_index: i64,
    pub rect: [f32; 4],
    pub det_score: f32,
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
    pub assigned_by: Option<String>,
    pub ignored: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceScanStatus {
    pub enabled: bool,
    pub total: i64,
    pub scanned: i64,
    /// TERMINAL failures only: error rows whose session retries are spent.
    /// A photo the worker will still retry is `retrying`, not failed —
    /// declaring it failed mid-schedule was exactly the round-3 finding.
    pub errors: i64,
    /// Error rows with retries still to come (this session, or a fresh
    /// session after relaunch). Counted inside `pending` too.
    pub retrying: i64,
    /// Photos with work still outstanding: never scanned, marked stale, or
    /// errored with retries remaining. The panel's "Scanning…" must key off
    /// THIS, not `scanned < total` — a photo parked in a terminal error is
    /// finished, and counting it as unscanned left the panel scanning
    /// forever.
    pub pending: i64,
}

/// (face_id, photo_id, prior person_id, prior assigned_by, prior similarity)
pub type PriorAssignment = (i64, String, Option<i64>, Option<String>, Option<f64>);

/// Exact prior state captured by `face_set_name` for its session-scoped undo.
#[derive(Debug, Clone)]
pub struct NamingOp {
    pub person_id: i64,
    pub person_created: bool,
    pub prior: Vec<PriorAssignment>,
    /// photo_id → face_revision AFTER this op; undo skips photos edited later.
    pub revisions: HashMap<String, i64>,
    /// Rejections this op inserted (undo removes exactly these).
    pub rejections: Vec<(i64, i64)>,
    /// Faces already auto-assigned to this person BEFORE the op. Undo clears
    /// the auto assignments this naming caused and only those — naming into
    /// an existing person must not delete labels that predate it.
    pub prior_auto: std::collections::HashSet<i64>,
    /// photo_id → face_revision, for every photo that held one of those
    /// pre-existing autos. A re-detection replaces those rows with new ids, so
    /// "not in `prior_auto`" would wrongly read as "created by this naming".
    /// A revision that moved means we can no longer tell, and undo leaves that
    /// photo's machine labels alone.
    pub prior_auto_revisions: HashMap<String, i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamingResult {
    pub person: PersonOut,
    pub affected_face_ids: Vec<i64>,
}

/// Rename result: a name collision is data, not an error — the UI turns it
/// into a merge offer.
#[derive(Debug, Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "status"
)]
pub enum RenameOutcome {
    Renamed,
    Conflict { target_id: i64, target_name: String },
}

// ---------- Store methods (command layer, mutex connection) ----------

impl Store {
    #[cfg_attr(not(test), allow(dead_code))] // command layer reads it via face_scan_status
    pub fn faces_enabled(&self) -> rusqlite::Result<bool> {
        index_enabled(&self.lock_conn())
    }

    /// Phase 1 of every app-driven faces-enabled change.
    ///
    /// Commits `enabled_mirror_ok = 0` on its own: from this instant until a
    /// phase-2 mirror write succeeds, settings.toml is untrusted and the DB
    /// decides at launch. A crash, a kill -9 or a failed file write anywhere
    /// after this point therefore fails **closed**. Returns the `wipe_seq` the
    /// caller must still be looking at when phase 2 runs.
    fn begin_enabled_intent(&self) -> rusqlite::Result<i64> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        state_set(&tx, "enabled_mirror_ok", 0)?;
        let seq = state_get_or(&tx, "wipe_seq", 0)?;
        tx.commit()?;
        Ok(seq)
    }

    /// Phase 2: apply the intent to the DB **and** write settings.toml inside
    /// ONE `BEGIN IMMEDIATE` critical section.
    ///
    /// This is the whole cross-process argument, and it rests on SQLite's
    /// single writer lock (WAL, shared DB file, two Ember processes):
    ///
    /// 1. `BEGIN IMMEDIATE` takes that lock before anything here runs and
    ///    holds it past `mirror(..)` until COMMIT, so **no two enabled-intent
    ///    phase 2s can overlap** — in this process or the other one. Every
    ///    such operation is totally ordered.
    /// 2. Each one writes the file and its own DB value inside its own slot,
    ///    so the last slot to run leaves the file and the DB agreeing. There
    ///    is no "write the file later" step for a stale writer to perform.
    /// 3. A writer whose intent was decided before a privacy delete that has
    ///    since committed sees `wipe_seq` moved and **refuses to re-enable**
    ///    (`Superseded`) — a delete-all cannot be undone by an enable that was
    ///    already in flight when it landed.
    /// 4. If the file write fails, `enabled_mirror_ok` stays 0 (phase 1
    ///    committed it): the DB wins at the next launch and the file is
    ///    rewritten from it.
    fn commit_enabled_intent(
        &self,
        enabled: bool,
        wipe: bool,
        wipe_seq_at_start: i64,
        mirror: &dyn Fn(bool) -> std::io::Result<()>,
    ) -> rusqlite::Result<MirrorOutcome> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let was = index_enabled(&tx)?;
        let seq_now = state_get_or(&tx, "wipe_seq", 0)?;
        // Turning indexing ON is the only direction that can resurrect what a
        // privacy delete just put down, so it is the only one that has to
        // yield. Disabling and deleting are always safe to apply.
        if enabled && !wipe && seq_now != wipe_seq_at_start {
            // Mirror the DB's CURRENT value, not `false`: by the time we get
            // here someone may legitimately have re-enabled indexing after the
            // delete, and writing a stale `false` would leave the file
            // disagreeing with a DB the flag then calls mirrored. The one rule
            // this branch enforces is that WE do not decide the value.
            let mirror_error = mirror_now(&tx, was, mirror)?;
            tx.commit()?;
            return Ok(MirrorOutcome {
                changed: false,
                superseded: true,
                mirror_error,
            });
        }
        if was != enabled {
            state_set(&tx, "index_enabled", enabled as i64)?;
            bump_epoch(&tx)?;
        }
        if wipe {
            // Always, even if indexing was already off: this kills every
            // in-flight worker commit and every pending chip publish in both
            // processes before the rows go.
            bump_epoch(&tx)?;
            bump_state(&tx, "wipe_seq", 0)?;
            state_set(&tx, "chip_sweep_pending", 1)?;
            tx.execute("DELETE FROM face_person_rejections", [])?;
            tx.execute("DELETE FROM faces", [])?;
            tx.execute("DELETE FROM persons", [])?;
            tx.execute("DELETE FROM face_scan", [])?;
        }
        let mirror_error = mirror_now(&tx, enabled, mirror)?;
        tx.commit()?;
        Ok(MirrorOutcome {
            changed: was != enabled,
            superseded: false,
            mirror_error,
        })
    }

    /// Explicit enable/disable. Applies the DB change and settings.toml as one
    /// serialized unit (§commit_enabled_intent).
    pub fn set_faces_enabled(
        &self,
        enabled: bool,
        mirror: &dyn Fn(bool) -> std::io::Result<()>,
    ) -> rusqlite::Result<MirrorOutcome> {
        let seq = self.begin_enabled_intent()?;
        self.commit_enabled_intent(enabled, false, seq, mirror)
    }

    /// Privacy delete: disables indexing durably (control rows are never
    /// deleted), bumps the epoch, wipes every face-derived row and mirrors
    /// `enabled = false` — all in the transaction described above.
    ///
    /// Derived face files are the caller's cleanup, and it is safe **after**
    /// this commit: from the commit on, every chip publish takes the same writer
    /// lock, finds the rows gone and the epoch moved, and writes nothing at all
    /// (chips are held in memory until that check passes). The wipe also sets
    /// the legacy-named `chip_sweep_pending`, so a chip or calibration-report
    /// cleanup that fails or is interrupted is retried at the next launch
    /// instead of being forgotten.
    pub fn delete_face_data(
        &self,
        mirror: &dyn Fn(bool) -> std::io::Result<()>,
    ) -> rusqlite::Result<MirrorOutcome> {
        let seq = self.begin_enabled_intent()?;
        self.commit_enabled_intent(false, true, seq, mirror)
    }

    /// Whether a privacy delete's derived-artifact sweep is still owed. The DB
    /// key keeps its historical name; callers clear it only after both chips
    /// and face-calibration reports have been swept successfully.
    pub fn chip_sweep_pending(&self) -> rusqlite::Result<bool> {
        Ok(state_get_or(&self.lock_conn(), "chip_sweep_pending", 0)? != 0)
    }

    /// Capture the privacy-delete generation immediately before sweeping face
    /// artifacts. A successful sweep may clear the persisted retry debt only
    /// while this generation is still current.
    pub fn face_artifact_wipe_seq(&self) -> rusqlite::Result<i64> {
        wipe_seq(&self.lock_conn())
    }

    /// Clear a completed artifact sweep's retry debt without stealing a newer
    /// delete's debt. `BEGIN IMMEDIATE` makes the comparison and clear one
    /// writer-ordered operation across Ember processes.
    pub fn clear_chip_sweep_pending_if_wipe_seq(
        &self,
        expected_wipe_seq: i64,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if wipe_seq(&tx)? != expected_wipe_seq {
            return Ok(false);
        }
        state_set(&tx, "chip_sweep_pending", 0)?;
        tx.commit()?;
        Ok(true)
    }

    /// Launch-time reconciliation, in one writer-lock critical section.
    ///
    /// A hand-edited settings.toml expresses user intent and wins on launch —
    /// but only while the file is a trustworthy mirror. If a previous
    /// app-driven write (delete-all, the toggle) never reached it, the DB is
    /// authoritative and the file is rewritten from it here, so a completed
    /// privacy delete can never relaunch enabled.
    ///
    /// The file is READ inside the transaction, not before it: reading it
    /// outside would let another process's (correctly serialized) change land
    /// in between, and we would adopt a value that is already history.
    pub fn sync_faces_enabled_from_settings(
        &self,
        read_file: &dyn Fn() -> bool,
        mirror: &dyn Fn(bool) -> std::io::Result<()>,
    ) -> rusqlite::Result<MirrorOutcome> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let db = index_enabled(&tx)?;
        if state_get_or(&tx, "enabled_mirror_ok", 1)? == 0 {
            // DB wins; put the file back in agreement with it.
            let mirror_error = mirror_now(&tx, db, mirror)?;
            tx.commit()?;
            return Ok(MirrorOutcome {
                changed: false,
                superseded: false,
                mirror_error,
            });
        }
        let file = read_file();
        if file != db {
            state_set(&tx, "index_enabled", file as i64)?;
            bump_epoch(&tx)?;
        }
        tx.commit()?;
        Ok(MirrorOutcome {
            changed: file != db,
            superseded: false,
            mirror_error: None,
        })
    }

    /// `exhausted` is the worker's session view of which error rows are
    /// terminal (`faces::exhausted_errors`); every other error row still has
    /// a retry coming — now, or in a fresh session — and counts as pending.
    pub fn face_scan_status(
        &self,
        folder_id: i64,
        exhausted: &std::collections::HashSet<String>,
    ) -> rusqlite::Result<FaceScanStatus> {
        let conn = self.lock_conn();
        let enabled = index_enabled(&conn)?;
        let (total, scanned, unscanned) = conn.query_row(
            "SELECT COUNT(*),
                    COUNT(s.photo_id) FILTER (WHERE s.status = 'ok'),
                    COUNT(*) FILTER (WHERE s.photo_id IS NULL OR s.status = 'stale')
             FROM photos p LEFT JOIN face_scan s ON s.photo_id = p.id
             WHERE p.folder_id = ?1 AND p.trashed = 0 AND p.missing = 0",
            params![folder_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;
        let error_ids: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT s.photo_id FROM face_scan s JOIN photos p ON p.id = s.photo_id
                 WHERE p.folder_id = ?1 AND p.trashed = 0 AND p.missing = 0
                   AND s.status = 'error'",
            )?;
            let v: Vec<String> = stmt
                .query_map(params![folder_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        let errors = error_ids
            .iter()
            .filter(|id| exhausted.contains(*id))
            .count() as i64;
        let retrying = error_ids.len() as i64 - errors;
        Ok(FaceScanStatus {
            enabled,
            total,
            scanned,
            errors,
            retrying,
            pending: unscanned + retrying,
        })
    }

    /// Unnamed faces for the People panel: current-gen, scan-ok, unassigned,
    /// unignored, untrashed — clustered greedily in det_score order.
    /// Recurring groups (≥2, largest first) plus the loose singletons.
    pub fn face_clusters(&self, folder_id: i64, threshold: f32) -> rusqlite::Result<ClustersOut> {
        let conn = self.lock_conn();
        let Some((gen, ..)) = current_gen(&conn)? else {
            return Ok(ClustersOut {
                clusters: Vec::new(),
                loose: Vec::new(),
            });
        };
        let mut stmt = conn.prepare(
            "SELECT f.id, f.photo_id, f.face_index, f.embedding, s.chip_revision
             FROM faces f
             JOIN photos p ON p.id = f.photo_id
             JOIN face_scan s ON s.photo_id = f.photo_id
             WHERE p.folder_id = ?1 AND p.trashed = 0
               AND s.status = 'ok' AND f.model_gen = ?2
               AND f.person_id IS NULL AND f.ignored = 0
             ORDER BY f.det_score DESC",
        )?;
        let rows: Vec<(i64, String, i64, Vec<u8>, i64)> = stmt
            .query_map(params![folder_id, gen], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        let embeddings: Vec<Vec<f32>> = rows
            .iter()
            .map(|(_, _, _, blob, _)| facedet::blob_to_embedding(blob).unwrap_or_default())
            .collect();
        let chip = |i: usize| ChipRef {
            photo_id: rows[i].1.clone(),
            face_index: rows[i].2,
            face_id: rows[i].0,
            revision: rows[i].4,
        };
        let mut clusters: Vec<ClusterOut> = Vec::new();
        let mut loose: Vec<ChipRef> = Vec::new();
        for members in facedet::cluster_greedy(&embeddings, threshold) {
            if members.len() < 2 {
                loose.push(chip(members[0]));
                continue;
            }
            let photo_count = members
                .iter()
                .map(|&i| rows[i].1.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len() as i64;
            clusters.push(ClusterOut {
                face_ids: members.iter().map(|&i| rows[i].0).collect(),
                chips: members.iter().map(|&i| chip(i)).collect(),
                size: members.len() as i64,
                photo_count,
            });
        }
        clusters.sort_by_key(|c| std::cmp::Reverse(c.size));
        clusters.truncate(30);
        loose.truncate(200);
        Ok(ClustersOut { clusters, loose })
    }

    /// Name faces: find-or-create by normalized name, assign as 'user'.
    /// Slice A: assigns exactly the given faces — no sweep. Captures exact
    /// prior state for the session-scoped undo toast.
    ///
    /// The selection is resolved BEFORE the person is created: a concurrent
    /// rescan replaces face rows, and a naming whose faces have all been
    /// replaced must fail loudly instead of leaving behind a person with no
    /// faces and reporting success.
    pub fn face_set_name(
        &self,
        face_ids: &[i64],
        name: &str,
    ) -> Result<(NamingResult, NamingOp), String> {
        let display = name.trim();
        let norm = display.to_lowercase();
        if norm.is_empty() {
            return Err("a name cannot be empty".into());
        }
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut live: Vec<PriorAssignment> = Vec::new();
        for &fid in face_ids {
            // A face replaced by a concurrent rescan simply isn't here anymore.
            let row = tx
                .query_row(
                    "SELECT photo_id, person_id, assigned_by, similarity FROM faces WHERE id = ?1",
                    params![fid],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<f64>>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some((photo_id, p_person, p_by, p_sim)) = row {
                live.push((fid, photo_id, p_person, p_by, p_sim));
            }
        }
        if live.is_empty() {
            return Err(
                "those faces were re-detected while you were naming them — reopen \
                 the People panel and try again"
                    .into(),
            );
        }
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM persons WHERE name_norm = ?1",
                params![norm],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let (person_id, person_created) = match existing {
            Some(id) => (id, false),
            None => {
                tx.execute(
                    "INSERT INTO persons (name, name_norm, created_at) VALUES (?1, ?2, ?3)",
                    params![display, norm, now_ms()],
                )
                .map_err(|e| e.to_string())?;
                (tx.last_insert_rowid(), true)
            }
        };
        // Auto assignments this person already had: undo must leave exactly
        // these alone (they do not exist because of this naming). Their
        // photos' revisions come along so a re-detection between now and the
        // undo is detectable — the replacement rows carry new ids.
        let (prior_auto, prior_auto_revisions) = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, photo_id FROM faces
                     WHERE person_id = ?1 AND assigned_by = 'auto'",
                )
                .map_err(|e| e.to_string())?;
            let rows: Vec<(i64, String)> = stmt
                .query_map(params![person_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let mut ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let mut revs: HashMap<String, i64> = HashMap::new();
            for (fid, photo) in rows {
                ids.insert(fid);
                if let std::collections::hash_map::Entry::Vacant(slot) = revs.entry(photo.clone()) {
                    slot.insert(face_revision(&tx, &photo).map_err(|e| e.to_string())?);
                }
            }
            (ids, revs)
        };
        let mut prior = Vec::new();
        let mut rejections = Vec::new();
        let mut affected = Vec::new();
        let mut photos: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (fid, photo_id, p_person, p_by, p_sim) in live {
            tx.execute(
                "UPDATE faces SET person_id = ?2, assigned_by = 'user', similarity = NULL
                 WHERE id = ?1",
                params![fid, person_id],
            )
            .map_err(|e| e.to_string())?;
            // Any correction records a rejection of the displaced identity —
            // whether that assignment was auto OR manual (review-2).
            if let Some(displaced) = p_person {
                if displaced != person_id {
                    let n = tx
                        .execute(
                            "INSERT OR IGNORE INTO face_person_rejections
                             (face_id, person_id, created_at) VALUES (?1, ?2, ?3)",
                            params![fid, displaced, now_ms()],
                        )
                        .map_err(|e| e.to_string())?;
                    if n > 0 {
                        rejections.push((fid, displaced));
                    }
                }
            }
            // …and naming someone explicitly clears any older "not them" on
            // this face: the user just overruled it. Without this, correcting
            // a correction ("Not Carson" → name it Carson again) would leave a
            // contradiction that silently blocks every future auto-match.
            tx.execute(
                "DELETE FROM face_person_rejections WHERE face_id = ?1 AND person_id = ?2",
                params![fid, person_id],
            )
            .map_err(|e| e.to_string())?;
            photos.insert(photo_id.clone());
            prior.push((fid, photo_id, p_person, p_by, p_sim));
            affected.push(fid);
        }
        let mut prior_auto_revisions = prior_auto_revisions;
        let mut revisions = HashMap::new();
        for pid in &photos {
            bump_revision(&tx, pid).map_err(|e| e.to_string())?;
            let rev = face_revision(&tx, pid).map_err(|e| e.to_string())?;
            // A photo this naming touched is re-stamped here, so its
            // pre-existing autos stay recognizable across the op's own bump.
            if prior_auto_revisions.contains_key(pid) {
                prior_auto_revisions.insert(pid.clone(), rev);
            }
            revisions.insert(pid.clone(), rev);
        }
        let person = person_row(&tx, person_id, None).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok((
            NamingResult {
                person,
                affected_face_ids: affected,
            },
            NamingOp {
                person_id,
                person_created,
                prior,
                revisions,
                rejections,
                prior_auto,
                prior_auto_revisions,
            },
        ))
    }

    /// Undo a naming op: restore exact prior state, but only for photos whose
    /// face_revision still matches the op (later edits win). Removes exactly
    /// the rejections the op wrote, and the person only if newly created and
    /// now unreferenced. Returns how many faces were restored.
    ///
    /// "Later edits win" now covers **re-detection** as well: a committed scan
    /// bumps `face_revision` (§bump_revision), so a photo whose faces were
    /// re-detected after the naming is ineligible. Without that, the op's face
    /// ids would point at rows that no longer exist — or, worse, at ids SQLite
    /// reused for the replacements — and the undo would silently do nothing
    /// while reporting success.
    pub fn undo_naming(&self, op: &NamingOp) -> rusqlite::Result<usize> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // One eligibility rule for the whole undo: a photo edited after the
        // naming keeps everything about it, assignments AND rejections. Rolling
        // a rejection back on an ineligible photo would erase a "not X" the
        // user re-created since.
        let mut eligible: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (photo_id, &rev) in &op.revisions {
            if face_revision(&tx, photo_id)? == rev {
                eligible.insert(photo_id.as_str());
            }
        }
        let mut restored = 0usize;
        let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (fid, photo_id, p_person, p_by, p_sim) in &op.prior {
            if !eligible.contains(photo_id.as_str()) {
                continue; // this photo was edited after the naming — leave it
            }
            // `photo_id` is part of the match, not decoration: an id SQLite
            // reused for a different photo's row must never be restored into.
            let n = tx.execute(
                "UPDATE faces SET person_id = ?2, assigned_by = ?3, similarity = ?4
                 WHERE id = ?1 AND photo_id = ?5",
                params![fid, p_person, p_by, p_sim, photo_id],
            )?;
            if n > 0 {
                restored += 1;
                touched.insert(photo_id.clone());
            }
        }
        let photo_of: HashMap<i64, &str> = op
            .prior
            .iter()
            .map(|(fid, photo_id, ..)| (*fid, photo_id.as_str()))
            .collect();
        for (fid, pid) in &op.rejections {
            if !photo_of.get(fid).is_some_and(|p| eligible.contains(p)) {
                continue;
            }
            tx.execute(
                "DELETE FROM face_person_rejections WHERE face_id = ?1 AND person_id = ?2",
                params![fid, pid],
            )?;
        }
        // Auto-assignments made BECAUSE of this naming (embed-time matching /
        // the post-naming sweep) go with it — machine state, regenerable, no
        // revision check. The ones this person already had are not ours to
        // touch: naming a face into an existing person must not delete labels
        // that predate the op.
        let now_auto: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, photo_id FROM faces WHERE person_id = ?1 AND assigned_by = 'auto'",
            )?;
            let v: Vec<(i64, String)> = stmt
                .query_map(params![op.person_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        for (fid, photo_id) in &now_auto {
            if op.prior_auto.contains(fid) {
                continue; // predates the naming — not ours to remove
            }
            // A photo that HELD a pre-existing auto and has been re-detected
            // since gives its replacement rows new ids. We can no longer tell
            // "created by this naming" from "carried over", so we leave it: a
            // surviving machine guess is recoverable, a deleted one is not.
            if op
                .prior_auto_revisions
                .get(photo_id)
                .is_some_and(|&rev| face_revision(&tx, photo_id).unwrap_or(rev) != rev)
            {
                continue;
            }
            tx.execute(
                "UPDATE faces SET person_id = NULL, assigned_by = NULL, similarity = NULL
                 WHERE id = ?1 AND assigned_by = 'auto'",
                params![fid],
            )?;
        }
        if op.person_created {
            tx.execute(
                "DELETE FROM persons WHERE id = ?1
                 AND NOT EXISTS (SELECT 1 FROM faces WHERE person_id = ?1)
                 AND NOT EXISTS (SELECT 1 FROM face_person_rejections WHERE person_id = ?1)",
                params![op.person_id],
            )?;
        }
        for pid in &touched {
            bump_revision(&tx, pid)?;
        }
        tx.commit()?;
        Ok(restored)
    }

    /// Reassign (person_id) or clear (None) one face, as an explicit user act.
    /// Displaced identities are rejected so no auto path brings them back.
    pub fn face_assign(&self, face_id: i64, person_id: Option<i64>) -> rusqlite::Result<()> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (photo_id, displaced): (String, Option<i64>) = tx.query_row(
            "SELECT photo_id, person_id FROM faces WHERE id = ?1",
            params![face_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE faces SET person_id = ?2,
                    assigned_by = CASE WHEN ?2 IS NULL THEN NULL ELSE 'user' END,
                    similarity = NULL
             WHERE id = ?1",
            params![face_id, person_id],
        )?;
        if let Some(old) = displaced {
            if Some(old) != person_id {
                tx.execute(
                    "INSERT OR IGNORE INTO face_person_rejections (face_id, person_id, created_at)
                     VALUES (?1, ?2, ?3)",
                    params![face_id, old, now_ms()],
                )?;
            }
        }
        // An explicit user assignment to X overrides a stale "not X" — the
        // user changed their mind; the contradiction must not linger.
        if let Some(new_person) = person_id {
            tx.execute(
                "DELETE FROM face_person_rejections WHERE face_id = ?1 AND person_id = ?2",
                params![face_id, new_person],
            )?;
        }
        bump_revision(&tx, &photo_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Suppress faces entirely — statues, faces inside photographed
    /// photos, strangers the user will never label. Ignored faces leave
    /// clusters, counts, prototypes and every auto path (all filter on
    /// ignored = 0); rows and chips stay so this is cheaply reversible.
    pub fn set_faces_ignored(&self, face_ids: &[i64], ignored: bool) -> rusqlite::Result<usize> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut photos: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut changed = 0usize;
        for &fid in face_ids {
            let photo: Option<String> = tx
                .query_row(
                    "SELECT photo_id FROM faces WHERE id = ?1",
                    params![fid],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(photo_id) = photo else {
                continue; // replaced by a concurrent rescan — nothing to do
            };
            changed += tx.execute(
                "UPDATE faces SET ignored = ?2,
                        person_id = CASE WHEN ?2 THEN NULL ELSE person_id END,
                        assigned_by = CASE WHEN ?2 THEN NULL ELSE assigned_by END,
                        similarity = CASE WHEN ?2 THEN NULL ELSE similarity END
                 WHERE id = ?1",
                params![fid, ignored],
            )?;
            photos.insert(photo_id);
        }
        for pid in &photos {
            bump_revision(&tx, pid)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Durable "not X": no auto path may ever re-pair this face and person.
    /// Clears the assignment if it currently points at that person.
    pub fn face_reject(&self, face_id: i64, person_id: i64) -> rusqlite::Result<()> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let photo_id: String = tx.query_row(
            "SELECT photo_id FROM faces WHERE id = ?1",
            params![face_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO face_person_rejections (face_id, person_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![face_id, person_id, now_ms()],
        )?;
        tx.execute(
            "UPDATE faces SET person_id = NULL, assigned_by = NULL, similarity = NULL
             WHERE id = ?1 AND person_id = ?2",
            params![face_id, person_id],
        )?;
        bump_revision(&tx, &photo_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Inline rename. Renaming onto another person's normalized name is not
    /// an error — it reports the collision so the UI can offer a merge (the
    /// "typo'd the same person twice" case).
    pub fn rename_person(&self, person_id: i64, name: &str) -> Result<RenameOutcome, String> {
        let display = name.trim();
        let norm = display.to_lowercase();
        if norm.is_empty() {
            return Err("name cannot be empty".into());
        }
        let conn = self.lock_conn();
        let clash: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, name FROM persons WHERE name_norm = ?1 AND id <> ?2",
                params![norm, person_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((target_id, target_name)) = clash {
            return Ok(RenameOutcome::Conflict {
                target_id,
                target_name,
            });
        }
        conn.execute(
            "UPDATE persons SET name = ?2, name_norm = ?3 WHERE id = ?1",
            params![person_id, display, norm],
        )
        .map_err(|e| e.to_string())?;
        Ok(RenameOutcome::Renamed)
    }

    /// Merge every face (and rejection) of `source` into `target`, then
    /// remove `source`. An explicit user act: moved faces keep 'user'
    /// provenance, and any "not target" rejection on a moved face is dropped
    /// (the merge asserts they ARE target). Returns moved face count.
    pub fn merge_persons(&self, source_id: i64, target_id: i64) -> rusqlite::Result<usize> {
        if source_id == target_id {
            // Merging a person into themselves moves no face and then deletes
            // the person: never anyone's intent, and unreachable from the UI —
            // but this is the last boundary before the DELETE, so it stops here.
            return Ok(0);
        }
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let photos: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT DISTINCT photo_id FROM faces WHERE person_id = ?1")?;
            let v: Vec<String> = stmt
                .query_map(params![source_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        tx.execute(
            "DELETE FROM face_person_rejections WHERE person_id = ?2
             AND face_id IN (SELECT id FROM faces WHERE person_id = ?1)",
            params![source_id, target_id],
        )?;
        let moved = tx.execute(
            "UPDATE faces SET person_id = ?2 WHERE person_id = ?1",
            params![source_id, target_id],
        )?;
        // Re-key "not source" rejections to the surviving person — except on
        // faces explicitly assigned to the target, where the assignment is
        // the newer truth and a re-keyed rejection would contradict it.
        tx.execute(
            "INSERT OR IGNORE INTO face_person_rejections (face_id, person_id, created_at)
             SELECT face_id, ?2, created_at FROM face_person_rejections
             WHERE person_id = ?1
               AND face_id NOT IN (SELECT id FROM faces WHERE person_id = ?2)",
            params![source_id, target_id],
        )?;
        tx.execute(
            "DELETE FROM face_person_rejections WHERE person_id = ?1",
            params![source_id],
        )?;
        tx.execute("DELETE FROM persons WHERE id = ?1", params![source_id])?;
        for pid in &photos {
            bump_revision(&tx, pid)?;
        }
        tx.commit()?;
        Ok(moved)
    }

    /// All persons (global records) with folder-scoped counts.
    pub fn list_persons(&self, folder_id: i64) -> rusqlite::Result<Vec<PersonOut>> {
        let conn = self.lock_conn();
        let mut stmt = conn.prepare("SELECT id FROM persons ORDER BY name_norm")?;
        let ids: Vec<i64> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        ids.iter()
            .map(|&id| person_row(&conn, id, Some(folder_id)))
            .collect()
    }

    /// A person's visible faces in one folder — the panel's correction list.
    pub fn person_faces(&self, person_id: i64, folder_id: i64) -> rusqlite::Result<Vec<ChipRef>> {
        let conn = self.lock_conn();
        let mut stmt = conn.prepare(
            "SELECT f.photo_id, f.face_index, f.id, s.chip_revision
             FROM faces f JOIN photos p ON p.id = f.photo_id
             JOIN face_scan s ON s.photo_id = f.photo_id
             WHERE f.person_id = ?1 AND f.ignored = 0 AND p.folder_id = ?2 AND p.trashed = 0
             ORDER BY f.det_score DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(params![person_id, folder_id], |r| {
            Ok(ChipRef {
                photo_id: r.get(0)?,
                face_index: r.get(1)?,
                face_id: r.get(2)?,
                revision: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// photo_id → distinct person ids with a visible assigned face (the
    /// person-filter's data source; trashed photos excluded).
    pub fn person_map(&self, folder_id: i64) -> rusqlite::Result<HashMap<String, Vec<i64>>> {
        let conn = self.lock_conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.photo_id, f.person_id
             FROM faces f JOIN photos p ON p.id = f.photo_id
             WHERE p.folder_id = ?1 AND p.trashed = 0
               AND f.person_id IS NOT NULL AND f.ignored = 0",
        )?;
        let mut map: HashMap<String, Vec<i64>> = HashMap::new();
        let rows = stmt.query_map(params![folder_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (photo, person) = row?;
            map.entry(photo).or_default().push(person);
        }
        Ok(map)
    }

    /// None = no successful scan yet (indistinguishable from "still queued");
    /// Some(vec) may be empty for a scanned photo with no faces.
    pub fn faces_for_photo(&self, photo_id: &str) -> rusqlite::Result<Option<Vec<FaceOut>>> {
        let conn = self.lock_conn();
        let ok: Option<String> = conn
            .query_row(
                "SELECT status FROM face_scan WHERE photo_id = ?1",
                params![photo_id],
                |r| r.get(0),
            )
            .optional()?;
        if ok.as_deref() != Some("ok") {
            return Ok(None);
        }
        let mut stmt = conn.prepare(
            "SELECT f.id, f.photo_id, f.face_index, f.x, f.y, f.w, f.h, f.det_score,
                    f.person_id, per.name, f.assigned_by, f.ignored
             FROM faces f LEFT JOIN persons per ON per.id = f.person_id
             WHERE f.photo_id = ?1 ORDER BY f.face_index",
        )?;
        let rows = stmt.query_map(params![photo_id], |r| {
            Ok(FaceOut {
                face_id: r.get(0)?,
                photo_id: r.get(1)?,
                face_index: r.get(2)?,
                rect: [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
                det_score: r.get(7)?,
                person_id: r.get(8)?,
                person_name: r.get(9)?,
                assigned_by: r.get(10)?,
                ignored: r.get::<_, i64>(11)? != 0,
            })
        })?;
        Ok(Some(rows.collect::<Result<_, _>>()?))
    }

    /// Everything the repair queue needs to bake a photo's chips, read as ONE
    /// consistent snapshot: the epoch and scan revision it must publish
    /// against (§publish_chips) plus the rects themselves. Reading them in
    /// separate statements would let a rescan land in between and produce
    /// crops that no guard could then reject. `None` = nothing to repair.
    pub fn face_chip_source(&self, photo_id: &str) -> rusqlite::Result<Option<ChipSource>> {
        let conn = self.lock_conn();
        let Some(revision) = conn
            .query_row(
                "SELECT chip_revision FROM face_scan WHERE photo_id = ?1 AND status = 'ok'",
                params![photo_id],
                |r| r.get(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        Ok(Some(ChipSource {
            epoch: state_get(&conn, "index_epoch")?,
            revision,
            rects: face_rects(&conn, photo_id)?,
        }))
    }

    /// The repair queue's publish step (§publish_chips), on the command
    /// connection: the same write lock a privacy delete needs.
    pub fn publish_face_chips(
        &self,
        cache_dir: &Path,
        photo_id: &str,
        epoch: i64,
        revision: i64,
        chips: &[(PathBuf, Vec<u8>)],
    ) -> Result<PublishOutcome, String> {
        publish_chips(
            &mut self.lock_conn(),
            cache_dir,
            photo_id,
            epoch,
            revision,
            chips,
        )
    }

    /// Pixel change / manual rescan: keep old rows as the carry-over source,
    /// just mark the scan stale so the worker reprocesses. The caller drops
    /// the photo's baked chips (`PreviewState::delete_face_chips`).
    pub fn mark_face_stale(&self, photo_id: &str) -> rusqlite::Result<()> {
        let conn = self.lock_conn();
        conn.execute(
            "UPDATE face_scan SET status = 'stale' WHERE photo_id = ?1",
            params![photo_id],
        )?;
        Ok(())
    }

    /// Throw away every machine guess in this folder, keeping user labels.
    /// The recovery path when recognition has made a mess (bad detection
    /// settings, a poisoned exemplar): the sweep re-derives from scratch
    /// under current settings. Rejections survive — "not X" is user intent.
    pub fn clear_auto_assignments(&self, folder_id: i64) -> rusqlite::Result<usize> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let photos: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT f.photo_id FROM faces f JOIN photos p ON p.id = f.photo_id
                 WHERE p.folder_id = ?1 AND f.assigned_by = 'auto'",
            )?;
            let v: Vec<String> = stmt
                .query_map(params![folder_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        let cleared = tx.execute(
            "UPDATE faces SET person_id = NULL, assigned_by = NULL, similarity = NULL
             WHERE assigned_by = 'auto' AND photo_id IN
               (SELECT id FROM photos WHERE folder_id = ?1)",
            params![folder_id],
        )?;
        for pid in &photos {
            bump_revision(&tx, pid)?;
        }
        tx.commit()?;
        Ok(cleared)
    }

    /// Remove a person entirely: their faces return to Unnamed and every
    /// rejection naming them is dropped. Used by the gate harness for
    /// guaranteed teardown (undo alone can't promise it — undo deliberately
    /// skips rows a later edit touched); also the honest answer to "I created
    /// this person by mistake". Returns freed face count.
    pub fn delete_person(&self, person_id: i64) -> rusqlite::Result<usize> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let photos: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT DISTINCT photo_id FROM faces WHERE person_id = ?1")?;
            let v: Vec<String> = stmt
                .query_map(params![person_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        let freed = tx.execute(
            "UPDATE faces SET person_id = NULL, assigned_by = NULL, similarity = NULL
             WHERE person_id = ?1",
            params![person_id],
        )?;
        tx.execute(
            "DELETE FROM face_person_rejections WHERE person_id = ?1",
            params![person_id],
        )?;
        tx.execute("DELETE FROM persons WHERE id = ?1", params![person_id])?;
        for pid in &photos {
            bump_revision(&tx, pid)?;
        }
        tx.commit()?;
        Ok(freed)
    }

    /// Manual "rescan faces": mark every scanned photo in the folder stale so
    /// the worker re-detects them through the shared carry-over path — names,
    /// ignored flags and rejections survive. Needed after a detection-setting
    /// change, since an already-'ok' photo is never re-examined otherwise.
    /// Returns the photo ids whose chips the caller should drop.
    pub fn rescan_faces(&self, folder_id: i64) -> rusqlite::Result<Vec<String>> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT s.photo_id FROM face_scan s
                 JOIN photos p ON p.id = s.photo_id
                 WHERE p.folder_id = ?1 AND p.trashed = 0",
            )?;
            let v: Vec<String> = stmt
                .query_map(params![folder_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        tx.execute(
            "UPDATE face_scan SET status = 'stale' WHERE photo_id IN
             (SELECT id FROM photos WHERE folder_id = ?1 AND trashed = 0)",
            params![folder_id],
        )?;
        tx.commit()?;
        Ok(rows)
    }

    /// Metadata-only JPEG rewrite (rating/tag XMP): refresh the stat so the
    /// worker doesn't re-index pixels that never changed (round 3).
    pub fn face_scan_refresh_stat(
        &self,
        photo_id: &str,
        mtime: i64,
        size: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.lock_conn();
        conn.execute(
            "UPDATE face_scan SET mtime = ?2, size = ?3 WHERE photo_id = ?1",
            params![photo_id, mtime, size],
        )?;
        Ok(())
    }
}

/// One consistent read of everything a chip bake publishes against.
pub struct ChipSource {
    pub epoch: i64,
    pub revision: i64,
    pub rects: Vec<(i64, [f32; 4])>,
}

/// Is a metadata (rating/tag) rewrite of this photo's file queued or running?
/// The XMP worker changes the JPEG's mtime/size without changing a pixel and
/// refreshes `face_scan`'s stat afterwards; a face worker that claims the
/// photo inside that window re-indexes it for nothing. The queue row is the
/// window — it outlives the exiftool call and the stat refresh (xmp.rs).
pub fn xmp_write_pending(conn: &Connection, photo_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM xmp_queue WHERE photo_id = ?1)",
        params![photo_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v != 0)
}

fn face_rects(conn: &Connection, photo_id: &str) -> rusqlite::Result<Vec<(i64, [f32; 4])>> {
    let mut stmt = conn.prepare(
        "SELECT face_index, x, y, w, h FROM faces WHERE photo_id = ?1 ORDER BY face_index",
    )?;
    let rows = stmt.query_map(params![photo_id], |r| {
        Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]))
    })?;
    rows.collect()
}

pub(crate) fn person_row(
    conn: &Connection,
    person_id: i64,
    folder_id: Option<i64>,
) -> rusqlite::Result<PersonOut> {
    let name: String = conn.query_row(
        "SELECT name FROM persons WHERE id = ?1",
        params![person_id],
        |r| r.get(0),
    )?;
    let folder_count = match folder_id {
        Some(fid) => conn.query_row(
            "SELECT COUNT(*) FROM faces f JOIN photos p ON p.id = f.photo_id
             WHERE f.person_id = ?1 AND f.ignored = 0 AND p.folder_id = ?2
               AND p.trashed = 0 AND p.missing = 0",
            params![person_id, fid],
            |r| r.get(0),
        )?,
        None => conn.query_row(
            "SELECT COUNT(*) FROM faces WHERE person_id = ?1 AND ignored = 0",
            params![person_id],
            |r| r.get(0),
        )?,
    };
    // A folder-scoped row never borrows its representative from a closed
    // folder: that folder's preview may have been evicted, leaving repair no
    // pixels to crop. A stale scan is still eligible because rescan leaves
    // the committed revision and any baked chips in place until replacement.
    // The route owns the final file-existence check and repair path.
    let rep = rep_face(conn, person_id, folder_id)?;
    Ok(PersonOut {
        id: person_id,
        name,
        folder_count,
        rep_photo_id: rep.as_ref().map(|(p, ..)| p.clone()),
        rep_face_index: rep.as_ref().map(|(_, i, _)| *i),
        rep_revision: rep.map(|(.., rev)| rev),
    })
}

/// User-confirmed first, then strongest detection; `folder_id` narrows the
/// pick to one folder's untrashed, on-disk photos. Scan status deliberately
/// does not gate the pick: a stale row retains its committed chip revision
/// while a rescan is in flight.
fn rep_face(
    conn: &Connection,
    person_id: i64,
    folder_id: Option<i64>,
) -> rusqlite::Result<Option<(String, i64, i64)>> {
    conn.query_row(
        "SELECT f.photo_id, f.face_index, s.chip_revision
         FROM faces f
         JOIN face_scan s ON s.photo_id = f.photo_id
         JOIN photos p ON p.id = f.photo_id
         WHERE f.person_id = ?1 AND f.ignored = 0
           AND p.trashed = 0 AND p.missing = 0
           AND (?2 IS NULL OR p.folder_id = ?2)
         ORDER BY (f.assigned_by = 'user') DESC, f.det_score DESC LIMIT 1",
        params![person_id, folder_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::PhotoEntry;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Store + a separate "worker" connection on the same file — mirrors the
    /// two-connection reality (and, in spirit, the two-process gate reality).
    fn setup() -> (Store, Connection, i64, PathBuf) {
        // A per-process counter, NOT a timestamp: tests run in parallel
        // threads and two of them landing in the same microsecond shared one
        // DB file, which surfaced as a flaky "database is locked".
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "emberfaces-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sqlite3");
        let store = Store::new(&path).unwrap();
        let worker = Connection::open(&path).unwrap();
        worker.pragma_update(None, "foreign_keys", "ON").unwrap();
        worker.pragma_update(None, "busy_timeout", 5000).unwrap();
        let folder = store.open_folder("/tmp/faces-x").unwrap();
        let entries: Vec<Arc<PhotoEntry>> = ["p1", "p2", "p3"]
            .iter()
            .map(|id| {
                Arc::new(PhotoEntry {
                    id: (*id).into(),
                    dir: "/tmp/faces-x".into(),
                    stem: (*id).to_uppercase(),
                    jpeg: Some(format!("/tmp/faces-x/{id}.JPG").into()),
                    raf: None,
                    mtime: 1,
                    size: 1,
                })
            })
            .collect();
        store.sync_photos(folder.folder_id, &entries).unwrap();
        (store, worker, folder.folder_id, path)
    }

    /// The worker's in-transaction source guard, satisfied — tests that care
    /// about a changed source pass their own probe instead.
    fn source_ok() -> bool {
        true
    }

    /// A settings.toml mirror that always succeeds, for the tests where the
    /// file is not the subject.
    fn no_mirror(_: bool) -> std::io::Result<()> {
        Ok(())
    }

    fn emb(x: f32, y: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; facedet::EMBED_DIM];
        v[0] = x;
        v[1] = y;
        facedet::l2_normalize(&mut v);
        v
    }

    fn nf(rect: [f32; 4], e: &[f32]) -> NewFace {
        NewFace {
            rect,
            det_score: 0.9,
            embedding: e.to_vec(),
        }
    }

    const R1: [f32; 4] = [0.1, 0.1, 0.2, 0.2];
    const R2: [f32; 4] = [0.6, 0.1, 0.2, 0.2];

    /// Commit two faces on p1 under a fresh gen; returns (gen, face ids).
    fn seed_faces(worker: &mut Connection) -> (i64, Vec<i64>) {
        let gen = ensure_gen(worker, "d1", "r1", 1).unwrap();
        let snap = snapshot(worker, "p1", gen).unwrap();
        let out = commit_scan(
            worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0)), nf(R2, &emb(0.0, 1.0))],
            &[],
            1,
            1,
            &source_ok,
        )
        .unwrap();
        assert!(matches!(out, CommitOutcome::Committed(_)));
        let mut stmt = worker
            .prepare("SELECT id FROM faces WHERE photo_id='p1' ORDER BY face_index")
            .unwrap();
        let ids: Vec<i64> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        (gen, ids)
    }

    /// The upgrade path the plan requires and the fresh-DB test can't cover:
    /// a real v4 journal, with the user's verdicts already in it, gaining the
    /// face schema in place.
    #[test]
    fn migration_v4_to_v5_upgrades_an_existing_db() {
        let (store, _, folder_id, path) = setup();
        store.set_rating(folder_id, "p1", 4).unwrap();
        drop(store);
        // Rewind this DB to v4: no face tables at all.
        {
            let conn = Connection::open(&path).unwrap();
            for t in [
                "face_person_rejections",
                "faces",
                "face_scan",
                "persons",
                "face_model_gens",
                "face_state",
            ] {
                conn.execute(&format!("DROP TABLE {t}"), []).unwrap();
            }
            conn.pragma_update(None, "user_version", 4).unwrap();
        }

        let store = Store::new(&path).unwrap();
        assert_eq!(
            store
                .lock_conn()
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            crate::store::SCHEMA_VERSION
        );
        assert!(store.faces_enabled().unwrap(), "control rows seeded");
        assert_eq!(state_get(&store.lock_conn(), "index_epoch").unwrap(), 1);
        assert!(store.list_persons(folder_id).unwrap().is_empty());
        assert!(store.faces_for_photo("p1").unwrap().is_none());
        // The verdict that was already there is untouched by the migration.
        let rating: i64 = store
            .lock_conn()
            .query_row("SELECT rating FROM photos WHERE id = 'p1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rating, 4);
    }

    #[test]
    fn migration_v5_fresh_and_reopen() {
        let (store, _, _, path) = setup();
        assert!(store.faces_enabled().unwrap(), "seeded enabled");
        drop(store);
        // Reopen: idempotent, control rows intact.
        let store = Store::new(&path).unwrap();
        assert!(store.faces_enabled().unwrap());
        let conn = store.lock_conn();
        assert_eq!(state_get(&conn, "index_epoch").unwrap(), 1);
    }

    /// A DB that reached v5 with data in it gains the v6 columns in place:
    /// existing scans adopt their `face_revision` as chip identity, the
    /// durable `scan_seq` counter is seeded ABOVE every adopted value, and
    /// the legacy (rank 0) owner row is ADOPTED into the matching binary's
    /// rank on that binary's first registration — no epoch bump, no reindex —
    /// after which the equal-rank rule really is enforceable against
    /// different hashes (before adoption, a rank-0 owner read to them as a
    /// strict upgrade: the round-4 review's hole).
    #[test]
    fn migration_v6_ranks_legacy_generations_and_stamps_chip_identity() {
        let (store, mut worker, _, path) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        // A user edit moves face_revision past the commit's value, so the two
        // columns differ in a real library.
        store.face_set_name(&ids[..1], "Alex").unwrap();
        let face_rev = face_revision(&worker, "p1").unwrap();
        drop(store);
        // Rewind the file to its v5 shape (the columns and the counter row
        // did not exist).
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("ALTER TABLE face_scan DROP COLUMN chip_revision", [])
                .unwrap();
            conn.execute("ALTER TABLE face_model_gens DROP COLUMN release", [])
                .unwrap();
            conn.execute("DELETE FROM face_state WHERE key = 'scan_seq'", [])
                .unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
        }

        let store = Store::new(&path).unwrap();
        {
            let conn = store.lock_conn();
            assert_eq!(
                conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                crate::store::SCHEMA_VERSION
            );
            let (chip_rev, stored_face_rev): (i64, i64) = conn
                .query_row(
                    "SELECT chip_revision, face_revision FROM face_scan WHERE photo_id = 'p1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(stored_face_rev, face_rev);
            assert_eq!(
                chip_rev, face_rev,
                "existing scans adopt a chip identity (their old unversioned \
                 files can never match it, so chips rebake on first request)"
            );
            assert_eq!(
                state_get(&conn, "scan_seq").unwrap(),
                face_rev,
                "the counter starts at the highest adopted identity, so no \
                 post-migration commit can re-mint an existing chip name"
            );
            let (owner_gen, .., release) = current_gen(&conn).unwrap().unwrap();
            assert_eq!((owner_gen, release), (gen, 0), "legacy rows rank 0");
            // The binary that built this library routes through register_gen
            // once (claim says so) to adopt the row into its rank…
            assert_eq!(
                claim_state(&conn, "d1", "r1", 1, 1).unwrap(),
                ClaimState::Unregistered
            );
        }
        let epoch_before = state_get(&store.lock_conn(), "index_epoch").unwrap();
        assert_eq!(
            register_gen(&mut worker, "d1", "r1", 1, 1).unwrap(),
            ClaimState::Ready(gen),
            "adoption keeps the SAME generation"
        );
        {
            let conn = store.lock_conn();
            let (owner_gen, .., release) = current_gen(&conn).unwrap().unwrap();
            assert_eq!((owner_gen, release), (gen, 1), "rank promoted in place");
            assert_eq!(
                state_get(&conn, "index_epoch").unwrap(),
                epoch_before,
                "no epoch bump: same embedding space, in-flight work survives"
            );
            let stale: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM face_scan WHERE status = 'stale'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(stale, 0, "…and nothing was stale-marked: no reindex");
            assert_eq!(
                claim_state(&conn, "d1", "r1", 1, 1).unwrap(),
                ClaimState::Ready(gen),
                "steady state after adoption is plain Ready"
            );
            // The hole the adoption closes: an unseen different-hash build at
            // the SAME rank must now park instead of reading rank 0 as a
            // strict upgrade…
            assert_eq!(
                claim_state(&conn, "other-det", "other-rec", 1, 1).unwrap(),
                ClaimState::Park
            );
        }
        assert_eq!(
            register_gen(&mut worker, "other-det", "other-rec", 1, 1).unwrap(),
            ClaimState::Park
        );
        // …while a genuinely newer release still upgrades normally.
        assert_eq!(
            claim_state(&store.lock_conn(), "other-det", "other-rec", 1, 2).unwrap(),
            ClaimState::Unregistered
        );
        // A post-migration detection mints an identity above every adopted
        // one — the counter really did survive-and-lead.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        let out = commit_scan(&mut worker, "p2", &snap, &[], &[], 1, 1, &source_ok).unwrap();
        let CommitOutcome::Committed(next) = out else {
            panic!("{out:?}");
        };
        assert!(next > face_rev, "fresh identity: {next} > {face_rev}");
    }

    /// A database stamped v6 before the counter existed (the round-4
    /// working-tree build) has chip identities on record but no `scan_seq`
    /// row — and skips the v5→v6 seeding on reopen. The idempotent startup
    /// guard must seed it above every identity the DB holds; without it the
    /// next commit restarts at 1 and can re-mint an existing chip name.
    #[test]
    fn a_v6_database_without_the_counter_is_seeded_above_existing_identities() {
        let (store, mut worker, _, path) = setup();
        let (gen, _) = seed_faces(&mut worker);
        {
            let conn = store.lock_conn();
            // Shape the file like a round-4 v6 DB: identities present (one
            // deliberately far above face_revision, proving the seed reads
            // chip_revision itself), counter row absent, version already 6.
            conn.execute(
                "UPDATE face_scan SET chip_revision = 41 WHERE photo_id = 'p1'",
                [],
            )
            .unwrap();
            conn.execute("DELETE FROM face_state WHERE key = 'scan_seq'", [])
                .unwrap();
        }
        drop(store);

        let store = Store::new(&path).unwrap();
        assert_eq!(
            state_get(&store.lock_conn(), "scan_seq").unwrap(),
            41,
            "seeded at the highest identity on record"
        );
        // The next detection mints strictly above it.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        let out = commit_scan(&mut worker, "p2", &snap, &[], &[], 1, 1, &source_ok).unwrap();
        let CommitOutcome::Committed(next) = out else {
            panic!("{out:?}");
        };
        assert!(next > 41, "fresh identity: {next} > 41");
    }

    /// The migration is verify-then-alter: a rerun after a crash between the
    /// ALTERs and the version bump finds both columns already present,
    /// verifies rather than blindly re-ALTERing, and completes — while a real
    /// ALTER failure would propagate and leave the version at 5 for a retry
    /// instead of recording v6 on a swallowed error.
    #[test]
    fn migration_v6_rerun_after_interruption_verifies_and_completes() {
        let (store, _, _, path) = setup();
        drop(store);
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
        }
        let store = Store::new(&path).unwrap();
        let conn = store.lock_conn();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            crate::store::SCHEMA_VERSION
        );
        // Both columns are live and queryable after the rerun.
        conn.query_row(
            "SELECT COALESCE(MAX(chip_revision), 0) FROM face_scan",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(release), 0) FROM face_model_gens",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    }

    /// Audit B1: a person's chip must stay inside the open folder, whose
    /// previews are protected/rebuilt while the panel is visible. A stale
    /// scan keeps its committed revision until the rescan replaces it, so an
    /// already-baked chip remains usable during that transition.
    #[test]
    fn representative_stays_in_open_folder_and_survives_rescan() {
        let (store, mut worker, folder_a, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        let (res, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        let alex = res.person.id;

        // A second folder with one stronger, user-confirmed face of Alex.
        let folder_b = store.open_folder("/tmp/faces-y").unwrap().folder_id;
        let q1 = Arc::new(PhotoEntry {
            id: "q1".into(),
            dir: "/tmp/faces-y".into(),
            stem: "Q1".into(),
            jpeg: Some("/tmp/faces-y/q1.JPG".into()),
            raf: None,
            mtime: 1,
            size: 1,
        });
        store.sync_photos(folder_b, &[q1]).unwrap();
        let snap = snapshot(&worker, "q1", gen).unwrap();
        let out = commit_scan(
            &mut worker,
            "q1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            1,
            1,
            &source_ok,
        )
        .unwrap();
        assert!(matches!(out, CommitOutcome::Committed(_)));
        let q1_face: i64 = worker
            .query_row("SELECT id FROM faces WHERE photo_id='q1'", [], |r| r.get(0))
            .unwrap();
        store.face_assign(q1_face, Some(alex)).unwrap();
        worker
            .execute(
                "UPDATE faces SET det_score = 1.0 WHERE id = ?1",
                params![q1_face],
            )
            .unwrap();

        let row = |folder: i64| {
            store
                .list_persons(folder)
                .unwrap()
                .into_iter()
                .find(|p| p.id == alex)
                .unwrap()
        };
        // Each folder shows a face from its own photos, whatever scores best
        // globally.
        assert_eq!(row(folder_a).rep_photo_id.as_deref(), Some("p1"));
        assert_eq!(row(folder_b).rep_photo_id.as_deref(), Some("q1"));

        // Trashed photos are never the representative, and a folder-scoped
        // row never falls back to a different folder.
        worker
            .execute("UPDATE photos SET trashed = 1 WHERE id = 'q1'", [])
            .unwrap();
        assert_eq!(row(folder_b).folder_count, 0);
        assert_eq!(row(folder_b).rep_photo_id, None);

        // Rescan marks the old scan stale before replacing it. Its committed
        // chip revision remains the right representative during that window.
        worker
            .execute(
                "UPDATE face_scan SET status = 'stale' WHERE photo_id = 'p1'",
                [],
            )
            .unwrap();
        assert_eq!(row(folder_a).rep_photo_id.as_deref(), Some("p1"));

        // Missing photos leave both the count and representative. Reopening
        // a photo in another folder cannot leak it back into this row.
        worker
            .execute("UPDATE photos SET missing = 1 WHERE id = 'p1'", [])
            .unwrap();
        assert_eq!(row(folder_a).folder_count, 0);
        assert_eq!(row(folder_a).rep_photo_id, None);
        worker
            .execute("UPDATE photos SET trashed = 0 WHERE id = 'q1'", [])
            .unwrap();
        assert_eq!(row(folder_a).rep_photo_id, None, "no global fallback");
    }

    #[test]
    fn naming_normalizes_round_trips_and_undoes_exactly() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);

        let (res, op) = store.face_set_name(&ids[..1], " Alex ").unwrap();
        assert_eq!(res.person.name, "Alex", "display name trimmed");
        assert!(op.person_created);
        // Same person via case/space variants.
        let (res2, op2) = store.face_set_name(&ids[1..], "alex").unwrap();
        assert_eq!(
            res2.person.id, res.person.id,
            "name_norm collapses variants"
        );
        assert!(!op2.person_created);
        let persons = store.list_persons(folder_id).unwrap();
        assert_eq!(persons.len(), 1);
        assert_eq!(persons[0].folder_count, 2);
        assert_eq!(persons[0].rep_photo_id.as_deref(), Some("p1"));

        // Undo the second naming: face 2 returns to unnamed; person survives
        // (not created by that op).
        assert_eq!(store.undo_naming(&op2).unwrap(), 1);
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].person_id, Some(res.person.id));
        assert_eq!(faces[1].person_id, None);
        assert_eq!(faces[1].assigned_by, None);

        // The FIRST op is no longer undoable — op2 (and its undo) moved this
        // photo's revision past it. Later edits win; nothing changes.
        assert_eq!(store.undo_naming(&op).unwrap(), 0);
        assert_eq!(store.list_persons(folder_id).unwrap().len(), 1);

        // Straight undo of a single fresh naming DOES remove a person it
        // created once nothing references them anymore.
        let f3: i64 = {
            let conn = store.lock_conn();
            conn.query_row(
                "SELECT id FROM faces WHERE photo_id='p1' AND face_index=1",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let (dani, op3) = store.face_set_name(&[f3], "Dani").unwrap();
        assert!(op3.person_created);
        assert_eq!(store.undo_naming(&op3).unwrap(), 1);
        let persons = store.list_persons(folder_id).unwrap();
        assert!(
            persons.iter().all(|p| p.id != dani.person.id),
            "unreferenced newly-created person removed by undo"
        );
    }

    #[test]
    fn undo_skips_photos_edited_after_the_op() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (res, op) = store.face_set_name(&ids, "Alex").unwrap();
        // A later edit on the same photo bumps its revision…
        store.face_assign(ids[1], None).unwrap();
        // …so undo must leave the whole photo alone (later edits win).
        assert_eq!(store.undo_naming(&op).unwrap(), 0);
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].person_id, Some(res.person.id), "kept");
        assert_eq!(faces[1].person_id, None, "kept the later clearing");
    }

    #[test]
    fn corrections_write_rejections_for_auto_and_manual() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        // Manual mistake corrected by renaming the face to Dani → rejection
        // of Alex recorded even though the displaced assignment was manual.
        let (dani, _) = store.face_set_name(&ids[..1], "Dani").unwrap();
        {
            let conn = store.lock_conn();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM face_person_rejections WHERE face_id=?1 AND person_id=?2",
                    params![ids[0], alex.person.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "displaced manual identity rejected");
        }
        // Simulated auto assignment displaced by face_assign → rejection too.
        {
            let conn = store.lock_conn();
            conn.execute(
                "UPDATE faces SET person_id=?2, assigned_by='auto', similarity=0.5 WHERE id=?1",
                params![ids[1], alex.person.id],
            )
            .unwrap();
        }
        store.face_assign(ids[1], Some(dani.person.id)).unwrap();
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE face_id=?1 AND person_id=?2",
                params![ids[1], alex.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "displaced auto identity rejected");
    }

    #[test]
    fn reject_clears_assignment_and_sticks() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        store.face_reject(ids[0], alex.person.id).unwrap();
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].person_id, None, "\"not Alex\" cleared the face");
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE face_id=?1",
                params![ids[0]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn carry_over_preserves_state_and_rejections_through_rescan() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        let (dani, _) = store.face_set_name(&ids[1..], "Dani").unwrap();
        store.face_reject(ids[1], dani.person.id).unwrap(); // "not Dani" on face 2
        {
            let conn = store.lock_conn();
            conn.execute("UPDATE faces SET ignored=1 WHERE id=?1", params![ids[1]])
                .unwrap();
        }
        store.mark_face_stale("p1").unwrap();

        // Rescan finds the same two faces slightly moved + one brand-new one.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        assert_eq!(snap.old.len(), 2);
        let moved = |r: [f32; 4]| [r[0] + 0.01, r[1], r[2], r[3]];
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[
                nf(moved(R1), &emb(1.0, 0.0)),
                nf(moved(R2), &emb(0.0, 1.0)),
                nf([0.4, 0.6, 0.2, 0.2], &emb(-1.0, 0.0)),
            ],
            &[],
            2,
            2,
            &source_ok,
        )
        .unwrap();
        assert!(matches!(out, CommitOutcome::Committed(_)));

        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces.len(), 3);
        assert_eq!(faces[0].person_id, Some(alex.person.id), "Alex survived");
        assert_eq!(faces[0].assigned_by.as_deref(), Some("user"));
        assert!(faces[1].ignored, "ignored flag survived");
        assert_eq!(faces[2].person_id, None, "new face starts unnamed");
        // The "not Dani" rejection was re-keyed to the replacement face id.
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections r JOIN faces f ON f.id = r.face_id
                 WHERE f.photo_id='p1' AND r.person_id=?1",
                params![dani.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "rejection survived the CASCADE via re-keying");
    }

    #[test]
    fn cross_generation_carry_over_is_geometric_only() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();

        // New model generation: old embeddings must be withheld from the
        // correspondence (and from clustering).
        let gen2 = ensure_gen(&mut worker, "d2", "r2", 1).unwrap();
        let snap = snapshot(&worker, "p1", gen2).unwrap();
        assert!(
            snap.old.iter().all(|f| f.embedding.is_none()),
            "cross-gen snapshot must not expose old embeddings"
        );
        // Same spot, wildly different embedding (new space) → still carried,
        // because correspondence is geometric.
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(-1.0, -1.0))],
            &[],
            3,
            3,
            &source_ok,
        )
        .unwrap();
        assert!(matches!(out, CommitOutcome::Committed(_)));
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].person_id, Some(alex.person.id));

        // Mixed-generation exclusion: clustering only sees current-gen rows.
        let clusters = store.face_clusters(folder_id, 0.4).unwrap().clusters;
        for c in &clusters {
            for chip in &c.chips {
                let g: i64 = store
                    .lock_conn()
                    .query_row(
                        "SELECT model_gen FROM faces WHERE id=?1",
                        params![chip.face_id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(g, gen2, "clustering must only see the current gen");
            }
        }
    }

    #[test]
    fn model_change_stale_marks_old_generation_scans() {
        let (_, mut worker, _, _) = setup();
        let (_, _) = seed_faces(&mut worker);
        assert_eq!(scan_row(&worker, "p1").unwrap().unwrap().status, "ok");
        ensure_gen(&mut worker, "d2", "r2", 1).unwrap();
        assert_eq!(
            scan_row(&worker, "p1").unwrap().unwrap().status,
            "stale",
            "old-gen scans re-enter the queue"
        );
    }

    #[test]
    fn conditional_commit_discards_on_every_guard() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);

        // Guard 1: user edit bumped face_revision mid-flight.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        store.face_set_name(&ids[..1], "Alex").unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
            &source_ok,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded, "user correction wins");
        assert_eq!(
            store.faces_for_photo("p1").unwrap().unwrap().len(),
            2,
            "rollback left the old rows"
        );

        // Guard 2: epoch bump (enable/disable transition).
        let snap = snapshot(&worker, "p1", gen).unwrap();
        store.set_faces_enabled(false, &no_mirror).unwrap();
        store.set_faces_enabled(true, &no_mirror).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
            &source_ok,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded, "epoch change kills commits");

        // Guard 3: disabled at commit time.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        store.set_faces_enabled(false, &no_mirror).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
            &source_ok,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded, "disabled parks all writes");
        store.set_faces_enabled(true, &no_mirror).unwrap();

        // Guard 4: model generation superseded.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        ensure_gen(&mut worker, "d3", "r3", 1).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
            &source_ok,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded, "stale gen never lands");
    }

    #[test]
    fn delete_all_is_durable_and_discards_in_flight() {
        let (store, mut worker, folder_id, path) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        store.face_set_name(&ids[..1], "Alex").unwrap();

        // Worker snapshotted BEFORE the delete → its commit must die.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        store.delete_face_data(&no_mirror).unwrap();
        let out = commit_scan(
            &mut worker,
            "p2",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
            &source_ok,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded);

        // Everything face-derived is gone; control rows survive.
        assert!(!store.faces_enabled().unwrap());
        assert!(store.list_persons(folder_id).unwrap().is_empty());
        assert!(store.faces_for_photo("p1").unwrap().is_none());
        let epoch = state_get(&store.lock_conn(), "index_epoch").unwrap();
        assert!(epoch >= 2, "epoch bumped by the delete");

        // Relaunch: still disabled (the DB remembers, not the process).
        drop(store);
        let store = Store::new(&path).unwrap();
        assert!(!store.faces_enabled().unwrap(), "delete-all sticks");
        // Re-enabling is explicit — and starts from scratch by design.
        assert!(store.set_faces_enabled(true, &no_mirror).unwrap().changed);
        assert!(store.faces_enabled().unwrap());
        assert_eq!(
            store
                .face_scan_status(folder_id, &Default::default())
                .unwrap()
                .scanned,
            0
        );
    }

    #[test]
    fn a_stale_calibration_report_cannot_publish_after_delete_and_reenable() {
        let (store, mut reporter, _, path) = setup();
        let reports = path.parent().unwrap().join("perf-reports");
        std::fs::create_dir_all(&reports).unwrap();
        let report_path = reports.join("faces-calibration-stale.json");
        let wipe_seq = state_get(&reporter, "wipe_seq").unwrap();

        store.set_faces_enabled(false, &no_mirror).unwrap();
        assert_eq!(
            publish_calibration_report(
                &mut reporter,
                wipe_seq,
                &report_path,
                br#"{"name":"Synthetic Person"}"#,
            )
            .unwrap(),
            PublishOutcome::Refused,
            "a disabled face system cannot publish retained personal data"
        );
        store.set_faces_enabled(true, &no_mirror).unwrap();

        store.delete_face_data(&no_mirror).unwrap();
        store.set_faces_enabled(true, &no_mirror).unwrap();
        let outcome = publish_calibration_report(
            &mut reporter,
            wipe_seq,
            &report_path,
            br#"{"name":"Synthetic Person"}"#,
        )
        .unwrap();

        assert_eq!(outcome, PublishOutcome::Refused);
        assert!(
            !report_path.exists(),
            "re-enabling after a delete must not revive the stale report"
        );
    }

    #[test]
    fn a_failed_calibration_report_write_removes_the_final_path() {
        let (_store, mut reporter, _, path) = setup();
        let reports = path.parent().unwrap().join("perf-reports");
        std::fs::create_dir_all(&reports).unwrap();
        let report_path = reports.join("faces-calibration-partial.json");
        let wipe_seq = state_get(&reporter, "wipe_seq").unwrap();

        let error = publish_calibration_report_with(
            &mut reporter,
            wipe_seq,
            &report_path,
            br#"{"name":"Synthetic Person"}"#,
            |path, bytes| {
                std::fs::write(path, bytes)?;
                Err(std::io::Error::other("injected writer failure"))
            },
        )
        .expect_err("an incomplete report write must fail publication");

        assert!(error.contains("injected writer failure"), "{error}");
        assert!(
            !report_path.exists(),
            "a failed write must not leave its final privacy-report path"
        );
    }

    #[test]
    fn a_parked_calibration_publish_finishes_before_delete_sweeps_it() {
        let (store, mut reporter, _, path) = setup();
        let app_data = path.parent().unwrap().to_path_buf();
        let reports = app_data.join("perf-reports");
        std::fs::create_dir_all(&reports).unwrap();
        let report_path = reports.join("faces-calibration-racing.json");
        let wipe_seq = state_get(&reporter, "wipe_seq").unwrap();
        let preview = std::sync::Arc::new(crate::preview::PreviewState::new(
            app_data.join("cache"),
            crate::exposure::BlinkiesCfg::default(),
        ));
        std::fs::create_dir_all(preview.cache_dir()).unwrap();

        let (parked_tx, parked_rx) = crossbeam_channel::bounded::<()>(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded::<()>(1);
        let writer_path = report_path.clone();
        let writer = std::thread::spawn(move || {
            publish_calibration_report_with(
                &mut reporter,
                wipe_seq,
                &writer_path,
                br#"{"name":"Synthetic Person"}"#,
                |path, bytes| {
                    parked_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    std::fs::write(path, bytes)
                },
            )
            .unwrap()
        });
        parked_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("reporter must park while holding SQLite's writer lock");

        let (delete_done_tx, delete_done_rx) = crossbeam_channel::bounded::<()>(1);
        let delete_app_data = app_data.clone();
        let deleter = std::thread::spawn(move || {
            store.delete_face_data(&no_mirror).unwrap();
            let sweep_wipe_seq = store.face_artifact_wipe_seq().unwrap();
            crate::faces::purge_face_artifacts(&preview, &delete_app_data).unwrap();
            assert!(store
                .clear_chip_sweep_pending_if_wipe_seq(sweep_wipe_seq)
                .unwrap());
            delete_done_tx.send(()).unwrap();
            store
        });
        assert!(
            delete_done_rx
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_err(),
            "delete completed while report publication held the writer lock"
        );

        release_tx.send(()).unwrap();
        assert_eq!(writer.join().unwrap(), PublishOutcome::Published);
        let store = deleter.join().unwrap();
        assert!(!report_path.exists(), "the later delete sweep must win");
        assert!(!store.chip_sweep_pending().unwrap());
    }

    #[test]
    fn a_parked_old_artifact_sweep_cannot_clear_a_newer_delete_debt() {
        let (store_a, _worker, _, path) = setup();
        store_a.delete_face_data(&no_mirror).unwrap();
        let store_b = Store::new(&path).unwrap();
        let app_data = path.parent().unwrap().to_path_buf();
        let reports = app_data.join("perf-reports");
        std::fs::create_dir_all(&reports).unwrap();
        let old_report = reports.join("faces-calibration-old-sweep.json");
        std::fs::write(&old_report, b"old private report").unwrap();
        let preview = crate::preview::PreviewState::new(
            app_data.join("cache"),
            crate::exposure::BlinkiesCfg::default(),
        );
        std::fs::create_dir_all(preview.cache_dir()).unwrap();

        let (parked_tx, parked_rx) = crossbeam_channel::bounded::<i64>(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded::<()>(1);
        let old_sweeper = std::thread::spawn(move || {
            let old_wipe_seq = store_a.face_artifact_wipe_seq().unwrap();
            crate::faces::purge_face_artifacts(&preview, &app_data).unwrap();
            parked_tx.send(old_wipe_seq).unwrap();
            release_rx.recv().unwrap();
            store_a
                .clear_chip_sweep_pending_if_wipe_seq(old_wipe_seq)
                .unwrap()
        });
        let old_wipe_seq = parked_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the old sweep must park after capturing its wipe generation");
        assert!(!old_report.exists(), "the old filesystem sweep completed");

        store_b.delete_face_data(&no_mirror).unwrap();
        assert!(
            store_b.face_artifact_wipe_seq().unwrap() > old_wipe_seq,
            "the second connection committed a newer privacy delete"
        );
        release_tx.send(()).unwrap();

        assert!(
            !old_sweeper.join().unwrap(),
            "an old sweep must not claim a newer delete's artifact debt"
        );
        assert!(
            store_b.chip_sweep_pending().unwrap(),
            "the newer delete must remain owed after the old sweep finishes"
        );
    }

    /// A delete whose settings.toml mirror never landed must still be a delete
    /// after the next launch. The DB says so, and the file — which still reads
    /// `enabled = true` — is not allowed to win.
    #[test]
    fn delete_all_survives_a_failed_settings_mirror() {
        let (store, mut worker, _, path) = setup();
        seed_faces(&mut worker);
        // The app tries to write settings.toml and FAILS (read-only disk, a
        // full volume). The file is left saying `enabled = true`.
        let outcome = store
            .delete_face_data(&|_| Err(std::io::Error::other("read-only file system")))
            .unwrap();
        assert!(outcome.mirror_error.is_some(), "the failure is reported");

        drop(store);
        let store = Store::new(&path).unwrap();
        // Next launch, with that stale `enabled = true` still in the file.
        let sync = store
            .sync_faces_enabled_from_settings(&|| true, &no_mirror)
            .unwrap();
        assert!(sync.mirror_error.is_none());
        assert!(
            !store.faces_enabled().unwrap(),
            "a completed privacy delete can never relaunch enabled"
        );
        // That launch rewrote the file from the DB, so the mirror is trusted
        // again and a genuine hand-edit is honored once more.
        assert!(
            store
                .sync_faces_enabled_from_settings(&|| true, &no_mirror)
                .unwrap()
                .changed
        );
        assert!(store.faces_enabled().unwrap());
    }

    /// **Blocker 1, as the follow-up review described the interleaving.**
    ///
    /// Process A starts `set_faces_enabled(true)` and is parked between its
    /// two phases (the phase boundary is the seam; the phases themselves are
    /// each writer-lock critical sections, and the lock-contention half of
    /// the argument is exercised by `an_enable_blocks_while_a_delete_holds_
    /// the_writer_lock` below). Process B — a second `Store` on the same DB
    /// file, its own connection and thread — then runs the whole privacy
    /// delete and finishes. A is released and completes.
    ///
    /// The mirror is the REAL `settings.toml`, written through the production
    /// writer and read back through the production loader, so the file the
    /// relaunch adopts is exactly what the two processes fought over. What
    /// must be impossible: that file ending up saying `enabled = true` and
    /// being trusted after the later delete.
    #[test]
    fn a_stale_enable_cannot_survive_a_privacy_delete_that_lands_first() {
        let (store_a, mut worker, folder_id, path) = setup();
        seed_faces(&mut worker);
        let store_b = Store::new(&path).unwrap(); // "the other Ember process"
        let dir = path.parent().unwrap().to_path_buf();
        crate::settings::write_faces_enabled(&dir, true).unwrap();
        let mirror = |dir: PathBuf| {
            move |v: bool| -> std::io::Result<()> { crate::settings::write_faces_enabled(&dir, v) }
        };
        let file_says = |dir: &std::path::Path| crate::settings::load(dir).faces.enabled;

        // Phase 1 of A's enable: the mirror is now durably untrusted, and A
        // has captured the wipe counter it was deciding against.
        let a_seq = store_a.begin_enabled_intent().unwrap();

        // …B's privacy delete runs to completion while A is parked.
        let b_done = {
            let store_b = Store::new(&path).unwrap();
            let m = mirror(dir.clone());
            std::thread::spawn(move || store_b.delete_face_data(&m).unwrap())
        };
        let b = b_done.join().unwrap();
        assert!(b.mirror_error.is_none());
        assert!(!file_says(&dir), "B wrote enabled = false");
        assert!(!store_b.faces_enabled().unwrap());

        // …and only now does A resume and complete its phase 2.
        let a = store_a
            .commit_enabled_intent(true, false, a_seq, &mirror(dir.clone()))
            .unwrap();
        assert!(
            a.superseded,
            "an enable decided before a delete that has since landed must yield"
        );
        assert!(
            !file_says(&dir),
            "the stale writer may not leave `enabled = true` in the file"
        );
        assert!(!store_a.faces_enabled().unwrap(), "the DB stayed disabled");

        // The state the next launch actually sees, from a fresh Store, with
        // settings.toml exactly as the two processes left it.
        drop(store_a);
        drop(store_b);
        let relaunch = Store::new(&path).unwrap();
        relaunch
            .sync_faces_enabled_from_settings(&|| file_says(&dir), &mirror(dir.clone()))
            .unwrap();
        assert!(
            !relaunch.faces_enabled().unwrap(),
            "delete-all stayed deleted across the relaunch"
        );
        assert!(
            relaunch
                .face_scan_status(folder_id, &Default::default())
                .unwrap()
                .scanned
                == 0
        );
    }

    /// The other half of the ordering argument: phase 2s themselves cannot
    /// overlap, because each holds SQLite's single cross-process writer lock
    /// across its DB change AND its settings.toml write. B's delete is parked
    /// INSIDE its critical section (the mirror closure runs under the lock);
    /// A's enable, started then, must not complete until B commits — and must
    /// then yield to the delete it was concurrent with.
    #[test]
    fn an_enable_blocks_while_a_delete_holds_the_writer_lock() {
        let (store_a, mut worker, _, path) = setup();
        seed_faces(&mut worker);
        let dir = path.parent().unwrap().to_path_buf();
        crate::settings::write_faces_enabled(&dir, true).unwrap();

        // A's phase 1 runs BEFORE B takes the lock (phase 1 needs it too).
        let a_seq = store_a.begin_enabled_intent().unwrap();

        let (reached_tx, reached_rx) = crossbeam_channel::bounded::<()>(1);
        let (go_tx, go_rx) = crossbeam_channel::bounded::<()>(1);
        let b_thread = {
            let store_b = Store::new(&path).unwrap();
            let dir = dir.clone();
            std::thread::spawn(move || {
                store_b
                    .delete_face_data(&|v| {
                        crate::settings::write_faces_enabled(&dir, v)?;
                        reached_tx.send(()).unwrap(); // parked mid-critical-section
                        let _ = go_rx.recv();
                        Ok(())
                    })
                    .unwrap()
            })
        };
        reached_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("B must reach its mirror write");

        // A's phase 2 now contends for the writer lock B is holding.
        let (a_done_tx, a_done_rx) = crossbeam_channel::bounded(1);
        let a_thread = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let out = store_a
                    .commit_enabled_intent(true, false, a_seq, &|v| {
                        crate::settings::write_faces_enabled(&dir, v)
                    })
                    .unwrap();
                a_done_tx.send(()).unwrap();
                (store_a, out)
            })
        };
        // Best-effort liveness probe (the hard guarantee is the final state):
        // while B holds the lock, A should not have completed.
        assert!(
            a_done_rx
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_err(),
            "A completed while B held the writer lock"
        );
        go_tx.send(()).unwrap(); // release B; its transaction commits
        let b = b_thread.join().unwrap();
        assert!(b.changed && !b.superseded);
        let (store_a, a) = a_thread.join().unwrap();
        assert!(a.superseded, "A ran strictly after B, and yielded to it");
        assert!(!store_a.faces_enabled().unwrap());
        assert!(
            !crate::settings::load(&dir).faces.enabled,
            "the file agrees with the DB the last critical section left"
        );
    }

    /// The mirror-failure half of the same protocol: A is parked between its
    /// phases, B deletes, and A's completion FAILS to write the file (so the
    /// file keeps whatever B left). The mirror must not be marked trusted by
    /// that failed completion, and the DB must still decide at launch.
    #[test]
    fn a_stale_writer_that_cannot_write_the_file_leaves_the_mirror_untrusted() {
        let (store_a, mut worker, _, path) = setup();
        seed_faces(&mut worker);
        let store_b = Store::new(&path).unwrap();

        let a_seq = store_a.begin_enabled_intent().unwrap();
        store_b
            .delete_face_data(&|_| Err(std::io::Error::other("disk full")))
            .unwrap();
        let a = store_a
            .commit_enabled_intent(true, false, a_seq, &|_| {
                Err(std::io::Error::other("disk full"))
            })
            .unwrap();
        assert!(a.superseded);
        assert!(a.mirror_error.is_some());

        drop(store_a);
        drop(store_b);
        let relaunch = Store::new(&path).unwrap();
        // The file still reads `enabled = true` from before everything.
        relaunch
            .sync_faces_enabled_from_settings(&|| true, &no_mirror)
            .unwrap();
        assert!(
            !relaunch.faces_enabled().unwrap(),
            "an untrusted mirror can never re-enable indexing"
        );
    }

    /// A superseded writer must not decide the value either. If someone
    /// legitimately re-enabled indexing after the delete, the parked writer
    /// completing must leave the file agreeing with THAT — writing its own
    /// stale answer would put a divergent file behind a trusted mirror flag.
    #[test]
    fn a_superseded_writer_mirrors_the_db_it_found_not_its_own_intent() {
        let (store_a, mut worker, _, path) = setup();
        seed_faces(&mut worker);
        let store_b = Store::new(&path).unwrap();
        let file = Arc::new(Mutex::new(true));
        let writer = |f: Arc<Mutex<bool>>| {
            move |v: bool| -> std::io::Result<()> {
                *f.lock().unwrap() = v;
                Ok(())
            }
        };

        let a_seq = store_a.begin_enabled_intent().unwrap();
        store_b.delete_face_data(&writer(file.clone())).unwrap();
        // …and the user then deliberately turns indexing back on.
        store_b
            .set_faces_enabled(true, &writer(file.clone()))
            .unwrap();
        assert!(*file.lock().unwrap());

        let a = store_a
            .commit_enabled_intent(true, false, a_seq, &writer(file.clone()))
            .unwrap();
        assert!(a.superseded);
        assert_eq!(
            *file.lock().unwrap(),
            store_a.faces_enabled().unwrap(),
            "file and DB agree whatever the parked writer intended"
        );
        assert!(store_a.faces_enabled().unwrap(), "the later act stands");
    }

    /// A DB that reached v5 before this control row existed reads it as
    /// "mirror is fine" — an upgrade must not park indexing on the DB value.
    #[test]
    fn a_missing_mirror_row_defaults_to_trusting_the_file() {
        let (store, _, _, _) = setup();
        store
            .lock_conn()
            .execute("DELETE FROM face_state WHERE key = 'enabled_mirror_ok'", [])
            .unwrap();
        store
            .sync_faces_enabled_from_settings(&|| false, &no_mirror)
            .unwrap();
        assert!(!store.faces_enabled().unwrap(), "the file was adopted");
    }

    /// The expected chip artifacts of one photo at its current revision, in
    /// the test dir standing in for the cache.
    fn expected_chips(
        store: &Store,
        dir: &std::path::Path,
        photo: &str,
    ) -> (i64, i64, Vec<(PathBuf, Vec<u8>)>) {
        let src = store.face_chip_source(photo).unwrap().unwrap();
        let chips = src
            .rects
            .iter()
            .map(|(n, _)| {
                (
                    dir.join(crate::preview::face_chip_name(photo, *n, src.revision)),
                    b"chip".to_vec(),
                )
            })
            .collect();
        (src.epoch, src.revision, chips)
    }

    /// Chip publishing is ordered against Delete-all by SQLite's write lock,
    /// so it holds across processes — here, a second connection standing in
    /// for the other Ember instance's worker. (The threaded, barrier-driven
    /// version of this seam lives in `faces::tests`.)
    #[test]
    fn chip_publish_is_refused_once_a_delete_has_landed() {
        let (store, mut worker, _, path) = setup();
        seed_faces(&mut worker);
        let dir = path.parent().unwrap().to_path_buf();
        let (epoch, revision, chips) = expected_chips(&store, &dir, "p1");

        // Delete-all lands between encoding and publishing.
        store.delete_face_data(&no_mirror).unwrap();
        assert_eq!(
            publish_chips(&mut worker, &dir, "p1", epoch, revision, &chips).unwrap(),
            PublishOutcome::Refused,
            "no chip may appear after a privacy delete"
        );
        assert!(chips.iter().all(|(out, _)| !out.exists()));

        // The same publish before the delete does land (and the delete's own
        // sweep is what removes it — see the faces:: chip tests).
        let (store2, mut worker2, _, path2) = setup();
        seed_faces(&mut worker2);
        let dir2 = path2.parent().unwrap().to_path_buf();
        let (epoch2, revision2, ok) = expected_chips(&store2, &dir2, "p1");
        assert_eq!(
            publish_chips(&mut worker2, &dir2, "p1", epoch2, revision2, &ok).unwrap(),
            PublishOutcome::Published
        );
        assert!(ok.iter().all(|(out, _)| out.exists()));
        // A moved epoch (enable/disable, a new model gen) also refuses — the
        // already-published files are removed first so a refusal that wrote
        // nothing is observable.
        store2.set_faces_enabled(false, &no_mirror).unwrap();
        for (out, _) in &ok {
            std::fs::remove_file(out).unwrap();
        }
        assert_eq!(
            publish_chips(&mut worker2, &dir2, "p1", epoch2, revision2, &ok).unwrap(),
            PublishOutcome::Refused
        );
        assert!(ok.iter().all(|(out, _)| !out.exists()));
    }

    /// A publish that cannot write every chip reports the failure and leaves
    /// no partial set behind — a half-published photo must not read as done.
    #[test]
    fn a_failed_chip_write_is_reported_and_rolled_back() {
        let (store, mut worker, _, path) = setup();
        seed_faces(&mut worker);
        let dir = path.parent().unwrap().to_path_buf();
        let (epoch, revision, chips) = expected_chips(&store, &dir, "p1");
        // A directory sits where the SECOND chip must land, so its rename
        // fails after the first one has already landed.
        std::fs::create_dir(&chips[1].0).unwrap();
        let err = publish_chips(&mut worker, &dir, "p1", epoch, revision, &chips)
            .expect_err("a failed chip write must not be swallowed");
        assert!(err.contains("-f1r"), "{err}");
        assert!(
            !chips[0].0.exists(),
            "the chips that did land are rolled back"
        );
        // …and a path outside the revision's artifact set is refused outright.
        std::fs::remove_dir(&chips[1].0).unwrap();
        let foreign = vec![(dir.join("chip0.jpg"), b"chip".to_vec())];
        let err = publish_chips(&mut worker, &dir, "p1", epoch, revision, &foreign)
            .expect_err("an off-contract path must never be written");
        assert!(err.contains("not an artifact"), "{err}");
        assert!(!foreign[0].0.exists());
    }

    /// The compatibility rule covers EVERY embedding computation: a photo
    /// whose scan is stale or errored contributes nothing until it is
    /// re-scanned, prototypes included.
    #[test]
    fn embedding_reads_ignore_photos_without_a_successful_scan() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        store.face_set_name(&ids[..1], "Alex").unwrap();
        assert_eq!(person_prototypes(&worker, gen, 5).unwrap().len(), 1);

        // A pixel change marks p1 stale — its confirmed face is no longer
        // evidence about pixels we have actually seen.
        store.mark_face_stale("p1").unwrap();
        assert!(
            person_prototypes(&worker, gen, 5).unwrap().is_empty(),
            "a stale scan must not supply prototypes"
        );
        assert_eq!(
            calibration_data(&worker, gen).unwrap()["summary"]["positives"],
            serde_json::json!(0),
            "nor calibration positives"
        );
        // A failed scan is excluded the same way, and the sweep sees nothing
        // to work with either.
        record_scan_error(&worker, "p2", gen, "boom").unwrap();
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        assert_eq!(
            sweep_photo(&mut worker, "p2", gen, &protos, 0.0, 0.0).unwrap(),
            0
        );
    }

    /// The source guard belongs INSIDE the write transaction: a display file
    /// rewritten while we inferred must not leave rows about gone pixels.
    #[test]
    fn commit_scan_discards_when_the_source_changed_under_it() {
        let (store, mut worker, _, _) = setup();
        let (gen, _) = seed_faces(&mut worker);
        let snap = snapshot(&worker, "p1", gen).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            7,
            7,
            &|| false, // the file moved on between inference and here
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded);
        assert_eq!(
            store.faces_for_photo("p1").unwrap().unwrap().len(),
            2,
            "the old rows survive the discard"
        );
    }

    /// Delete-then-insert has to be rollback-safe: a failing insert must leave
    /// the photo exactly as it was, not faceless.
    #[test]
    fn a_failed_insert_rolls_the_whole_commit_back() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        let snap = snapshot(&worker, "p1", gen).unwrap();
        // A proposal naming a person who doesn't exist trips the foreign key
        // on insert, after the DELETE has already run inside the transaction.
        let err = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0)), nf(R2, &emb(0.0, 1.0))],
            &[None, Some((alex.person.id + 999, 0.9))],
            5,
            5,
            &source_ok,
        );
        assert!(err.is_err(), "the bad insert must fail, not be swallowed");
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces.len(), 2, "rolled back to the pre-commit rows");
        assert_eq!(faces[0].person_id, Some(alex.person.id), "name intact");
    }

    #[test]
    fn xmp_rewrite_refreshes_stat_without_rescan() {
        let (store, mut worker, _, _) = setup();
        let (_, _) = seed_faces(&mut worker);
        // Rating rewrite changed the file stat (5, 5): refresh keeps status ok
        // and stores the new stat, so job selection sees nothing to do.
        store.face_scan_refresh_stat("p1", 5, 5).unwrap();
        let row = scan_row(&worker, "p1").unwrap().unwrap();
        assert_eq!((row.mtime, row.size, row.status.as_str()), (5, 5, "ok"));
    }

    #[test]
    fn clusters_recurring_only_and_trashed_excluded() {
        let (store, mut worker, folder_id, _) = setup();
        let gen = ensure_gen(&mut worker, "d1", "r1", 1).unwrap();
        // p1: two similar faces; p2: one matching face; p3: a loner.
        for (photo, faces) in [
            ("p1", vec![nf(R1, &emb(1.0, 0.05)), nf(R2, &emb(0.0, 1.0))]),
            ("p2", vec![nf(R1, &emb(1.0, 0.0))]),
            ("p3", vec![nf(R1, &emb(-1.0, 0.3))]),
        ] {
            let snap = snapshot(&worker, photo, gen).unwrap();
            commit_scan(&mut worker, photo, &snap, &faces, &[], 1, 1, &source_ok).unwrap();
        }
        let out = store.face_clusters(folder_id, 0.9).unwrap();
        assert_eq!(out.clusters.len(), 1, "only the recurring face clusters");
        assert_eq!(out.clusters[0].size, 2);
        assert_eq!(out.clusters[0].photo_count, 2);
        assert_eq!(
            out.loose.len(),
            2,
            "the two non-recurring faces surface as loose singles"
        );

        // Trash p2 → its face leaves clusters and counts.
        let payload = crate::store::TrashPayload::default();
        store.record_trash(folder_id, "p2", &payload).unwrap();
        let out = store.face_clusters(folder_id, 0.9).unwrap();
        assert!(out.clusters.is_empty(), "trashed photos leave the panel");

        // person_map excludes trashed too.
        let ids: Vec<i64> = {
            let conn = store.lock_conn();
            let mut stmt = conn
                .prepare("SELECT id FROM faces WHERE photo_id IN ('p1','p2') ORDER BY photo_id")
                .unwrap();
            let v: Vec<i64> = stmt
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            v
        };
        store.face_set_name(&ids, "Alex").unwrap();
        let map = store.person_map(folder_id).unwrap();
        assert!(map.contains_key("p1"));
        assert!(!map.contains_key("p2"), "trashed photo not in person_map");
    }

    // ---------- Slice B: auto-recognition ----------

    /// Seed p1 with two named faces (Alex at e(1,0), Dani at e(0,1)) and
    /// p2/p3 with unassigned faces; returns (gen, alex_id, dani_id).
    fn seed_recognition(store: &Store, worker: &mut Connection) -> (i64, i64, i64) {
        let (gen, ids) = seed_faces(worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        let (dani, _) = store.face_set_name(&ids[1..], "Dani").unwrap();
        // p2: a face nearly identical to Alex; p3: an ambiguous face.
        for (photo, e) in [("p2", emb(0.98, 0.05)), ("p3", emb(1.0, 1.0))] {
            let snap = snapshot(worker, photo, gen).unwrap();
            commit_scan(worker, photo, &snap, &[nf(R1, &e)], &[], 1, 1, &source_ok).unwrap();
        }
        (gen, alex.person.id, dani.person.id)
    }

    #[test]
    fn sweep_assigns_confident_skips_ambiguous_and_rejected() {
        let (store, mut worker, _, _) = setup();
        let (gen, alex, _) = seed_recognition(&store, &mut worker);
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        assert_eq!(protos.len(), 2);

        // p2 (clear Alex lookalike) assigns; p3 (equidistant) must not.
        let n = sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        assert_eq!(n, 1);
        let f = &store.faces_for_photo("p2").unwrap().unwrap()[0];
        assert_eq!(f.person_id, Some(alex));
        assert_eq!(f.assigned_by.as_deref(), Some("auto"));
        let n = sweep_photo(&mut worker, "p3", gen, &protos, 0.40, 0.05).unwrap();
        assert_eq!(n, 0, "ambiguous face stays for the human");

        // "not Alex" on p2's face, then re-sweep: rejection is absolute.
        let fid = f.face_id;
        store.face_reject(fid, alex).unwrap();
        let n = sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        assert_eq!(n, 0, "\"not X\" survives the sweep, forever");
        let f = &store.faces_for_photo("p2").unwrap().unwrap()[0];
        assert_eq!(f.person_id, None);

        // Disabled indexing parks the sweep too.
        store.set_faces_enabled(false, &no_mirror).unwrap();
        let n = sweep_photo(&mut worker, "p3", gen, &protos, 0.0, 0.0).unwrap();
        assert_eq!(n, 0, "sweep writes nothing while disabled");
    }

    #[test]
    fn auto_assignments_never_train_prototypes() {
        let (store, mut worker, _, _) = setup();
        let (gen, alex, _) = seed_recognition(&store, &mut worker);
        let before: usize = person_prototypes(&worker, gen, 5)
            .unwrap()
            .iter()
            .find(|p| p.person_id == alex)
            .unwrap()
            .exemplars
            .len();
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        let after: usize = person_prototypes(&worker, gen, 5)
            .unwrap()
            .iter()
            .find(|p| p.person_id == alex)
            .unwrap()
            .exemplars
            .len();
        assert_eq!(
            before, after,
            "an auto face must never move anyone's anchor — no cascade"
        );
    }

    #[test]
    fn embed_time_proposals_fill_only_uncarried_faces() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        store.mark_face_stale("p1").unwrap();

        // Rescan: face 1 carries Alex (user); face 2 carries nothing and has
        // a proposal; a brand-new face 3 has a proposal too.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[
                nf(R1, &emb(1.0, 0.0)),
                nf(R2, &emb(0.0, 1.0)),
                nf([0.4, 0.6, 0.2, 0.2], &emb(0.9, 0.1)),
            ],
            &[
                Some((alex.person.id, 0.99)), // must be IGNORED (carried user state wins)
                None,
                Some((alex.person.id, 0.61)),
            ],
            2,
            2,
            &source_ok,
        )
        .unwrap();
        assert!(matches!(out, CommitOutcome::Committed(_)));
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].assigned_by.as_deref(), Some("user"), "carried");
        assert_eq!(faces[1].person_id, None);
        assert_eq!(faces[2].person_id, Some(alex.person.id));
        assert_eq!(faces[2].assigned_by.as_deref(), Some("auto"));
    }

    #[test]
    fn undo_naming_clears_the_sweeps_auto_assignments() {
        let (store, mut worker, folder_id, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        // p2 gets an unassigned Alex-lookalike BEFORE naming.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        commit_scan(
            &mut worker,
            "p2",
            &snap,
            &[nf(R1, &emb(0.98, 0.05))],
            &[],
            1,
            1,
            &source_ok,
        )
        .unwrap();

        let (_, op) = store.face_set_name(&ids[..1], "Alex").unwrap();
        // The post-naming sweep auto-assigns p2's face.
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        assert!(store.faces_for_photo("p2").unwrap().unwrap()[0]
            .person_id
            .is_some());

        // Undo the naming: the confirmed face reverts AND every auto
        // assignment that existed only because of it is cleared; the
        // newly-created person leaves with them.
        assert_eq!(store.undo_naming(&op).unwrap(), 1);
        assert!(store.faces_for_photo("p2").unwrap().unwrap()[0]
            .person_id
            .is_none());
        assert!(store.list_persons(folder_id).unwrap().is_empty());
    }

    /// Undo restores the op's own work and nothing else: auto-labels that
    /// already pointed at this person predate the naming and must survive it.
    #[test]
    fn undo_keeps_auto_labels_that_predate_the_op() {
        let (store, mut worker, folder_id, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        for photo in ["p2", "p3"] {
            let snap = snapshot(&worker, photo, gen).unwrap();
            commit_scan(
                &mut worker,
                photo,
                &snap,
                &[nf(R1, &emb(0.98, 0.05))],
                &[],
                1,
                1,
                &source_ok,
            )
            .unwrap();
        }
        // Naming Alex once, then the sweep auto-labels p2.
        store.face_set_name(&ids[..1], "Alex").unwrap();
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        let p2_face = store.faces_for_photo("p2").unwrap().unwrap()[0].clone();
        assert_eq!(p2_face.assigned_by.as_deref(), Some("auto"));

        // A SECOND naming into the same, existing person.
        let p3_face = store.faces_for_photo("p3").unwrap().unwrap()[0].face_id;
        let (_, op2) = store.face_set_name(&[p3_face], "Alex").unwrap();
        assert!(!op2.person_created);

        assert_eq!(store.undo_naming(&op2).unwrap(), 1);
        assert_eq!(
            store.faces_for_photo("p3").unwrap().unwrap()[0].person_id,
            None,
            "the op's own face reverts"
        );
        assert_eq!(
            store.faces_for_photo("p2").unwrap().unwrap()[0].person_id,
            p2_face.person_id,
            "an auto-label that predates the op is not the op's to erase"
        );
        assert_eq!(store.list_persons(folder_id).unwrap().len(), 1);
    }

    /// Rejection rollback follows the same eligibility rule as assignment
    /// restoration — otherwise undo erases a "not X" the user re-created after
    /// the naming, and the identity it barred comes back.
    #[test]
    fn undo_leaves_a_rejection_recreated_after_the_op() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        // Correcting to Dani records "not Alex" on that face — this is the op
        // we will undo.
        let (_, op) = store.face_set_name(&ids[..1], "Dani").unwrap();
        assert_eq!(op.rejections, vec![(ids[0], alex.person.id)]);

        // Later edits: the user re-assigns to Alex (retracting the rejection),
        // then rejects Alex again. Both bump p1's revision.
        store.face_assign(ids[0], Some(alex.person.id)).unwrap();
        store.face_reject(ids[0], alex.person.id).unwrap();

        assert_eq!(store.undo_naming(&op).unwrap(), 0, "later edits win");
        let n: i64 = store
            .lock_conn()
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections
                 WHERE face_id = ?1 AND person_id = ?2",
                params![ids[0], alex.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "the rejection the user just re-created must survive");
    }

    /// A pending undo is keyed by face row ids, and a re-detection replaces
    /// those rows. The op must become ineligible rather than quietly restoring
    /// nothing (or, worse, writing into ids SQLite reused) — and the naming's
    /// effects on a photo the worker has since re-detected stay put, because
    /// the carry-over already moved them onto the new rows.
    #[test]
    fn undo_is_ineligible_for_a_photo_re_detected_since_the_naming() {
        let (store, mut worker, folder_id, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        let (alex, op) = store.face_set_name(&ids[..1], "Alex").unwrap();

        // The worker re-detects p1 at the same rects: carry-over keeps the
        // name, but every face row is a NEW row with a new id.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0)), nf(R2, &emb(0.0, 1.0))],
            &[],
            1,
            1,
            &source_ok,
        )
        .unwrap();
        assert!(matches!(out, CommitOutcome::Committed(_)));
        let after = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(
            after[0].person_id,
            Some(alex.person.id),
            "carry-over kept the name"
        );
        // The replacement row may even carry the SAME id: SQLite hands out
        // max(rowid)+1, and the rows we just deleted were the maximum. That is
        // precisely why the eligibility guard is the photo's revision and not
        // "do these face ids still exist" — an id check can be satisfied by a
        // row the op never saw.
        assert_ne!(
            face_revision(&worker, "p1").unwrap(),
            op.revisions["p1"],
            "the re-detection moved the photo's identity"
        );

        assert_eq!(
            store.undo_naming(&op).unwrap(),
            0,
            "a re-detected photo is not the photo the op recorded"
        );
        assert_eq!(
            store.faces_for_photo("p1").unwrap().unwrap()[0].person_id,
            Some(alex.person.id),
            "and undo left the surviving state alone instead of half-erasing it"
        );
        assert_eq!(
            store.list_persons(folder_id).unwrap().len(),
            1,
            "the person it could not fully undo is still referenced"
        );
    }

    /// The same hazard for the pre-existing-auto bookkeeping: an auto label
    /// that predates the naming gets a NEW id when its photo is re-detected,
    /// so "not in prior_auto" would read as "created by this naming". Undo has
    /// to leave it alone.
    #[test]
    fn undo_keeps_a_pre_existing_auto_that_was_re_detected() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        for photo in ["p2", "p3"] {
            let snap = snapshot(&worker, photo, gen).unwrap();
            commit_scan(
                &mut worker,
                photo,
                &snap,
                &[nf(R1, &emb(0.98, 0.05))],
                &[],
                1,
                1,
                &source_ok,
            )
            .unwrap();
        }
        store.face_set_name(&ids[..1], "Alex").unwrap();
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        assert_eq!(
            store.faces_for_photo("p2").unwrap().unwrap()[0]
                .assigned_by
                .as_deref(),
            Some("auto")
        );

        // A second naming into the same person captures p2's auto label as
        // pre-existing…
        let p3_face = store.faces_for_photo("p3").unwrap().unwrap()[0].face_id;
        let (_, op2) = store.face_set_name(&[p3_face], "Alex").unwrap();
        // …and then p2 is re-detected, giving that label a new row id.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        commit_scan(
            &mut worker,
            "p2",
            &snap,
            &[nf(R1, &emb(0.98, 0.05))],
            &[],
            1,
            1,
            &source_ok,
        )
        .unwrap();

        store.undo_naming(&op2).unwrap();
        assert!(
            store.faces_for_photo("p2").unwrap().unwrap()[0]
                .person_id
                .is_some(),
            "a label that predates the op survives its re-detection too"
        );
    }

    /// The plan's carry-over ordering requirement, both halves: the UNIQUE
    /// constraint really does forbid old and new rows coexisting (so the order
    /// is not cosmetic), and a failure PART WAY THROUGH the inserts — after
    /// the delete and after some rows already landed — rolls the photo back to
    /// exactly the state it had.
    #[test]
    fn carry_over_is_delete_then_insert_and_rolls_back_intact() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[1..2], "Alex").unwrap();

        // The constraint the ordering exists for: a second row at the same
        // (photo_id, face_index) is rejected, so inserting before deleting
        // could never have worked.
        let clash = worker.execute(
            "INSERT INTO faces (photo_id, face_index, x, y, w, h, det_score, embedding, model_gen)
             VALUES ('p1', 0, 0.1, 0.1, 0.2, 0.2, 0.9, X'00', ?1)",
            params![gen],
        );
        assert!(
            clash.unwrap_err().to_string().contains("UNIQUE"),
            "old and new rows cannot coexist — hence delete, then insert"
        );

        let before = store.faces_for_photo("p1").unwrap().unwrap();
        let snap = snapshot(&worker, "p1", gen).unwrap();
        // Delete the person out from under the snapshot. `ON DELETE SET NULL`
        // clears the live rows, but the snapshot still carries the id — so the
        // SECOND insert (the carried one) violates the foreign key, after the
        // delete and the first insert have already happened inside the tx.
        worker
            .execute("DELETE FROM persons WHERE id = ?1", params![alex.person.id])
            .unwrap();
        let err = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0)), nf(R2, &emb(0.0, 1.0))],
            &[],
            1,
            1,
            &source_ok,
        )
        .unwrap_err();
        assert!(err.to_string().to_uppercase().contains("FOREIGN KEY"));

        let after = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(after.len(), 2, "the old rows are still there");
        assert_eq!(
            after.iter().map(|f| f.face_id).collect::<Vec<_>>(),
            before.iter().map(|f| f.face_id).collect::<Vec<_>>(),
            "…the very same rows, not replacements"
        );
        assert_eq!(
            face_revision(&worker, "p1").unwrap(),
            snap.revision,
            "a rolled-back scan does not move the photo's identity"
        );
    }

    /// The same rollback under the UNIQUE constraint itself, mid-transaction —
    /// the case the plan named. Production inputs cannot produce duplicate
    /// face indexes (`commit_scan` enumerates them), so the conflict is
    /// injected narrowly: a TEMP trigger on this connection inserts a
    /// duplicate `(photo_id, 0)` row just before the SECOND insert, firing
    /// UNIQUE(photo_id, face_index) part-way through the insert loop — after
    /// the delete and the first insert have already run inside the tx.
    #[test]
    fn a_mid_insert_unique_conflict_rolls_the_carry_over_back() {
        let (store, mut worker, _, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        let before = store.faces_for_photo("p1").unwrap().unwrap();
        let snap = snapshot(&worker, "p1", gen).unwrap();
        worker
            .execute_batch(
                "CREATE TEMP TRIGGER unique_clash BEFORE INSERT ON faces
                 WHEN NEW.face_index = 1
                 BEGIN
                   INSERT INTO faces (photo_id, face_index, x, y, w, h, det_score,
                                      embedding, model_gen)
                   VALUES (NEW.photo_id, 0, 0, 0, 0, 0, 0, X'00', NEW.model_gen);
                 END",
            )
            .unwrap();
        let err = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0)), nf(R2, &emb(0.0, 1.0))],
            &[],
            3,
            3,
            &source_ok,
        )
        .unwrap_err();
        assert!(
            err.to_string().to_uppercase().contains("UNIQUE"),
            "the failure really is the UNIQUE constraint: {err}"
        );
        worker.execute_batch("DROP TRIGGER unique_clash").unwrap();

        let after = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(
            after.iter().map(|f| f.face_id).collect::<Vec<_>>(),
            before.iter().map(|f| f.face_id).collect::<Vec<_>>(),
            "the photo is exactly the rows it was — the delete rolled back"
        );
        assert_eq!(after[0].person_id, Some(alex.person.id), "name intact");
        assert_eq!(
            face_revision(&worker, "p1").unwrap(),
            snap.revision,
            "identity unmoved"
        );
    }

    /// The People panel's "Scanning…" is driven by `pending`. A failed photo
    /// the worker will still retry is *retrying* — pending, not failed
    /// (declaring it terminal mid-schedule was the round-3 finding) — and
    /// only a photo whose session retries are exhausted is a terminal error
    /// that stops counting as work.
    #[test]
    fn scan_status_separates_pending_work_from_terminal_failures() {
        let (store, mut worker, folder_id, _) = setup();
        let gen = ensure_gen(&mut worker, "d1", "r1", 1).unwrap();
        let none: std::collections::HashSet<String> = Default::default();
        let status = store.face_scan_status(folder_id, &none).unwrap();
        assert_eq!(
            (status.total, status.pending),
            (3, 3),
            "nothing scanned yet"
        );

        let snap = snapshot(&worker, "p1", gen).unwrap();
        commit_scan(&mut worker, "p1", &snap, &[], &[], 1, 1, &source_ok).unwrap();
        record_scan_error(&worker, "p2", gen, "decode failed").unwrap();
        // The worker still owes p2 retries: it is retrying, not failed.
        let status = store.face_scan_status(folder_id, &none).unwrap();
        assert_eq!(status.scanned, 1);
        assert_eq!(
            (status.errors, status.retrying, status.pending),
            (0, 1, 2),
            "p2 retrying + p3 queued; nothing is a failure yet"
        );

        // The session exhausts p2: now it is terminal and leaves pending.
        let spent: std::collections::HashSet<String> = ["p2".to_string()].into_iter().collect();
        let status = store.face_scan_status(folder_id, &spent).unwrap();
        assert_eq!((status.errors, status.retrying, status.pending), (1, 0, 1));

        // The last photo fails and exhausts too: nothing is pending any more,
        // so the panel stops claiming a scan is running.
        record_scan_error(&worker, "p3", gen, "decode failed").unwrap();
        let spent: std::collections::HashSet<String> =
            ["p2".to_string(), "p3".to_string()].into_iter().collect();
        let status = store.face_scan_status(folder_id, &spent).unwrap();
        assert_eq!((status.scanned, status.errors, status.pending), (1, 2, 0));

        // A rescan puts the work back.
        store.mark_face_stale("p1").unwrap();
        assert_eq!(
            store.face_scan_status(folder_id, &spent).unwrap().pending,
            1
        );
    }

    /// A selection the rescan replaced is a stale selection: fail loudly
    /// instead of leaving a person behind with no faces.
    #[test]
    fn naming_a_vanished_selection_creates_no_person() {
        let (store, mut worker, folder_id, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        // Another photo's row, so the rescan below can't hand p1's replacement
        // the same rowid the panel is still holding.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        commit_scan(
            &mut worker,
            "p2",
            &snap,
            &[nf(R1, &emb(0.0, 1.0))],
            &[],
            1,
            1,
            &source_ok,
        )
        .unwrap();
        // A rescan replaces p1's rows — the panel's face ids are now stale.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            2,
            2,
            &source_ok,
        )
        .unwrap();

        let err = store.face_set_name(&ids, "Ghost").unwrap_err();
        assert!(err.contains("re-detected"), "{err}");
        assert!(
            store.list_persons(folder_id).unwrap().is_empty(),
            "a naming that named nothing must not create a person"
        );
    }

    #[test]
    fn merging_a_person_into_themselves_changes_nothing() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids, "Alex").unwrap();
        assert_eq!(
            store.merge_persons(alex.person.id, alex.person.id).unwrap(),
            0
        );
        let persons = store.list_persons(folder_id).unwrap();
        assert_eq!(persons.len(), 1, "the person survives a self-merge");
        assert_eq!(persons[0].folder_count, 2, "and keeps every face");
    }

    #[test]
    fn ignored_faces_leave_every_surface_and_come_back() {
        let (store, mut worker, folder_id, _) = setup();
        let (gen, alex, _) = seed_recognition(&store, &mut worker);
        // Ignore Alex's confirmed face + p2's unassigned lookalike (a
        // "statue/stranger cluster dismiss").
        let p2_face = store.faces_for_photo("p2").unwrap().unwrap()[0].face_id;
        let alex_face: i64 = {
            let conn = store.lock_conn();
            conn.query_row(
                "SELECT id FROM faces WHERE person_id = ?1",
                params![alex],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            store
                .set_faces_ignored(&[alex_face, p2_face], true)
                .unwrap(),
            2
        );

        // Gone from counts, prototypes, clusters and the sweep.
        let persons = store.list_persons(folder_id).unwrap();
        let alex_row = persons.iter().find(|p| p.id == alex).unwrap();
        assert_eq!(alex_row.folder_count, 0, "ignoring clears the assignment");
        assert!(
            person_prototypes(&worker, gen, 5)
                .unwrap()
                .iter()
                .all(|p| p.person_id != alex),
            "ignored faces never train prototypes"
        );
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        assert_eq!(
            sweep_photo(&mut worker, "p2", gen, &protos, 0.0, 0.0).unwrap(),
            0,
            "ignored faces are invisible to the sweep"
        );
        let out = store.face_clusters(folder_id, 0.0).unwrap();
        assert!(out.clusters.iter().all(|c| !c.face_ids.contains(&p2_face)));
        assert!(out.loose.iter().all(|c| c.face_id != p2_face));

        // Reversible: un-ignore returns the face to Unnamed (not to Alex —
        // the assignment was deliberately dropped).
        store.set_faces_ignored(&[alex_face], false).unwrap();
        let f = store
            .faces_for_photo("p1")
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|f| f.face_id == alex_face)
            .unwrap();
        assert!(!f.ignored);
        assert_eq!(f.person_id, None);
    }

    #[test]
    fn delete_person_frees_faces_and_drops_rejections() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        let (dani, _) = store.face_set_name(&ids[1..], "Dani").unwrap();
        store.face_reject(ids[1], alex.person.id).unwrap();

        assert_eq!(store.delete_person(alex.person.id).unwrap(), 1);
        let persons = store.list_persons(folder_id).unwrap();
        assert_eq!(persons.len(), 1);
        assert_eq!(persons[0].id, dani.person.id, "other people untouched");
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].person_id, None, "faces return to Unnamed");
        assert_eq!(faces[1].person_id, Some(dani.person.id));
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE person_id = ?1",
                params![alex.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "rejections naming the deleted person are gone");
    }

    #[test]
    fn rescan_marks_folder_stale_and_reports_chip_counts() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        store.face_set_name(&ids[..1], "Alex").unwrap();
        let rows = store.rescan_faces(folder_id).unwrap();
        assert_eq!(
            rows,
            vec!["p1".to_string()],
            "the scanned photo whose chips the caller drops"
        );
        assert_eq!(scan_row(&worker, "p1").unwrap().unwrap().status, "stale");
        // Badges go quiet while a photo is pending re-detection…
        assert!(
            store.faces_for_photo("p1").unwrap().is_none(),
            "a stale photo reports 'not scanned', not stale rows"
        );
        // …but the rows survive as the carry-over source, so the name comes
        // back with the replacement face rather than being lost.
        let conn = store.lock_conn();
        let name: String = conn
            .query_row(
                "SELECT per.name FROM faces f JOIN persons per ON per.id = f.person_id
                 WHERE f.photo_id = 'p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "Alex");
    }

    #[test]
    fn rename_reports_conflict_for_merge_offer() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        let (dani, _) = store.face_set_name(&ids[1..], "Dani").unwrap();
        assert!(matches!(
            store.rename_person(alex.person.id, "Alexis").unwrap(),
            RenameOutcome::Renamed
        ));
        // Colliding rename (case-insensitive) is a merge offer, not an error.
        match store.rename_person(alex.person.id, "dani").unwrap() {
            RenameOutcome::Conflict {
                target_id,
                target_name,
            } => {
                assert_eq!(target_id, dani.person.id);
                assert_eq!(target_name, "Dani");
            }
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn merge_persons_moves_faces_rekeys_rejections_drops_contradictions() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        // "Aelx" (typo, face 1) and "Alex" (face 2).
        let (aelx, _) = store.face_set_name(&ids[..1], "Aelx").unwrap();
        let (alex, _) = store.face_set_name(&ids[1..], "Alex").unwrap();
        // Face 1 once got "not Alex" (before the user realized Aelx==Alex),
        // plus a "not Aelx" from some third face's history — via face 2.
        store.face_reject(ids[0], alex.person.id).unwrap();
        // face_reject cleared nothing (face 1 is Aelx's), but re-assert it:
        store.face_assign(ids[0], Some(aelx.person.id)).unwrap();
        store.face_reject(ids[1], aelx.person.id).unwrap();
        store.face_assign(ids[1], Some(alex.person.id)).unwrap();

        let moved = store.merge_persons(aelx.person.id, alex.person.id).unwrap();
        assert_eq!(moved, 1);
        let persons = store.list_persons(folder_id).unwrap();
        assert_eq!(persons.len(), 1, "source person removed");
        assert_eq!(persons[0].id, alex.person.id);
        assert_eq!(persons[0].folder_count, 2, "faces moved to the survivor");
        // The moved face's "not Alex" contradiction is gone — the merge
        // asserts they ARE Alex.
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE person_id = ?1",
                params![alex.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "no rejection may target the merged identity's faces");
        let orphaned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE person_id = ?1",
                params![aelx.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphaned, 0, "source rejections re-keyed or removed");
    }

    #[test]
    fn naming_a_rejected_face_again_clears_the_contradiction() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (carson, _) = store.face_set_name(&ids[..1], "Carson").unwrap();
        // "Not Carson" on the photo → back to Unnamed, Carson barred.
        store.face_reject(ids[0], carson.person.id).unwrap();
        assert_eq!(
            store.faces_for_photo("p1").unwrap().unwrap()[0].person_id,
            None
        );
        // The user changes their mind and names it Carson again.
        store.face_set_name(&ids[..1], "Carson").unwrap();
        let f = &store.faces_for_photo("p1").unwrap().unwrap()[0];
        assert_eq!(f.person_id, Some(carson.person.id));
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections
                 WHERE face_id = ?1 AND person_id = ?2",
                params![ids[0], carson.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 0,
            "the stale \"not Carson\" must not outlive the retraction"
        );
    }

    #[test]
    fn user_assignment_clears_stale_not_x() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (alex, _) = store.face_set_name(&ids[..1], "Alex").unwrap();
        store.face_reject(ids[0], alex.person.id).unwrap(); // "not Alex"
                                                            // The user changes their mind: explicit re-assign to Alex.
        store.face_assign(ids[0], Some(alex.person.id)).unwrap();
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections
                 WHERE face_id = ?1 AND person_id = ?2",
                params![ids[0], alex.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "explicit assignment drops the contradiction");
    }
}
