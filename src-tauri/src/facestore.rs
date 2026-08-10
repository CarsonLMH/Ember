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
    hidden INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS face_model_gens (
    gen INTEGER PRIMARY KEY,
    det_sha256 TEXT NOT NULL, rec_sha256 TEXT NOT NULL, prep_version INTEGER NOT NULL,
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
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    scanned_at INTEGER NOT NULL);
INSERT OR IGNORE INTO face_state (key, value) VALUES ('index_epoch', 1), ('index_enabled', 1);
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

pub fn index_enabled(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(state_get(conn, "index_enabled")? != 0)
}

/// Newest registered model generation, if any.
pub fn current_gen(conn: &Connection) -> rusqlite::Result<Option<(i64, String, String, i64)>> {
    conn.query_row(
        "SELECT gen, det_sha256, rec_sha256, prep_version FROM face_model_gens
         ORDER BY gen DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .optional()
}

/// Worker init: register the bundled models as the newest generation if they
/// aren't already. A new generation bumps the epoch (kills in-flight commits
/// from any process) and stale-marks every older-generation scan so those
/// photos re-enter the queue for progressive reprocessing.
pub fn ensure_gen(
    conn: &mut Connection,
    det_sha: &str,
    rec_sha: &str,
    prep_version: i64,
) -> rusqlite::Result<i64> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some((gen, det, rec, prep)) = current_gen(&tx)? {
        if det == det_sha && rec == rec_sha && prep == prep_version {
            tx.commit()?;
            return Ok(gen);
        }
    }
    tx.execute(
        "INSERT INTO face_model_gens (det_sha256, rec_sha256, prep_version, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![det_sha, rec_sha, prep_version, now_ms()],
    )?;
    let gen = tx.last_insert_rowid();
    bump_epoch(&tx)?;
    tx.execute(
        "UPDATE face_scan SET status = 'stale' WHERE model_gen <> ?1",
        params![gen],
    )?;
    tx.commit()?;
    Ok(gen)
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
/// simply dropped and the photo re-enters the queue naturally.
#[derive(Debug, PartialEq)]
pub enum CommitOutcome {
    Committed,
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
pub fn commit_scan(
    conn: &mut Connection,
    photo_id: &str,
    snap: &ScanSnapshot,
    new: &[NewFace],
    proposals: &[Option<(i64, f32)>],
    mtime: i64,
    size: i64,
) -> rusqlite::Result<CommitOutcome> {
    // Correspondence computed before the write, outside the transaction.
    let by_new = plan_carry_over(snap, new);

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Conditional guards — any mismatch: discard, never overwrite newer truth.
    let enabled = index_enabled(&tx)?;
    let epoch = state_get(&tx, "index_epoch")?;
    let gen_now = current_gen(&tx)?.map(|(g, ..)| g);
    let revision = face_revision(&tx, photo_id)?;
    if !enabled || epoch != snap.epoch || gen_now != Some(snap.gen) || revision != snap.revision {
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
    tx.execute(
        "INSERT INTO face_scan (photo_id, mtime, size, model_gen, face_count, status,
                                face_revision, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'ok', ?6, ?7)
         ON CONFLICT(photo_id) DO UPDATE SET
           mtime = excluded.mtime, size = excluded.size, model_gen = excluded.model_gen,
           face_count = excluded.face_count, status = 'ok', last_error = NULL,
           scanned_at = excluded.scanned_at",
        params![
            photo_id,
            mtime,
            size,
            snap.gen,
            new.len() as i64,
            snap.revision,
            now_ms()
        ],
    )?;
    tx.commit()?;
    Ok(CommitOutcome::Committed)
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

/// Matching prototypes: USER-CONFIRMED faces only, current generation, up to
/// `max_exemplars` per person picked for diversity. Auto-assigned faces never
/// train future matches — one false positive must not move anyone's anchor.
pub fn person_prototypes(
    conn: &Connection,
    gen: i64,
    max_exemplars: usize,
) -> rusqlite::Result<Vec<facedet::PersonProtos>> {
    let mut stmt = conn.prepare(
        "SELECT person_id, embedding FROM faces
         WHERE person_id IS NOT NULL AND assigned_by = 'user'
           AND ignored = 0 AND model_gen = ?1
         ORDER BY person_id, det_score DESC",
    )?;
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
        let mut stmt = conn.prepare(
            "SELECT id, embedding FROM faces
             WHERE photo_id = ?1 AND person_id IS NULL AND ignored = 0 AND model_gen = ?2",
        )?;
        let v: Vec<(i64, Vec<u8>)> = stmt
            .query_map(params![photo_id, gen], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    // Comparisons outside any lock.
    let empty = std::collections::HashSet::new();
    let matches: Vec<(i64, i64, f32)> = faces
        .iter()
        .filter_map(|(fid, blob)| {
            let emb = facedet::blob_to_embedding(blob)?;
            let rejected = rejections.get(fid).unwrap_or(&empty);
            facedet::match_face(&emb, protos, rejected, threshold, margin)
                .map(|(pid, score)| (*fid, pid, score))
        })
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
    let mut stmt = conn.prepare(
        "SELECT f.person_id, per.name, f.embedding FROM faces f
         JOIN persons per ON per.id = f.person_id
         WHERE f.assigned_by = 'user' AND f.ignored = 0 AND f.model_gen = ?1
         ORDER BY f.person_id, f.det_score DESC",
    )?;
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
    let mut stmt = conn.prepare(
        "SELECT embedding FROM faces
         WHERE person_id IS NULL AND ignored = 0 AND model_gen = ?1",
    )?;
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

/// Scan-time state of one photo, for the worker's job selection.
pub struct ScanRow {
    pub mtime: i64,
    pub size: i64,
    pub model_gen: i64,
    pub status: String,
    pub face_count: i64,
}

pub fn scan_row(conn: &Connection, photo_id: &str) -> rusqlite::Result<Option<ScanRow>> {
    conn.query_row(
        "SELECT mtime, size, model_gen, face_count, status FROM face_scan WHERE photo_id = ?1",
        params![photo_id],
        |r| {
            Ok(ScanRow {
                mtime: r.get(0)?,
                size: r.get(1)?,
                model_gen: r.get(2)?,
                face_count: r.get(3)?,
                status: r.get(4)?,
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
    pub hidden: bool,
    /// Faces of this person in the queried folder (trashed excluded).
    pub folder_count: i64,
    /// Global representative face for the chip (photoId, faceIndex).
    pub rep_photo_id: Option<String>,
    pub rep_face_index: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChipRef {
    pub photo_id: String,
    pub face_index: i64,
    pub face_id: i64,
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
    pub errors: i64,
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

    /// Returns true when the value actually changed (each transition bumps the
    /// epoch, discarding any in-flight worker commit from either process).
    pub fn set_faces_enabled(&self, enabled: bool) -> rusqlite::Result<bool> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cur = index_enabled(&tx)?;
        if cur == enabled {
            tx.commit()?;
            return Ok(false);
        }
        state_set(&tx, "index_enabled", enabled as i64)?;
        bump_epoch(&tx)?;
        tx.commit()?;
        Ok(true)
    }

    /// Privacy delete: one atomic control transaction. Disables indexing
    /// (durably — control rows are never deleted), bumps the epoch, and wipes
    /// every face-derived row. Chips are the caller's cleanup (files).
    /// No automatic reindex follows; re-enabling is an explicit user act.
    pub fn delete_face_data(&self) -> rusqlite::Result<()> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        state_set(&tx, "index_enabled", 0)?;
        bump_epoch(&tx)?;
        tx.execute("DELETE FROM face_person_rejections", [])?;
        tx.execute("DELETE FROM faces", [])?;
        tx.execute("DELETE FROM persons", [])?;
        tx.execute("DELETE FROM face_scan", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Startup sync: a hand-edited settings.toml expresses user intent and
    /// wins on launch. Bumps the epoch only on an actual transition.
    pub fn sync_faces_enabled_from_settings(&self, enabled: bool) -> rusqlite::Result<bool> {
        self.set_faces_enabled(enabled)
    }

    pub fn face_scan_status(&self, folder_id: i64) -> rusqlite::Result<FaceScanStatus> {
        let conn = self.lock_conn();
        let enabled = index_enabled(&conn)?;
        let (total, scanned, errors) = conn.query_row(
            "SELECT COUNT(*),
                    COUNT(s.photo_id) FILTER (WHERE s.status = 'ok'),
                    COUNT(s.photo_id) FILTER (WHERE s.status = 'error')
             FROM photos p LEFT JOIN face_scan s ON s.photo_id = p.id
             WHERE p.folder_id = ?1 AND p.trashed = 0 AND p.missing = 0",
            params![folder_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(FaceScanStatus {
            enabled,
            total,
            scanned,
            errors,
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
            "SELECT f.id, f.photo_id, f.face_index, f.embedding
             FROM faces f
             JOIN photos p ON p.id = f.photo_id
             JOIN face_scan s ON s.photo_id = f.photo_id
             WHERE p.folder_id = ?1 AND p.trashed = 0
               AND s.status = 'ok' AND f.model_gen = ?2
               AND f.person_id IS NULL AND f.ignored = 0
             ORDER BY f.det_score DESC",
        )?;
        let rows: Vec<(i64, String, i64, Vec<u8>)> = stmt
            .query_map(params![folder_id, gen], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_, _>>()?;
        let embeddings: Vec<Vec<f32>> = rows
            .iter()
            .map(|(.., blob)| facedet::blob_to_embedding(blob).unwrap_or_default())
            .collect();
        let chip = |i: usize| ChipRef {
            photo_id: rows[i].1.clone(),
            face_index: rows[i].2,
            face_id: rows[i].0,
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
    pub fn face_set_name(
        &self,
        face_ids: &[i64],
        name: &str,
    ) -> rusqlite::Result<(NamingResult, NamingOp)> {
        let display = name.trim();
        let norm = display.to_lowercase();
        if norm.is_empty() {
            return Err(rusqlite::Error::InvalidParameterName("empty name".into()));
        }
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM persons WHERE name_norm = ?1",
                params![norm],
                |r| r.get(0),
            )
            .optional()?;
        let (person_id, person_created) = match existing {
            Some(id) => (id, false),
            None => {
                tx.execute(
                    "INSERT INTO persons (name, name_norm, created_at) VALUES (?1, ?2, ?3)",
                    params![display, norm, now_ms()],
                )?;
                (tx.last_insert_rowid(), true)
            }
        };
        let mut prior = Vec::new();
        let mut rejections = Vec::new();
        let mut affected = Vec::new();
        let mut photos: std::collections::HashSet<String> = std::collections::HashSet::new();
        for &fid in face_ids {
            // A face replaced by a concurrent rescan simply isn't here anymore.
            let Some((photo_id, p_person, p_by, p_sim)) = tx
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
                .optional()?
            else {
                continue;
            };
            tx.execute(
                "UPDATE faces SET person_id = ?2, assigned_by = 'user', similarity = NULL
                 WHERE id = ?1",
                params![fid, person_id],
            )?;
            // Any correction records a rejection of the displaced identity —
            // whether that assignment was auto OR manual (review-2).
            if let Some(displaced) = p_person {
                if displaced != person_id {
                    let n = tx.execute(
                        "INSERT OR IGNORE INTO face_person_rejections
                         (face_id, person_id, created_at) VALUES (?1, ?2, ?3)",
                        params![fid, displaced, now_ms()],
                    )?;
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
            )?;
            photos.insert(photo_id.clone());
            prior.push((fid, photo_id, p_person, p_by, p_sim));
            affected.push(fid);
        }
        let mut revisions = HashMap::new();
        for pid in &photos {
            bump_revision(&tx, pid)?;
            revisions.insert(pid.clone(), face_revision(&tx, pid)?);
        }
        let person = person_row(&tx, person_id, None)?;
        tx.commit()?;
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
            },
        ))
    }

    /// Undo a naming op: restore exact prior state, but only for photos whose
    /// face_revision still matches the op (later edits win). Removes exactly
    /// the rejections the op wrote, and the person only if newly created and
    /// now unreferenced. Returns how many faces were restored.
    pub fn undo_naming(&self, op: &NamingOp) -> rusqlite::Result<usize> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut restored = 0usize;
        let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (fid, photo_id, p_person, p_by, p_sim) in &op.prior {
            let Some(&rev) = op.revisions.get(photo_id) else {
                continue;
            };
            if face_revision(&tx, photo_id)? != rev {
                continue; // this photo was edited after the naming — leave it
            }
            let n = tx.execute(
                "UPDATE faces SET person_id = ?2, assigned_by = ?3, similarity = ?4
                 WHERE id = ?1",
                params![fid, p_person, p_by, p_sim],
            )?;
            if n > 0 {
                restored += 1;
                touched.insert(photo_id.clone());
            }
        }
        for (fid, pid) in &op.rejections {
            tx.execute(
                "DELETE FROM face_person_rejections WHERE face_id = ?1 AND person_id = ?2",
                params![fid, pid],
            )?;
        }
        // Auto-assignments to this person exist only because of the naming
        // being undone (embed-time matching / the post-naming sweep) — clear
        // them regardless of revision: machine state, regenerable.
        tx.execute(
            "UPDATE faces SET person_id = NULL, assigned_by = NULL, similarity = NULL
             WHERE person_id = ?1 AND assigned_by = 'auto'",
            params![op.person_id],
        )?;
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
            "SELECT f.photo_id, f.face_index, f.id
             FROM faces f JOIN photos p ON p.id = f.photo_id
             WHERE f.person_id = ?1 AND f.ignored = 0 AND p.folder_id = ?2 AND p.trashed = 0
             ORDER BY f.det_score DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(params![person_id, folder_id], |r| {
            Ok(ChipRef {
                photo_id: r.get(0)?,
                face_index: r.get(1)?,
                face_id: r.get(2)?,
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

    /// Chip rects for one photo, for the repair queue (no ONNX involved).
    pub fn face_rects(&self, photo_id: &str) -> rusqlite::Result<Vec<(i64, [f32; 4])>> {
        face_rects(&self.lock_conn(), photo_id)
    }

    /// Pixel change / manual rescan: keep old rows as the carry-over source,
    /// just mark the scan stale so the worker reprocesses. Returns the old
    /// face_count so the caller can delete that photo's chips.
    pub fn mark_face_stale(&self, photo_id: &str) -> rusqlite::Result<i64> {
        let conn = self.lock_conn();
        let count: Option<i64> = conn
            .query_row(
                "SELECT face_count FROM face_scan WHERE photo_id = ?1",
                params![photo_id],
                |r| r.get(0),
            )
            .optional()?;
        conn.execute(
            "UPDATE face_scan SET status = 'stale' WHERE photo_id = ?1",
            params![photo_id],
        )?;
        Ok(count.unwrap_or(0))
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
    pub fn rescan_faces(&self, folder_id: i64) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows: Vec<(String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT s.photo_id, s.face_count FROM face_scan s
                 JOIN photos p ON p.id = s.photo_id
                 WHERE p.folder_id = ?1 AND p.trashed = 0",
            )?;
            let v: Vec<(String, i64)> = stmt
                .query_map(params![folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?
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
    let (name, hidden): (String, bool) = conn.query_row(
        "SELECT name, hidden FROM persons WHERE id = ?1",
        params![person_id],
        |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
    )?;
    let folder_count = match folder_id {
        Some(fid) => conn.query_row(
            "SELECT COUNT(*) FROM faces f JOIN photos p ON p.id = f.photo_id
             WHERE f.person_id = ?1 AND f.ignored = 0 AND p.folder_id = ?2 AND p.trashed = 0",
            params![person_id, fid],
            |r| r.get(0),
        )?,
        None => conn.query_row(
            "SELECT COUNT(*) FROM faces WHERE person_id = ?1 AND ignored = 0",
            params![person_id],
            |r| r.get(0),
        )?,
    };
    // Stable global representative: user-confirmed first, then strongest.
    let rep: Option<(String, i64)> = conn
        .query_row(
            "SELECT photo_id, face_index FROM faces WHERE person_id = ?1 AND ignored = 0
             ORDER BY (assigned_by = 'user') DESC, det_score DESC LIMIT 1",
            params![person_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(PersonOut {
        id: person_id,
        name,
        hidden,
        folder_count,
        rep_photo_id: rep.as_ref().map(|(p, _)| p.clone()),
        rep_face_index: rep.map(|(_, i)| i),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::PhotoEntry;
    use std::path::PathBuf;
    use std::sync::Arc;

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
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Committed);
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

    #[test]
    fn naming_normalizes_round_trips_and_undoes_exactly() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);

        let (res, op) = store.face_set_name(&ids[..1], " Nati ").unwrap();
        assert_eq!(res.person.name, "Nati", "display name trimmed");
        assert!(op.person_created);
        // Same person via case/space variants.
        let (res2, op2) = store.face_set_name(&ids[1..], "nati").unwrap();
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
        let (res, op) = store.face_set_name(&ids, "Nati").unwrap();
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
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
        // Manual mistake corrected by renaming the face to Dani → rejection
        // of Nati recorded even though the displaced assignment was manual.
        let (dani, _) = store.face_set_name(&ids[..1], "Dani").unwrap();
        {
            let conn = store.lock_conn();
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM face_person_rejections WHERE face_id=?1 AND person_id=?2",
                    params![ids[0], nati.person.id],
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
                params![ids[1], nati.person.id],
            )
            .unwrap();
        }
        store.face_assign(ids[1], Some(dani.person.id)).unwrap();
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE face_id=?1 AND person_id=?2",
                params![ids[1], nati.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "displaced auto identity rejected");
    }

    #[test]
    fn reject_clears_assignment_and_sticks() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
        store.face_reject(ids[0], nati.person.id).unwrap();
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].person_id, None, "\"not Nati\" cleared the face");
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
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
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
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Committed);

        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces.len(), 3);
        assert_eq!(faces[0].person_id, Some(nati.person.id), "Nati survived");
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
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();

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
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Committed);
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].person_id, Some(nati.person.id));

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
        store.face_set_name(&ids[..1], "Nati").unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
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
        store.set_faces_enabled(false).unwrap();
        store.set_faces_enabled(true).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded, "epoch change kills commits");

        // Guard 3: disabled at commit time.
        let snap = snapshot(&worker, "p1", gen).unwrap();
        store.set_faces_enabled(false).unwrap();
        let out = commit_scan(
            &mut worker,
            "p1",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded, "disabled parks all writes");
        store.set_faces_enabled(true).unwrap();

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
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Discarded, "stale gen never lands");
    }

    #[test]
    fn delete_all_is_durable_and_discards_in_flight() {
        let (store, mut worker, folder_id, path) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        store.face_set_name(&ids[..1], "Nati").unwrap();

        // Worker snapshotted BEFORE the delete → its commit must die.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        store.delete_face_data().unwrap();
        let out = commit_scan(
            &mut worker,
            "p2",
            &snap,
            &[nf(R1, &emb(1.0, 0.0))],
            &[],
            9,
            9,
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
        assert!(store.set_faces_enabled(true).unwrap());
        assert!(store.faces_enabled().unwrap());
        assert_eq!(store.face_scan_status(folder_id).unwrap().scanned, 0);
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
            commit_scan(&mut worker, photo, &snap, &faces, &[], 1, 1).unwrap();
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
        store.face_set_name(&ids, "Nati").unwrap();
        let map = store.person_map(folder_id).unwrap();
        assert!(map.contains_key("p1"));
        assert!(!map.contains_key("p2"), "trashed photo not in person_map");
    }

    // ---------- Slice B: auto-recognition ----------

    /// Seed p1 with two named faces (Nati at e(1,0), Dani at e(0,1)) and
    /// p2/p3 with unassigned faces; returns (gen, nati_id, dani_id).
    fn seed_recognition(store: &Store, worker: &mut Connection) -> (i64, i64, i64) {
        let (gen, ids) = seed_faces(worker);
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
        let (dani, _) = store.face_set_name(&ids[1..], "Dani").unwrap();
        // p2: a face nearly identical to Nati; p3: an ambiguous face.
        for (photo, e) in [("p2", emb(0.98, 0.05)), ("p3", emb(1.0, 1.0))] {
            let snap = snapshot(worker, photo, gen).unwrap();
            commit_scan(worker, photo, &snap, &[nf(R1, &e)], &[], 1, 1).unwrap();
        }
        (gen, nati.person.id, dani.person.id)
    }

    #[test]
    fn sweep_assigns_confident_skips_ambiguous_and_rejected() {
        let (store, mut worker, _, _) = setup();
        let (gen, nati, _) = seed_recognition(&store, &mut worker);
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        assert_eq!(protos.len(), 2);

        // p2 (clear Nati lookalike) assigns; p3 (equidistant) must not.
        let n = sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        assert_eq!(n, 1);
        let f = &store.faces_for_photo("p2").unwrap().unwrap()[0];
        assert_eq!(f.person_id, Some(nati));
        assert_eq!(f.assigned_by.as_deref(), Some("auto"));
        let n = sweep_photo(&mut worker, "p3", gen, &protos, 0.40, 0.05).unwrap();
        assert_eq!(n, 0, "ambiguous face stays for the human");

        // "not Nati" on p2's face, then re-sweep: rejection is absolute.
        let fid = f.face_id;
        store.face_reject(fid, nati).unwrap();
        let n = sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        assert_eq!(n, 0, "\"not X\" survives the sweep, forever");
        let f = &store.faces_for_photo("p2").unwrap().unwrap()[0];
        assert_eq!(f.person_id, None);

        // Disabled indexing parks the sweep too.
        store.set_faces_enabled(false).unwrap();
        let n = sweep_photo(&mut worker, "p3", gen, &protos, 0.0, 0.0).unwrap();
        assert_eq!(n, 0, "sweep writes nothing while disabled");
    }

    #[test]
    fn auto_assignments_never_train_prototypes() {
        let (store, mut worker, _, _) = setup();
        let (gen, nati, _) = seed_recognition(&store, &mut worker);
        let before: usize = person_prototypes(&worker, gen, 5)
            .unwrap()
            .iter()
            .find(|p| p.person_id == nati)
            .unwrap()
            .exemplars
            .len();
        let protos = person_prototypes(&worker, gen, 5).unwrap();
        sweep_photo(&mut worker, "p2", gen, &protos, 0.40, 0.05).unwrap();
        let after: usize = person_prototypes(&worker, gen, 5)
            .unwrap()
            .iter()
            .find(|p| p.person_id == nati)
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
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
        store.mark_face_stale("p1").unwrap();

        // Rescan: face 1 carries Nati (user); face 2 carries nothing and has
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
                Some((nati.person.id, 0.99)), // must be IGNORED (carried user state wins)
                None,
                Some((nati.person.id, 0.61)),
            ],
            2,
            2,
        )
        .unwrap();
        assert_eq!(out, CommitOutcome::Committed);
        let faces = store.faces_for_photo("p1").unwrap().unwrap();
        assert_eq!(faces[0].assigned_by.as_deref(), Some("user"), "carried");
        assert_eq!(faces[1].person_id, None);
        assert_eq!(faces[2].person_id, Some(nati.person.id));
        assert_eq!(faces[2].assigned_by.as_deref(), Some("auto"));
    }

    #[test]
    fn undo_naming_clears_the_sweeps_auto_assignments() {
        let (store, mut worker, folder_id, _) = setup();
        let (gen, ids) = seed_faces(&mut worker);
        // p2 gets an unassigned Nati-lookalike BEFORE naming.
        let snap = snapshot(&worker, "p2", gen).unwrap();
        commit_scan(
            &mut worker,
            "p2",
            &snap,
            &[nf(R1, &emb(0.98, 0.05))],
            &[],
            1,
            1,
        )
        .unwrap();

        let (_, op) = store.face_set_name(&ids[..1], "Nati").unwrap();
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

    #[test]
    fn ignored_faces_leave_every_surface_and_come_back() {
        let (store, mut worker, folder_id, _) = setup();
        let (gen, nati, _) = seed_recognition(&store, &mut worker);
        // Ignore Nati's confirmed face + p2's unassigned lookalike (a
        // "statue/stranger cluster dismiss").
        let p2_face = store.faces_for_photo("p2").unwrap().unwrap()[0].face_id;
        let nati_face: i64 = {
            let conn = store.lock_conn();
            conn.query_row(
                "SELECT id FROM faces WHERE person_id = ?1",
                params![nati],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            store
                .set_faces_ignored(&[nati_face, p2_face], true)
                .unwrap(),
            2
        );

        // Gone from counts, prototypes, clusters and the sweep.
        let persons = store.list_persons(folder_id).unwrap();
        let nati_row = persons.iter().find(|p| p.id == nati).unwrap();
        assert_eq!(nati_row.folder_count, 0, "ignoring clears the assignment");
        assert!(
            person_prototypes(&worker, gen, 5)
                .unwrap()
                .iter()
                .all(|p| p.person_id != nati),
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

        // Reversible: un-ignore returns the face to Unnamed (not to Nati —
        // the assignment was deliberately dropped).
        store.set_faces_ignored(&[nati_face], false).unwrap();
        let f = store
            .faces_for_photo("p1")
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|f| f.face_id == nati_face)
            .unwrap();
        assert!(!f.ignored);
        assert_eq!(f.person_id, None);
    }

    #[test]
    fn delete_person_frees_faces_and_drops_rejections() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
        let (dani, _) = store.face_set_name(&ids[1..], "Dani").unwrap();
        store.face_reject(ids[1], nati.person.id).unwrap();

        assert_eq!(store.delete_person(nati.person.id).unwrap(), 1);
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
                params![nati.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "rejections naming the deleted person are gone");
    }

    #[test]
    fn rescan_marks_folder_stale_and_reports_chip_counts() {
        let (store, mut worker, folder_id, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        store.face_set_name(&ids[..1], "Nati").unwrap();
        let rows = store.rescan_faces(folder_id).unwrap();
        assert_eq!(rows, vec![("p1".to_string(), 2)], "photo + chip count");
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
        assert_eq!(name, "Nati");
    }

    #[test]
    fn rename_reports_conflict_for_merge_offer() {
        let (store, mut worker, _, _) = setup();
        let (_, ids) = seed_faces(&mut worker);
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
        let (dani, _) = store.face_set_name(&ids[1..], "Dani").unwrap();
        assert!(matches!(
            store.rename_person(nati.person.id, "Natalie").unwrap(),
            RenameOutcome::Renamed
        ));
        // Colliding rename (case-insensitive) is a merge offer, not an error.
        match store.rename_person(nati.person.id, "dani").unwrap() {
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
        // "Nai" (typo, face 1) and "Nati" (face 2).
        let (nai, _) = store.face_set_name(&ids[..1], "Nai").unwrap();
        let (nati, _) = store.face_set_name(&ids[1..], "Nati").unwrap();
        // Face 1 once got "not Nati" (before the user realized Nai==Nati),
        // plus a "not Nai" from some third face's history — via face 2.
        store.face_reject(ids[0], nati.person.id).unwrap();
        // face_reject cleared nothing (face 1 is Nai's), but re-assert it:
        store.face_assign(ids[0], Some(nai.person.id)).unwrap();
        store.face_reject(ids[1], nai.person.id).unwrap();
        store.face_assign(ids[1], Some(nati.person.id)).unwrap();

        let moved = store.merge_persons(nai.person.id, nati.person.id).unwrap();
        assert_eq!(moved, 1);
        let persons = store.list_persons(folder_id).unwrap();
        assert_eq!(persons.len(), 1, "source person removed");
        assert_eq!(persons[0].id, nati.person.id);
        assert_eq!(persons[0].folder_count, 2, "faces moved to the survivor");
        // The moved face's "not Nati" contradiction is gone — the merge
        // asserts they ARE Nati.
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE person_id = ?1",
                params![nati.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "no rejection may target the merged identity's faces");
        let orphaned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections WHERE person_id = ?1",
                params![nai.person.id],
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
        let (nati, _) = store.face_set_name(&ids[..1], "Nati").unwrap();
        store.face_reject(ids[0], nati.person.id).unwrap(); // "not Nati"
                                                            // The user changes their mind: explicit re-assign to Nati.
        store.face_assign(ids[0], Some(nati.person.id)).unwrap();
        let conn = store.lock_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM face_person_rejections
                 WHERE face_id = ?1 AND person_id = ?2",
                params![ids[0], nati.person.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "explicit assignment drops the contradiction");
    }
}
