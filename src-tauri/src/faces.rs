//! Faces — engine + background index worker (SPEC §14, plan rev 4).
//!
//! One worker thread, lowest priority in spirit: only photos whose preview
//! already exists, cursor-prioritized, its own SQLite connection (never the
//! command mutex). Every result lands through `facestore::commit_scan`, an
//! optimistic conditional write — correctness comes from the DB (epoch /
//! enabled / generation / face_revision guards), because a second Ember
//! process may share this DB during gate runs.
//!
//! `EMBER_FACES_FORCE=1` (gate harness) keeps the worker reprocessing in
//! continuous passes so a flip storm always measures against live inference —
//! including the real commit path. Same code, no separate spike.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;
use sha2::{Digest, Sha256};
use tauri::Emitter;

use crate::facedet;
use crate::facestore::{self, CommitOutcome, NewFace};
use crate::preview::PreviewState;
use crate::settings::FacesCfg;

/// Pinned model assets (opencv_zoo). The worker registers (det, rec, prep)
/// as a model generation at init — every embedding row self-describes its
/// space, and a changed hash triggers the progressive-reindex path.
pub const YUNET_FILE: &str = "face_detection_yunet_2023mar.onnx";
pub const YUNET_SHA256: &str = "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4";
pub const SFACE_FILE: &str = "face_recognition_sface_2021dec.onnx";
pub const SFACE_SHA256: &str = "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79";
/// Bump when preprocessing (letterbox, alignment, blob layout) changes in a
/// way that shifts embeddings — it is part of the generation identity.
pub const PREP_VERSION: i64 = 1;

/// Chip bake parameters (square crop, +25% margin per side via chip_crop).
const CHIP_SIZE: u32 = 160;
const CHIP_QUALITY: u8 = 80;

/// In-session retry backoff for per-photo errors; `face_scan.attempts` is the
/// lifetime diagnostic, this is the session policy.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(180),
];

// ---------- worker stats (gate harness reads these via faces_spike_stats) ----------

static PHOTOS: AtomicU64 = AtomicU64::new(0);
static FACES: AtomicU64 = AtomicU64::new(0);
static PASSES: AtomicU64 = AtomicU64::new(0);
static DECODE_US: AtomicU64 = AtomicU64::new(0);
static DETECT_US: AtomicU64 = AtomicU64::new(0);
static EMBED_US: AtomicU64 = AtomicU64::new(0);
static INIT_MS: AtomicU64 = AtomicU64::new(0);
static ERRORS: AtomicU64 = AtomicU64::new(0);
static DISCARDS: AtomicU64 = AtomicU64::new(0);

/// Terminal model-init failure — surfaced as a non-blocking People-panel
/// notice; the app is otherwise unaffected.
static ENGINE_ERROR: Mutex<Option<String>> = Mutex::new(None);
static STOP: AtomicBool = AtomicBool::new(false);
static IDLE: AtomicBool = AtomicBool::new(true);

pub fn force_enabled() -> bool {
    std::env::var("EMBER_FACES_FORCE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

pub fn engine_error() -> Option<String> {
    ENGINE_ERROR.lock().unwrap().clone()
}

/// App exit: park the worker between photos so the ONNX runtime is never torn
/// down mid-inference (that logged a scary error at quit in Slice 0).
pub fn stop_and_wait(timeout: Duration) {
    STOP.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + timeout;
    while !IDLE.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn spike_stats() -> serde_json::Value {
    let photos = PHOTOS.load(Ordering::Relaxed);
    let us = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1000.0 / photos.max(1) as f64;
    serde_json::json!({
        "photos": photos,
        "faces": FACES.load(Ordering::Relaxed),
        "passes": PASSES.load(Ordering::Relaxed),
        "errors": ERRORS.load(Ordering::Relaxed),
        "discards": DISCARDS.load(Ordering::Relaxed),
        "avgDecodeMs": us(&DECODE_US),
        "avgDetectMs": us(&DETECT_US),
        "avgEmbedMs": us(&EMBED_US),
        "avgTotalMs": us(&DECODE_US) + us(&DETECT_US) + us(&EMBED_US),
        "initMs": INIT_MS.load(Ordering::Relaxed),
        "rssMb": rss_mb(),
    })
}

fn rss_mb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

// ---------- engine ----------

pub struct FaceResult {
    /// Normalized [0,1] display-space rect (previews are orientation-applied).
    pub rect: [f32; 4],
    pub score: f32,
    /// Preview-pixel landmarks — not persisted (alignment is recomputed from
    /// rects when needed); kept for the fixture tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub landmarks: [[f32; 2]; 5],
    /// L2-normalized 128-d SFace embedding.
    pub embedding: Vec<f32>,
}

pub struct FaceEngine {
    det: Session,
    rec: Session,
    min_det_score: f32,
    pub det_sha: String,
    pub rec_sha: String,
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

impl FaceEngine {
    pub fn new(models_dir: &Path, min_det_score: f32) -> Result<Self, String> {
        let t0 = Instant::now();
        let det_sha = sha256_hex(&models_dir.join(YUNET_FILE))?;
        let rec_sha = sha256_hex(&models_dir.join(SFACE_FILE))?;
        for (file, actual, pinned) in [
            (YUNET_FILE, &det_sha, YUNET_SHA256),
            (SFACE_FILE, &rec_sha, SFACE_SHA256),
        ] {
            if actual != pinned {
                eprintln!("faces: WARNING {file} sha256 {actual} != pinned {pinned}");
            }
        }
        let session = |file: &str| -> Result<Session, String> {
            Session::builder()
                .map_err(|e| e.to_string())?
                .with_intra_threads(1)
                .map_err(|e| e.to_string())?
                .commit_from_file(models_dir.join(file))
                .map_err(|e| format!("{file}: {e}"))
        };
        let engine = Self {
            det: session(YUNET_FILE)?,
            rec: session(SFACE_FILE)?,
            min_det_score,
            det_sha,
            rec_sha,
        };
        INIT_MS.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
        Ok(engine)
    }

    /// Full pipeline on one orientation-applied preview: letterbox → YuNet →
    /// NMS → per face: Umeyama-align 112×112 from the full preview → SFace.
    pub fn detect_embed(&mut self, preview: &RgbImage) -> Result<Vec<FaceResult>, String> {
        let (pw, ph) = preview.dimensions();
        let lb = facedet::letterbox(pw, ph);
        let scaled = crate::preview::resize_rgb(preview, lb.scaled_w, lb.scaled_h)?;
        let tensor = Tensor::from_array((
            [1usize, 3, facedet::YUNET_INPUT, facedet::YUNET_INPUT],
            facedet::yunet_tensor(&scaled),
        ))
        .map_err(|e| e.to_string())?;

        let t_det = Instant::now();
        let outputs = self
            .det
            .run(ort::inputs!["input" => tensor])
            .map_err(|e| e.to_string())?;
        let mut dets = Vec::new();
        for stride in facedet::YUNET_STRIDES {
            let plane = |name: &str| -> Result<&[f32], String> {
                outputs[format!("{name}_{stride}")]
                    .try_extract_tensor::<f32>()
                    .map(|(_, data)| data)
                    .map_err(|e| e.to_string())
            };
            dets.extend(facedet::decode_stride(
                plane("cls")?,
                plane("obj")?,
                plane("bbox")?,
                plane("kps")?,
                stride,
                self.min_det_score,
            ));
        }
        let kept = facedet::nms(dets, facedet::YUNET_NMS_IOU);
        DETECT_US.fetch_add(t_det.elapsed().as_micros() as u64, Ordering::Relaxed);

        let t_emb = Instant::now();
        let mut results = Vec::with_capacity(kept.len());
        for det in &kept {
            let (rect, landmarks) = facedet::to_preview_space(det, lb, pw, ph);
            let warp = Tensor::from_array((
                [1usize, 3, facedet::SFACE_INPUT, facedet::SFACE_INPUT],
                facedet::warp_112(preview, &facedet::umeyama_112(&landmarks)),
            ))
            .map_err(|e| e.to_string())?;
            let out = self
                .rec
                .run(ort::inputs!["data" => warp])
                .map_err(|e| e.to_string())?;
            let (_, emb) = out["fc1"]
                .try_extract_tensor::<f32>()
                .map_err(|e| e.to_string())?;
            let mut embedding = emb.to_vec();
            facedet::l2_normalize(&mut embedding);
            results.push(FaceResult {
                rect,
                score: det.score,
                landmarks,
                embedding,
            });
        }
        EMBED_US.fetch_add(t_emb.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(results)
    }
}

/// Bundled models: resource dir in a .app, the source tree in dev.
pub fn models_dir(resource_dir: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = resource_dir {
        let d = dir.join("models");
        if d.join(YUNET_FILE).exists() {
            return d;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("models")
}

// ---------- chips ----------

/// Bake `{id}-f{n}.jpg` chips from the already-decoded preview and drop
/// leftovers past the new face count. Chips are person-agnostic crops —
/// assignment changes never invalidate them.
fn bake_chips(
    preview: &PreviewState,
    photo_id: &str,
    img: &RgbImage,
    rects: &[[f32; 4]],
    old_count: i64,
) {
    let (pw, ph) = img.dimensions();
    for (n, rect) in rects.iter().enumerate() {
        let (x, y, side) = facedet::chip_crop(*rect, pw, ph);
        let crop = image::imageops::crop_imm(img, x, y, side, side).to_image();
        let small = match crate::preview::resize_rgb(&crop, CHIP_SIZE, CHIP_SIZE) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let out = preview.face_chip_path(photo_id, n as i64);
        let _ = crate::preview::write_jpeg(&small, &out, CHIP_QUALITY);
    }
    for n in rects.len() as i64..old_count {
        let _ = std::fs::remove_file(preview.face_chip_path(photo_id, n));
    }
}

/// Chip repair: the face/ route 404s on a missing chip and enqueues the photo
/// here. One dedicated thread (never the protocol pool), deduplicated by
/// photo id, one preview decode regenerates ALL of that photo's missing
/// chips. Also self-heals a crash between commit and bake, and a wiped
/// cache dir (documented "safe to delete").
pub struct ChipRepair {
    tx: crossbeam_channel::Sender<String>,
    inflight: Arc<Mutex<HashSet<String>>>,
}

impl ChipRepair {
    pub fn enqueue(&self, photo_id: &str) {
        let mut inflight = self.inflight.lock().unwrap();
        if inflight.insert(photo_id.to_string()) {
            let _ = self.tx.send(photo_id.to_string());
        }
    }
}

/// Repair handle wired to a bare channel — protocol tests observe enqueues
/// without a store or a thread.
#[cfg(test)]
pub fn test_repair() -> (Arc<ChipRepair>, crossbeam_channel::Receiver<String>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    (
        Arc::new(ChipRepair {
            tx,
            inflight: Arc::new(Mutex::new(HashSet::new())),
        }),
        rx,
    )
}

pub fn spawn_chip_repair(
    preview: Arc<PreviewState>,
    store: Arc<crate::store::Store>,
) -> Arc<ChipRepair> {
    let (tx, rx) = crossbeam_channel::unbounded::<String>();
    let inflight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let repair = Arc::new(ChipRepair {
        tx,
        inflight: inflight.clone(),
    });
    std::thread::Builder::new()
        .name("face-chip-repair".into())
        .spawn(move || {
            while let Ok(photo_id) = rx.recv() {
                repair_photo(&preview, &store, &photo_id);
                inflight.lock().unwrap().remove(&photo_id);
            }
        })
        .expect("spawn face-chip-repair worker");
    repair
}

/// One decode regenerates every missing chip of the photo.
fn repair_photo(preview: &PreviewState, store: &crate::store::Store, photo_id: &str) {
    let rects = store.face_rects(photo_id).unwrap_or_default();
    let missing: Vec<_> = rects
        .iter()
        .filter(|(n, _)| !preview.face_chip_path(photo_id, *n).exists())
        .collect();
    if missing.is_empty() {
        return;
    }
    let Some(img) = std::fs::read(preview.preview_path_for_id(photo_id))
        .ok()
        .and_then(|b| image::load_from_memory(&b).ok())
        .map(|i| i.to_rgb8())
    else {
        return;
    };
    let (pw, ph) = img.dimensions();
    for (n, rect) in missing {
        let (x, y, side) = facedet::chip_crop(*rect, pw, ph);
        let crop = image::imageops::crop_imm(&img, x, y, side, side).to_image();
        if let Ok(small) = crate::preview::resize_rgb(&crop, CHIP_SIZE, CHIP_SIZE) {
            let out = preview.face_chip_path(photo_id, *n);
            let _ = crate::preview::write_jpeg(&small, &out, CHIP_QUALITY);
        }
    }
}

// ---------- index worker ----------

fn display_stat(entry: &crate::scanner::PhotoEntry) -> Option<(i64, i64)> {
    let file = entry.display_file()?;
    let meta = std::fs::metadata(file).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((mtime, meta.len() as i64))
}

pub fn spawn_worker(
    app: tauri::AppHandle,
    preview: Arc<PreviewState>,
    db_path: PathBuf,
    models_dir: PathBuf,
    cfg: FacesCfg,
) {
    std::thread::Builder::new()
        .name("faces".into())
        .spawn(move || {
            let mut conn = match open_worker_conn(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    *ENGINE_ERROR.lock().unwrap() = Some(format!("face DB open failed: {e}"));
                    return;
                }
            };
            let force = force_enabled();
            let mut engine: Option<FaceEngine> = None;
            let mut gen: i64 = 0;
            // Session retry state: photo_id → (session attempts, next retry).
            let mut backoff: HashMap<String, (u32, Instant)> = HashMap::new();
            // Force-mode pass tracking (gate harness).
            let mut pass_done: HashSet<String> = HashSet::new();
            let mut batch: Vec<String> = Vec::new();
            loop {
                if STOP.load(Ordering::SeqCst) {
                    IDLE.store(true, Ordering::SeqCst);
                    return;
                }
                IDLE.store(false, Ordering::SeqCst);
                // DB-authoritative enabled state, checked BEFORE claiming any
                // job: a requeued job cannot resume under a new epoch, in this
                // process or the other one (round 3).
                if !facestore::index_enabled(&conn).unwrap_or(false) {
                    if engine.is_some() {
                        engine = None; // drop ONNX sessions while parked
                        backoff.clear();
                        eprintln!("faces: indexing disabled — worker parked");
                    }
                    flush_progress(&app, &conn, &mut batch);
                    IDLE.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(1000));
                    continue;
                }
                let now = Instant::now();
                let job = preview.entries_by_distance().into_iter().find(|e| {
                    if !preview.preview_path_for_id(&e.id).exists() {
                        return false;
                    }
                    if let Some((attempts, next)) = backoff.get(&e.id) {
                        if *attempts >= RETRY_DELAYS.len() as u32 || now < *next {
                            return false;
                        }
                    }
                    if force && !pass_done.contains(&e.id) {
                        return true;
                    }
                    match facestore::scan_row(&conn, &e.id) {
                        Ok(Some(row)) => match row.status.as_str() {
                            "stale" => true,
                            "error" => backoff.contains_key(&e.id),
                            // Compare against a FRESH stat, not the scan-time
                            // entry stat: XMP rewrites (ratings) bump the file
                            // and refresh face_scan — matching fresh-vs-row
                            // means "no work"; the stale entry stat would
                            // re-index that photo forever.
                            _ => {
                                row.model_gen != gen
                                    || display_stat(e)
                                        .map(|s| s != (row.mtime, row.size))
                                        .unwrap_or(false)
                            }
                        },
                        Ok(None) => true,
                        Err(_) => false,
                    }
                });
                let Some(entry) = job else {
                    flush_progress(&app, &conn, &mut batch);
                    if force && !pass_done.is_empty() {
                        PASSES.fetch_add(1, Ordering::Relaxed);
                        pass_done.clear();
                        continue;
                    }
                    IDLE.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                };
                if engine.is_none() {
                    match FaceEngine::new(&models_dir, cfg.min_det_score) {
                        Ok(e) => {
                            match facestore::ensure_gen(
                                &mut conn,
                                &e.det_sha,
                                &e.rec_sha,
                                PREP_VERSION,
                            ) {
                                Ok(g) => {
                                    eprintln!(
                                        "faces: sessions ready in {}ms (gen {g}), rss={}MB",
                                        INIT_MS.load(Ordering::Relaxed),
                                        rss_mb()
                                    );
                                    gen = g;
                                    engine = Some(e);
                                }
                                Err(err) => {
                                    *ENGINE_ERROR.lock().unwrap() =
                                        Some(format!("model registration failed: {err}"));
                                    return;
                                }
                            }
                        }
                        Err(err) => {
                            // Terminal: one log + a visible non-blocking notice.
                            eprintln!("faces: model init FAILED: {err}");
                            *ENGINE_ERROR.lock().unwrap() = Some(err);
                            return;
                        }
                    }
                }
                let id = entry.id.clone();
                match process_photo(&mut conn, engine.as_mut().unwrap(), &preview, &entry, gen) {
                    Ok(outcome) => {
                        backoff.remove(&id);
                        if outcome == CommitOutcome::Discarded {
                            DISCARDS.fetch_add(1, Ordering::Relaxed);
                        }
                        batch.push(id.clone());
                    }
                    Err(e) => {
                        ERRORS.fetch_add(1, Ordering::Relaxed);
                        let _ = facestore::record_scan_error(&conn, &id, gen, &e);
                        let attempts = backoff.get(&id).map(|(a, _)| *a).unwrap_or(0);
                        let delay = RETRY_DELAYS[(attempts as usize).min(RETRY_DELAYS.len() - 1)];
                        backoff.insert(id.clone(), (attempts + 1, Instant::now() + delay));
                        eprintln!("faces: {id} error: {e}");
                    }
                }
                if force {
                    pass_done.insert(id);
                }
                let n = PHOTOS.fetch_add(1, Ordering::Relaxed) + 1;
                if batch.len() >= 4 {
                    flush_progress(&app, &conn, &mut batch);
                }
                if n.is_multiple_of(25) {
                    eprintln!("faces: {}", spike_stats());
                }
            }
        })
        .expect("spawn faces worker");
}

fn open_worker_conn(db_path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db_path)?;
    // Derived data on WAL: NORMAL is durable enough and keeps this worker's
    // fsyncs from competing with rating acks on the journal connection.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(conn)
}

/// One photo through the full pipeline and the conditional commit.
fn process_photo(
    conn: &mut rusqlite::Connection,
    engine: &mut FaceEngine,
    preview: &PreviewState,
    entry: &crate::scanner::PhotoEntry,
    gen: i64,
) -> Result<CommitOutcome, String> {
    let id = &entry.id;
    let (mtime, size) = display_stat(entry).ok_or("display file missing")?;
    let snap = facestore::snapshot(conn, id, gen).map_err(|e| e.to_string())?;
    let old_count = facestore::scan_row(conn, id)
        .ok()
        .flatten()
        .map(|r| r.face_count)
        .unwrap_or(0);

    let t0 = Instant::now();
    let img = std::fs::read(preview.preview_path_for_id(id))
        .ok()
        .and_then(|b| image::load_from_memory(&b).ok())
        .map(|i| i.to_rgb8())
        .ok_or("preview unreadable")?;
    DECODE_US.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);

    let faces = engine.detect_embed(&img)?;
    FACES.fetch_add(faces.len() as u64, Ordering::Relaxed);

    // Source check: if the display file changed while we were inferring, this
    // result describes pixels that no longer exist — discard.
    if display_stat(entry) != Some((mtime, size)) {
        return Ok(CommitOutcome::Discarded);
    }
    let new: Vec<NewFace> = faces
        .iter()
        .map(|f| NewFace {
            rect: f.rect,
            det_score: f.score,
            embedding: f.embedding.clone(),
        })
        .collect();
    let outcome =
        facestore::commit_scan(conn, id, &snap, &new, mtime, size).map_err(|e| e.to_string())?;
    if outcome == CommitOutcome::Committed {
        let rects: Vec<[f32; 4]> = faces.iter().map(|f| f.rect).collect();
        bake_chips(preview, id, &img, &rects, old_count);
    }
    Ok(outcome)
}

/// Emit `faces-progress` for the batch's folder — the panel refetches status
/// and clusters on this signal.
fn flush_progress(app: &tauri::AppHandle, conn: &rusqlite::Connection, batch: &mut Vec<String>) {
    if batch.is_empty() {
        return;
    }
    let folder_id: Option<i64> = conn
        .query_row(
            "SELECT folder_id FROM photos WHERE id = ?1",
            rusqlite::params![batch[0]],
            |r| r.get(0),
        )
        .ok();
    let (scanned, total) = folder_id
        .and_then(|fid| {
            conn.query_row(
                "SELECT COUNT(s.photo_id) FILTER (WHERE s.status = 'ok'), COUNT(*)
                 FROM photos p LEFT JOIN face_scan s ON s.photo_id = p.id
                 WHERE p.folder_id = ?1 AND p.trashed = 0 AND p.missing = 0",
                rusqlite::params![fid],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok()
        })
        .unwrap_or((0, 0));
    let _ = app.emit(
        "faces-progress",
        serde_json::json!({
            "folderId": folder_id,
            "scanned": scanned,
            "total": total,
            "photoIds": std::mem::take(batch),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Cache-dir delete (documented safe) and crash-between-commit-and-bake
    /// both leave committed face rows without chips — repair must rebake from
    /// stored rects alone: no ONNX, one preview decode per photo.
    #[test]
    fn chip_repair_rebakes_from_rects_without_inference() {
        let dir = std::env::temp_dir().join(format!(
            "emberchip-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        let store = crate::store::Store::new(&dir.join("t.sqlite3")).unwrap();
        let folder = store.open_folder("/tmp/chip-x").unwrap();
        let entry = std::sync::Arc::new(crate::scanner::PhotoEntry {
            id: "p1".into(),
            dir: "/tmp/chip-x".into(),
            stem: "P1".into(),
            jpeg: Some("/tmp/chip-x/P1.JPG".into()),
            raf: None,
            mtime: 1,
            size: 1,
        });
        store.sync_photos(folder.folder_id, &[entry]).unwrap();
        {
            // Commit two faces (rows only — the "crash before bake" state).
            let mut guard = store.lock_conn();
            let conn = &mut *guard;
            let gen = crate::facestore::ensure_gen(conn, "d", "r", 1).unwrap();
            let snap = crate::facestore::snapshot(conn, "p1", gen).unwrap();
            let e = vec![0.1f32; facedet::EMBED_DIM];
            crate::facestore::commit_scan(
                conn,
                "p1",
                &snap,
                &[
                    crate::facestore::NewFace {
                        rect: [0.1, 0.1, 0.2, 0.3],
                        det_score: 0.9,
                        embedding: e.clone(),
                    },
                    crate::facestore::NewFace {
                        rect: [0.6, 0.5, 0.2, 0.3],
                        det_score: 0.85,
                        embedding: e,
                    },
                ],
                1,
                1,
            )
            .unwrap();
        }
        let preview = PreviewState::new(dir.join("cache"), crate::exposure::BlinkiesCfg::default());
        let img = RgbImage::from_pixel(400, 300, image::Rgb([90, 120, 150]));
        crate::preview::write_jpeg(&img, &preview.preview_path_for_id("p1"), 85).unwrap();

        repair_photo(&preview, &store, "p1");
        for n in 0..2 {
            let chip = preview.face_chip_path("p1", n);
            assert!(chip.exists(), "chip {n} rebaked");
            let decoded = image::open(&chip).unwrap().to_rgb8();
            assert_eq!(decoded.dimensions(), (CHIP_SIZE, CHIP_SIZE));
        }
        // Wipe one chip (partial cache delete) → only it is rebaked.
        std::fs::remove_file(preview.face_chip_path("p1", 1)).unwrap();
        repair_photo(&preview, &store, "p1");
        assert!(preview.face_chip_path("p1", 1).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Real models + a real photograph — validates the fixed 640×640
    /// letterbox strategy end to end (Slice 0 acceptance). `#[ignore]` only
    /// because it needs the 38MB model files; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn detects_known_face_and_embeds_deterministically() {
        let models = models_dir(None);
        let mut engine = FaceEngine::new(&models, 0.8).expect("models load");
        let img = image::open(fixture("astronaut.png"))
            .expect("fixture")
            .to_rgb8();
        let faces = engine.detect_embed(&img).expect("inference");
        assert!(
            !faces.is_empty(),
            "known face fixture must yield a detection"
        );
        let f = &faces[0];
        assert!(f.score >= 0.8);
        for v in f.rect {
            assert!((0.0..=1.0).contains(&v), "rect {:?}", f.rect);
        }
        let (w, h) = img.dimensions();
        let (rx, ry) = (f.rect[0] * w as f32, f.rect[1] * h as f32);
        let (rw, rh) = (f.rect[2] * w as f32, f.rect[3] * h as f32);
        for lm in f.landmarks {
            assert!(lm[0] > rx - rw * 0.25 && lm[0] < rx + rw * 1.25, "{lm:?}");
            assert!(lm[1] > ry - rh * 0.25 && lm[1] < ry + rh * 1.25, "{lm:?}");
        }
        assert_eq!(f.embedding.len(), facedet::EMBED_DIM);
        let norm: f32 = f.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
        let again = engine.detect_embed(&img).expect("second run");
        assert_eq!(faces.len(), again.len());
        let cos = facedet::cosine(&f.embedding, &again[0].embedding);
        assert!(cos > 0.9999, "same input must embed identically, cos={cos}");
    }
}
