use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::scanner::PhotoEntry;

/// Source of truth for verdicts. Append-only `actions` journal; `photos` rows
/// are the replayed snapshot. WAL + synchronous=FULL: an acknowledged write
/// survives kill -9 at any instant.
pub struct Store {
    conn: Mutex<Connection>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderState {
    pub folder_id: i64,
    pub sort: String,
    pub reverse: bool,
    pub filter: String,
    pub cursor_photo: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PhotoRow {
    pub rating: u8,
    pub trashed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrashPayload {
    pub jpeg: Option<String>,
    pub raf: Option<String>,
    pub tjpeg: Option<String>,
    pub traf: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActionRow {
    pub seq: i64,
    pub photo_id: String,
    pub kind: String,
    pub payload: String,
}

/// What changed, for the frontend to patch its state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    pub photo_id: String,
    pub kind: String,
    pub rating: Option<u8>,
    pub trashed: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashedPhoto {
    pub id: String,
    pub stem: String,
    pub rating: u8,
    pub trashed_at: i64,
}

#[derive(Debug, Clone)]
pub struct XmpJob {
    pub photo_id: String,
    pub jpeg: Option<String>,
    pub raf: Option<String>,
    pub rating: u8,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XmpStatus {
    pub pending: i64,
    pub failed: i64,
    pub last_error: Option<String>,
}

/// A photo the last rescan couldn't find on disk (still listed, badged).
#[derive(Debug, Clone)]
pub struct MissingPhoto {
    pub id: String,
    pub dir: String,
    pub stem: String,
    pub has_jpeg: bool,
    pub has_raf: bool,
    pub rating: u8,
}

impl Store {
    pub fn new(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // A user instance and a hidden harness instance may share this DB.
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS folders (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                sort TEXT NOT NULL DEFAULT 'capture',
                reverse INTEGER NOT NULL DEFAULT 0,
                filter TEXT NOT NULL DEFAULT 'all',
                cursor_photo TEXT,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS photos (
                id TEXT PRIMARY KEY,
                folder_id INTEGER NOT NULL REFERENCES folders(id),
                dir TEXT NOT NULL,
                stem TEXT NOT NULL,
                jpeg_path TEXT,
                raf_path TEXT,
                rating INTEGER NOT NULL DEFAULT 0,
                trashed INTEGER NOT NULL DEFAULT 0,
                trashed_jpeg TEXT,
                trashed_raf TEXT,
                trashed_at INTEGER,
                missing INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_photos_folder ON photos(folder_id);
            CREATE TABLE IF NOT EXISTS actions (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                folder_id INTEGER NOT NULL,
                ts INTEGER NOT NULL,
                photo_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                undone INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_actions_folder ON actions(folder_id, undone, seq);
            CREATE TABLE IF NOT EXISTS xmp_queue (
                photo_id TEXT PRIMARY KEY,
                jpeg_path TEXT,
                raf_path TEXT,
                rating INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                attempts INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            );
            CREATE TABLE IF NOT EXISTS previews (
                photo_id TEXT PRIMARY KEY,
                mtime INTEGER NOT NULL,
                size INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS metadata (
                photo_id TEXT PRIMARY KEY,
                makernotes_json TEXT NOT NULL
            );
            "#,
        )?;
        // v2: metadata switched from numeric (-n) to readable values.
        // v3: a stale pre-v2 instance re-cached numeric rows into the shared
        // DB after the v2 wipe (and FileSize# is now requested numerically);
        // wipe again now that instance hygiene prevents recontamination.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 3 {
            conn.execute("DELETE FROM metadata", [])?;
            conn.pragma_update(None, "user_version", 3)?;
        }
        // v4: mood/role tags (XMP dc:subject) — stored as a JSON array on the
        // photo, written through the same crash-safe queue as ratings.
        if version < 4 {
            let _ = conn.execute("ALTER TABLE photos ADD COLUMN tags TEXT", []);
            let _ = conn.execute("ALTER TABLE xmp_queue ADD COLUMN tags TEXT", []);
            conn.pragma_update(None, "user_version", 4)?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ---------- folders / scan ----------

    pub fn open_folder(&self, path: &str) -> rusqlite::Result<FolderState> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO folders (path, updated_at) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET updated_at = ?2",
            params![path, now_ms()],
        )?;
        conn.query_row(
            "SELECT id, sort, reverse, filter, cursor_photo FROM folders WHERE path = ?1",
            params![path],
            |r| {
                Ok(FolderState {
                    folder_id: r.get(0)?,
                    sort: r.get(1)?,
                    reverse: r.get::<_, i64>(2)? != 0,
                    filter: r.get(3)?,
                    cursor_photo: r.get(4)?,
                })
            },
        )
    }

    /// Upsert scanned photos. Existing verdicts are never touched; path/missing
    /// state is refreshed. Returns (rows keyed by id, ids new to the DB).
    pub fn sync_photos(
        &self,
        folder_id: i64,
        entries: &[std::sync::Arc<PhotoEntry>],
    ) -> rusqlite::Result<(HashMap<String, PhotoRow>, Vec<String>)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut existing: HashMap<String, PhotoRow> = HashMap::new();
        {
            let mut stmt =
                tx.prepare("SELECT id, rating, trashed FROM photos WHERE folder_id = ?1")?;
            let rows = stmt.query_map(params![folder_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    PhotoRow {
                        rating: r.get::<_, i64>(1)? as u8,
                        trashed: r.get::<_, i64>(2)? != 0,
                    },
                ))
            })?;
            for row in rows {
                let (id, pr) = row?;
                existing.insert(id, pr);
            }
        }
        let mut new_ids = Vec::new();
        {
            let mut insert = tx.prepare(
                "INSERT INTO photos (id, folder_id, dir, stem, jpeg_path, raf_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                   jpeg_path = excluded.jpeg_path,
                   raf_path = excluded.raf_path,
                   missing = 0",
            )?;
            for e in entries {
                if !existing.contains_key(&e.id) {
                    new_ids.push(e.id.clone());
                }
                insert.execute(params![
                    e.id,
                    folder_id,
                    e.dir.to_string_lossy(),
                    e.stem,
                    e.jpeg.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    e.raf.as_ref().map(|p| p.to_string_lossy().into_owned()),
                ])?;
            }
        }
        // Photos the scan no longer sees: missing (unless trashed — expected gone).
        {
            let scanned: std::collections::HashSet<&str> =
                entries.iter().map(|e| e.id.as_str()).collect();
            let mut stmt =
                tx.prepare("SELECT id FROM photos WHERE folder_id = ?1 AND trashed = 0")?;
            let ids: Vec<String> = stmt
                .query_map(params![folder_id], |r| r.get::<_, String>(0))?
                .filter_map(Result::ok)
                .collect();
            let mut mark = tx.prepare("UPDATE photos SET missing = 1 WHERE id = ?1")?;
            for id in ids {
                if !scanned.contains(id.as_str()) {
                    mark.execute(params![id])?;
                }
            }
        }
        tx.commit()?;
        Ok((existing, new_ids))
    }

    /// Fill in an adopted (pre-existing XMP) rating — only if still untouched.
    pub fn adopt_rating(&self, photo_id: &str, rating: u8) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE photos SET rating = ?2 WHERE id = ?1 AND rating = 0
             AND NOT EXISTS (SELECT 1 FROM actions WHERE photo_id = ?1)",
            params![photo_id, rating],
        )?;
        Ok(n > 0)
    }

    pub fn save_view(
        &self,
        folder_id: i64,
        sort: &str,
        reverse: bool,
        filter: &str,
        cursor: Option<&str>,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE folders SET sort=?2, reverse=?3, filter=?4, cursor_photo=?5, updated_at=?6
             WHERE id=?1",
            params![folder_id, sort, reverse as i64, filter, cursor, now_ms()],
        )?;
        Ok(())
    }

    // ---------- verdicts ----------

    pub fn set_rating(&self, folder_id: i64, photo_id: &str, to: u8) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let (from, jpeg, raf): (u8, Option<String>, Option<String>) = tx.query_row(
            "SELECT rating, jpeg_path, raf_path FROM photos WHERE id = ?1",
            params![photo_id],
            |r| Ok((r.get::<_, i64>(0)? as u8, r.get(1)?, r.get(2)?)),
        )?;
        tx.execute(
            "UPDATE photos SET rating = ?2 WHERE id = ?1",
            params![photo_id, to],
        )?;
        tx.execute(
            "DELETE FROM actions WHERE folder_id = ?1 AND undone = 1",
            params![folder_id],
        )?;
        tx.execute(
            "INSERT INTO actions (folder_id, ts, photo_id, kind, payload)
             VALUES (?1, ?2, ?3, 'rate', ?4)",
            params![
                folder_id,
                now_ms(),
                photo_id,
                format!(r#"{{"from":{from},"to":{to}}}"#)
            ],
        )?;
        tx.execute(
            "INSERT INTO xmp_queue (photo_id, jpeg_path, raf_path, rating)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(photo_id) DO UPDATE SET
               rating = excluded.rating, status = 'pending', attempts = 0, last_error = NULL",
            params![photo_id, jpeg, raf, to],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Set mood/role tags. Journaled like every verdict; queued to XMP as
    /// dc:subject alongside the current rating.
    pub fn set_tags(
        &self,
        folder_id: i64,
        photo_id: &str,
        to: &[String],
    ) -> rusqlite::Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let (from_json, rating, jpeg, raf): (Option<String>, i64, Option<String>, Option<String>) =
            tx.query_row(
                "SELECT tags, rating, jpeg_path, raf_path FROM photos WHERE id = ?1",
                params![photo_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        let from: Vec<String> = from_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        let to_json = serde_json::to_string(to).unwrap();
        tx.execute(
            "UPDATE photos SET tags = ?2 WHERE id = ?1",
            params![photo_id, to_json],
        )?;
        tx.execute(
            "DELETE FROM actions WHERE folder_id = ?1 AND undone = 1",
            params![folder_id],
        )?;
        tx.execute(
            "INSERT INTO actions (folder_id, ts, photo_id, kind, payload)
             VALUES (?1, ?2, ?3, 'tags', ?4)",
            params![
                folder_id,
                now_ms(),
                photo_id,
                serde_json::json!({ "from": from, "to": to }).to_string()
            ],
        )?;
        tx.execute(
            "INSERT INTO xmp_queue (photo_id, jpeg_path, raf_path, rating, tags)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(photo_id) DO UPDATE SET
               tags = excluded.tags, status = 'pending', attempts = 0, last_error = NULL",
            params![photo_id, jpeg, raf, rating, to_json],
        )?;
        tx.commit()?;
        Ok(from)
    }

    pub fn photo_paths(
        &self,
        photo_id: &str,
    ) -> rusqlite::Result<(i64, Option<PathBuf>, Option<PathBuf>)> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT folder_id, jpeg_path, raf_path FROM photos WHERE id = ?1 AND trashed = 0",
            params![photo_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<String>>(1)?.map(PathBuf::from),
                    r.get::<_, Option<String>>(2)?.map(PathBuf::from),
                ))
            },
        )
    }

    pub fn record_trash(
        &self,
        folder_id: i64,
        photo_id: &str,
        payload: &TrashPayload,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE photos SET trashed = 1, trashed_jpeg = ?2, trashed_raf = ?3, trashed_at = ?4
             WHERE id = ?1",
            params![photo_id, payload.tjpeg, payload.traf, now_ms()],
        )?;
        tx.execute(
            "DELETE FROM actions WHERE folder_id = ?1 AND undone = 1",
            params![folder_id],
        )?;
        tx.execute(
            "INSERT INTO actions (folder_id, ts, photo_id, kind, payload)
             VALUES (?1, ?2, ?3, 'trash', ?4)",
            params![
                folder_id,
                now_ms(),
                photo_id,
                serde_json::to_string(payload).unwrap()
            ],
        )?;
        // A trashed photo needs no pending XMP write.
        tx.execute(
            "DELETE FROM xmp_queue WHERE photo_id = ?1",
            params![photo_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_restore(
        &self,
        folder_id: i64,
        photo_id: &str,
        payload: &TrashPayload,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE photos SET trashed = 0, trashed_jpeg = NULL, trashed_raf = NULL, trashed_at = NULL
             WHERE id = ?1",
            params![photo_id],
        )?;
        tx.execute(
            "DELETE FROM actions WHERE folder_id = ?1 AND undone = 1",
            params![folder_id],
        )?;
        tx.execute(
            "INSERT INTO actions (folder_id, ts, photo_id, kind, payload)
             VALUES (?1, ?2, ?3, 'restore', ?4)",
            params![
                folder_id,
                now_ms(),
                photo_id,
                serde_json::to_string(payload).unwrap()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn trashed_list(&self, folder_id: i64) -> rusqlite::Result<Vec<TrashedPhoto>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, stem, rating, trashed_at FROM photos
             WHERE folder_id = ?1 AND trashed = 1 ORDER BY trashed_at DESC",
        )?;
        let rows = stmt.query_map(params![folder_id], |r| {
            Ok(TrashedPhoto {
                id: r.get(0)?,
                stem: r.get(1)?,
                rating: r.get::<_, i64>(2)? as u8,
                trashed_at: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            })
        })?;
        rows.collect()
    }

    pub fn missing_count(&self, folder_id: i64) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM photos WHERE folder_id = ?1 AND missing = 1 AND trashed = 0",
            params![folder_id],
            |r| r.get::<_, i64>(0).map(|n| n as usize),
        )
    }

    /// Photos a rescan no longer sees on disk — kept visible in the list so
    /// they degrade with a badge instead of vanishing (spec §9).
    pub fn missing_photos(&self, folder_id: i64) -> rusqlite::Result<Vec<MissingPhoto>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, dir, stem, jpeg_path, raf_path, rating FROM photos
             WHERE folder_id = ?1 AND missing = 1 AND trashed = 0
             ORDER BY stem",
        )?;
        let rows = stmt.query_map(params![folder_id], |r| {
            Ok(MissingPhoto {
                id: r.get(0)?,
                dir: r.get(1)?,
                stem: r.get(2)?,
                has_jpeg: r.get::<_, Option<String>>(3)?.is_some(),
                has_raf: r.get::<_, Option<String>>(4)?.is_some(),
                rating: r.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn trash_info(&self, photo_id: &str) -> rusqlite::Result<TrashPayload> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT jpeg_path, raf_path, trashed_jpeg, trashed_raf FROM photos
             WHERE id = ?1 AND trashed = 1",
            params![photo_id],
            |r| {
                Ok(TrashPayload {
                    jpeg: r.get(0)?,
                    raf: r.get(1)?,
                    tjpeg: r.get(2)?,
                    traf: r.get(3)?,
                })
            },
        )
    }

    // ---------- undo / redo ----------

    pub fn peek_undo(&self, folder_id: i64) -> rusqlite::Result<Option<ActionRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT seq, photo_id, kind, payload FROM actions
             WHERE folder_id = ?1 AND undone = 0 ORDER BY seq DESC LIMIT 1",
            params![folder_id],
            |r| {
                Ok(ActionRow {
                    seq: r.get(0)?,
                    photo_id: r.get(1)?,
                    kind: r.get(2)?,
                    payload: r.get(3)?,
                })
            },
        )
        .optional()
    }

    pub fn peek_redo(&self, folder_id: i64) -> rusqlite::Result<Option<ActionRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT seq, photo_id, kind, payload FROM actions
             WHERE folder_id = ?1 AND undone = 1 ORDER BY seq ASC LIMIT 1",
            params![folder_id],
            |r| {
                Ok(ActionRow {
                    seq: r.get(0)?,
                    photo_id: r.get(1)?,
                    kind: r.get(2)?,
                    payload: r.get(3)?,
                })
            },
        )
        .optional()
    }

    /// Flip an action's undone flag and apply the photo-row consequence.
    /// FS side effects must already have happened.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_flip(
        &self,
        seq: i64,
        undone: bool,
        photo_id: &str,
        rating: Option<u8>,
        tags: Option<&[String]>,
        trash: Option<(&TrashPayload, bool)>, // (payload, now_trashed)
        new_payload: Option<&str>,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE actions SET undone = ?2 WHERE seq = ?1",
            params![seq, undone as i64],
        )?;
        if let Some(p) = new_payload {
            tx.execute(
                "UPDATE actions SET payload = ?2 WHERE seq = ?1",
                params![seq, p],
            )?;
        }
        if let Some(r) = rating {
            tx.execute(
                "UPDATE photos SET rating = ?2 WHERE id = ?1",
                params![photo_id, r],
            )?;
            let (jpeg, raf): (Option<String>, Option<String>) = tx.query_row(
                "SELECT jpeg_path, raf_path FROM photos WHERE id = ?1",
                params![photo_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            tx.execute(
                "INSERT INTO xmp_queue (photo_id, jpeg_path, raf_path, rating)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(photo_id) DO UPDATE SET
                   rating = excluded.rating, status = 'pending', attempts = 0, last_error = NULL",
                params![photo_id, jpeg, raf, r],
            )?;
        }
        if let Some(t) = tags {
            let to_json = serde_json::to_string(t).unwrap();
            tx.execute(
                "UPDATE photos SET tags = ?2 WHERE id = ?1",
                params![photo_id, to_json],
            )?;
            let (jpeg, raf, rating): (Option<String>, Option<String>, i64) = tx.query_row(
                "SELECT jpeg_path, raf_path, rating FROM photos WHERE id = ?1",
                params![photo_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            tx.execute(
                "INSERT INTO xmp_queue (photo_id, jpeg_path, raf_path, rating, tags)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(photo_id) DO UPDATE SET
                   tags = excluded.tags, status = 'pending', attempts = 0, last_error = NULL",
                params![photo_id, jpeg, raf, rating, to_json],
            )?;
        }
        if let Some((payload, now_trashed)) = trash {
            if now_trashed {
                tx.execute(
                    "UPDATE photos SET trashed = 1, trashed_jpeg = ?2, trashed_raf = ?3, trashed_at = ?4
                     WHERE id = ?1",
                    params![photo_id, payload.tjpeg, payload.traf, now_ms()],
                )?;
                tx.execute(
                    "DELETE FROM xmp_queue WHERE photo_id = ?1",
                    params![photo_id],
                )?;
            } else {
                tx.execute(
                    "UPDATE photos SET trashed = 0, trashed_jpeg = NULL, trashed_raf = NULL, trashed_at = NULL
                     WHERE id = ?1",
                    params![photo_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---------- xmp queue ----------

    pub fn xmp_take_batch(&self, limit: usize) -> rusqlite::Result<Vec<XmpJob>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT photo_id, jpeg_path, raf_path, rating, tags FROM xmp_queue
             WHERE status = 'pending' AND attempts < 5 LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let tags: Option<String> = r.get(4)?;
            Ok(XmpJob {
                photo_id: r.get(0)?,
                jpeg: r.get(1)?,
                raf: r.get(2)?,
                rating: r.get::<_, i64>(3)? as u8,
                tags: tags.and_then(|t| serde_json::from_str(&t).ok()),
            })
        })?;
        rows.collect()
    }

    /// Remove a completed job — but only if the rating hasn't changed since it
    /// was taken (a newer keypress re-queues with a different value).
    pub fn xmp_done(&self, photo_id: &str, written_rating: u8) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM xmp_queue WHERE photo_id = ?1 AND rating = ?2",
            params![photo_id, written_rating],
        )?;
        Ok(())
    }

    pub fn xmp_error(&self, photo_id: &str, err: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE xmp_queue SET attempts = attempts + 1, last_error = ?2,
             status = CASE WHEN attempts + 1 >= 5 THEN 'error' ELSE 'pending' END
             WHERE photo_id = ?1",
            params![photo_id, err],
        )?;
        Ok(())
    }

    /// Truthful queue status for the HUD: `pending` counts only rows the
    /// worker will still attempt; `failed` are parked after 5 attempts and
    /// stay visible (with their error) until retried.
    pub fn xmp_status(&self) -> rusqlite::Result<XmpStatus> {
        let conn = self.conn.lock().unwrap();
        let (pending, failed) = conn.query_row(
            "SELECT
               COUNT(*) FILTER (WHERE status = 'pending'),
               COUNT(*) FILTER (WHERE status = 'error')
             FROM xmp_queue",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let last_error = if failed > 0 {
            conn.query_row(
                "SELECT last_error FROM xmp_queue
                 WHERE status = 'error' AND last_error IS NOT NULL LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
        } else {
            None
        };
        Ok(XmpStatus {
            pending,
            failed,
            last_error,
        })
    }

    /// Re-arm every permanently-failed write for another round of attempts.
    pub fn xmp_retry_errors(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE xmp_queue SET status = 'pending', attempts = 0 WHERE status = 'error'",
            [],
        )
    }

    // ---------- metadata cache ----------

    pub fn metadata_json(&self, photo_id: &str) -> rusqlite::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT makernotes_json FROM metadata WHERE photo_id = ?1",
            params![photo_id],
            |r| r.get(0),
        )
        .optional()
    }

    pub fn set_metadata_json(&self, photo_id: &str, json: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO metadata (photo_id, makernotes_json) VALUES (?1, ?2)
             ON CONFLICT(photo_id) DO UPDATE SET makernotes_json = excluded.makernotes_json",
            params![photo_id, json],
        )?;
        Ok(())
    }

    pub fn metadata_for_folder(&self, folder_id: i64) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.photo_id, m.makernotes_json FROM metadata m
             JOIN photos p ON p.id = m.photo_id
             WHERE p.folder_id = ?1 AND p.trashed = 0",
        )?;
        let rows = stmt.query_map(params![folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    pub fn metadata_ids(&self) -> rusqlite::Result<std::collections::HashSet<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT photo_id FROM metadata")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    // ---------- preview validity ----------

    pub fn preview_stat(&self, photo_id: &str) -> rusqlite::Result<Option<(u64, u64)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT mtime, size FROM previews WHERE photo_id = ?1",
            params![photo_id],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        )
        .optional()
    }

    pub fn set_preview_stat(&self, photo_id: &str, mtime: u64, size: u64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO previews (photo_id, mtime, size) VALUES (?1, ?2, ?3)
             ON CONFLICT(photo_id) DO UPDATE SET mtime = excluded.mtime, size = excluded.size",
            params![photo_id, mtime as i64, size as i64],
        )?;
        Ok(())
    }

    pub fn clear_preview(&self, photo_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM previews WHERE photo_id = ?1",
            params![photo_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::PhotoEntry;
    use std::sync::Arc;

    fn mem_store() -> Store {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        // Reuse schema by round-tripping through Store::new on a temp file is
        // clumsier than duplicating; instead open a temp-file store.
        drop(conn);
        let dir = std::env::temp_dir().join(format!(
            "emberstore-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::new(&dir.join("test.sqlite3")).unwrap()
    }

    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
    }

    fn entry(id: &str, stem: &str) -> Arc<PhotoEntry> {
        Arc::new(PhotoEntry {
            id: id.into(),
            dir: "/tmp/x".into(),
            stem: stem.into(),
            jpeg: Some(format!("/tmp/x/{stem}.JPG").into()),
            raf: None,
            mtime: 1,
            size: 1,
        })
    }

    #[test]
    fn missing_photos_marked_listed_and_cleared() {
        let s = mem_store();
        let f = s.open_folder("/tmp/x").unwrap();
        s.sync_photos(f.folder_id, &[entry("a", "A"), entry("b", "B")])
            .unwrap();
        s.set_rating(f.folder_id, "b", 4).unwrap();

        // Rescan sees only "a": "b" goes missing but stays queryable.
        s.sync_photos(f.folder_id, &[entry("a", "A")]).unwrap();
        let missing = s.missing_photos(f.folder_id).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, "b");
        assert_eq!(missing[0].rating, 4, "verdict survives the absence");
        assert!(missing[0].has_jpeg && !missing[0].has_raf);

        // File returns: flag clears.
        s.sync_photos(f.folder_id, &[entry("a", "A"), entry("b", "B")])
            .unwrap();
        assert!(s.missing_photos(f.folder_id).unwrap().is_empty());
    }

    #[test]
    fn rate_undo_redo_branch_cut() {
        let s = mem_store();
        let f = s.open_folder("/tmp/x").unwrap();
        let (_, new_ids) = s
            .sync_photos(f.folder_id, &[entry("a", "A"), entry("b", "B")])
            .unwrap();
        assert_eq!(new_ids.len(), 2);

        s.set_rating(f.folder_id, "a", 3).unwrap();
        s.set_rating(f.folder_id, "a", 5).unwrap();

        // undo 5 → back to 3
        let act = s.peek_undo(f.folder_id).unwrap().unwrap();
        assert_eq!(act.kind, "rate");
        s.finish_flip(act.seq, true, "a", Some(3), None, None, None)
            .unwrap();

        // redo → 5 again
        let act = s.peek_redo(f.folder_id).unwrap().unwrap();
        s.finish_flip(act.seq, false, "a", Some(5), None, None, None)
            .unwrap();
        assert!(s.peek_redo(f.folder_id).unwrap().is_none());

        // undo, then a NEW action cuts the redo branch
        let act = s.peek_undo(f.folder_id).unwrap().unwrap();
        s.finish_flip(act.seq, true, "a", Some(3), None, None, None)
            .unwrap();
        s.set_rating(f.folder_id, "b", 2).unwrap();
        assert!(
            s.peek_redo(f.folder_id).unwrap().is_none(),
            "redo branch must be cut"
        );
    }

    #[test]
    fn adoption_never_clobbers_user_action() {
        let s = mem_store();
        let f = s.open_folder("/tmp/x").unwrap();
        s.sync_photos(f.folder_id, &[entry("a", "A")]).unwrap();
        s.set_rating(f.folder_id, "a", 4).unwrap();
        assert!(
            !s.adopt_rating("a", 2).unwrap(),
            "adoption must not override a user rating"
        );
    }

    #[test]
    fn trash_survives_and_lists() {
        let s = mem_store();
        let f = s.open_folder("/tmp/x").unwrap();
        s.sync_photos(f.folder_id, &[entry("a", "A")]).unwrap();
        s.set_rating(f.folder_id, "a", 1).unwrap();
        let payload = TrashPayload {
            jpeg: Some("/tmp/x/A.JPG".into()),
            raf: None,
            tjpeg: Some("/Users/t/.Trash/A.JPG".into()),
            traf: None,
        };
        s.record_trash(f.folder_id, "a", &payload).unwrap();
        let listed = s.trashed_list(f.folder_id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].stem, "A");
        // xmp queue purged for trashed photo
        assert_eq!(s.xmp_status().unwrap().pending, 0);
    }

    #[test]
    fn xmp_status_is_truthful_and_retry_rearms() {
        let s = mem_store();
        let f = s.open_folder("/tmp/x").unwrap();
        s.sync_photos(f.folder_id, &[entry("a", "A"), entry("b", "B")])
            .unwrap();
        s.set_rating(f.folder_id, "a", 3).unwrap();
        s.set_rating(f.folder_id, "b", 4).unwrap();
        assert_eq!(s.xmp_status().unwrap().pending, 2);

        // Five failed attempts park the job as 'error' — it must leave
        // `pending` (the worker will never take it again) and show up in
        // `failed` with its error message.
        for _ in 0..5 {
            s.xmp_error("a", "exiftool spawn: not found").unwrap();
        }
        let st = s.xmp_status().unwrap();
        assert_eq!((st.pending, st.failed), (1, 1));
        assert_eq!(st.last_error.as_deref(), Some("exiftool spawn: not found"));
        assert!(
            s.xmp_take_batch(8)
                .unwrap()
                .iter()
                .all(|j| j.photo_id != "a"),
            "errored job must not be retaken"
        );

        // Retry re-arms it.
        assert_eq!(s.xmp_retry_errors().unwrap(), 1);
        let st = s.xmp_status().unwrap();
        assert_eq!((st.pending, st.failed), (2, 0));
        assert_eq!(st.last_error, None);
    }
}
