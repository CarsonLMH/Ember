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
/// Monotonic rank of the bundled (detector, recognizer, preprocessing) set.
/// **Bump this whenever any of the three changes.** Ownership of a shared DB
/// is decided by comparing ranks — never by whether a hash has been seen
/// before, because a DB first indexed by a newer build has never seen an
/// older build's hashes and the older build must still park
/// (§facestore::claim_state).
pub const MODEL_RELEASE: i64 = 1;

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

/// Per-session retry state for photos whose scan failed — the plan's contract
/// made explicit: three tries per session on the `RETRY_DELAYS` schedule,
/// terminal only when they are spent, and **reset on relaunch** (a fresh
/// session starts with an empty map, so a persisted `error` row is eligible
/// again immediately). The DB's `face_scan.attempts` stays the lifetime
/// diagnostic; nothing here is persisted.
pub struct SessionRetries {
    map: HashMap<String, (u32, Instant)>,
}

impl SessionRetries {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// May the worker claim this photo's `error` row right now? Never failed
    /// this session (first sighting, or a relaunch) → yes. Otherwise only if
    /// tries remain and the backoff delay has elapsed — a photo waiting out
    /// its delay is skipped, not spun on, so there is no hot loop.
    pub fn eligible(&self, photo_id: &str, now: Instant) -> bool {
        match self.map.get(photo_id) {
            None => true,
            Some((attempts, next)) => *attempts < RETRY_DELAYS.len() as u32 && now >= *next,
        }
    }

    /// Record a failure. Returns true when the photo just became terminal for
    /// this session — its last try is spent.
    pub fn on_failure(&mut self, photo_id: &str, now: Instant) -> bool {
        let attempts = self.map.get(photo_id).map(|(a, _)| *a).unwrap_or(0);
        let delay = RETRY_DELAYS[(attempts as usize).min(RETRY_DELAYS.len() - 1)];
        let attempts = attempts + 1;
        self.map
            .insert(photo_id.to_string(), (attempts, now + delay));
        attempts >= RETRY_DELAYS.len() as u32
    }

    /// The photo was claimed for non-retry work (stale, pixel change, model
    /// change): its session budget starts over. Without this, a photo that
    /// exhausted its tries and was then explicitly rescanned by the user
    /// would sit pending forever with a worker that refuses to touch it.
    /// Returns whether there was state to reset.
    pub fn on_fresh_claim(&mut self, photo_id: &str) -> bool {
        self.map.remove(photo_id).is_some()
    }

    pub fn on_success(&mut self, photo_id: &str) {
        self.map.remove(photo_id);
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Photos whose session tries are spent — terminal until relaunch or a
    /// fresh claim. The status command subtracts exactly these from pending:
    /// an error row the worker will still retry is *retrying*, not failed.
    pub fn exhausted(&self) -> HashSet<String> {
        self.map
            .iter()
            .filter(|(_, (a, _))| *a >= RETRY_DELAYS.len() as u32)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

impl Default for SessionRetries {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of the worker's exhausted set, for `face_scan_status` — status
/// display only, never a correctness guard (each process reports what its own
/// worker knows, which is the worker whose retries that panel is watching).
static EXHAUSTED: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

fn publish_exhausted(retries: &SessionRetries) {
    *EXHAUSTED.lock().unwrap() = retries.exhausted();
}

/// Error-row photos whose session retries are spent. Everything not in here
/// with `status = 'error'` is still owed a retry (now or after relaunch).
pub fn exhausted_errors() -> HashSet<String> {
    EXHAUSTED.lock().unwrap().clone()
}

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
/// Post-naming sweep request (set by face_set_name; drained by the worker).
static SWEEP: AtomicBool = AtomicBool::new(false);

/// Ask the worker to run the global auto-assign sweep when it next idles.
pub fn request_sweep() {
    SWEEP.store(true, Ordering::SeqCst);
}

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
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

impl FaceEngine {
    /// Loads the ONNX sessions only. Model *identity* is settled earlier, by
    /// `model_shas` + the DB claim gate — an engine can no longer register
    /// anything on its way in.
    pub fn new(models_dir: &Path, min_det_score: f32) -> Result<Self, String> {
        let t0 = Instant::now();
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

/// Crop + encode a photo's chips **into memory**. Nothing reaches the disk
/// here: a face crop is biometric data, and the only place allowed to write
/// one is `facestore::publish_chips`, which does it under the DB write lock
/// after re-checking that no privacy delete has happened. Shared by the index
/// worker's bake and the repair queue so both reach that same step.
///
/// All-or-nothing: one crop failing to resize or encode fails the set, so a
/// partial bake can never be handed to publication as a complete one (and the
/// publish contract would refuse it anyway).
fn encode_chips(
    preview: &PreviewState,
    photo_id: &str,
    revision: i64,
    img: &RgbImage,
    rects: &[(i64, [f32; 4])],
) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let (pw, ph) = img.dimensions();
    let mut chips = Vec::with_capacity(rects.len());
    for (n, rect) in rects {
        let (x, y, side) = facedet::chip_crop(*rect, pw, ph);
        let crop = image::imageops::crop_imm(img, x, y, side, side).to_image();
        let small = crate::preview::resize_rgb(&crop, CHIP_SIZE, CHIP_SIZE)
            .map_err(|e| format!("chip {n} resize: {e}"))?;
        let bytes = crate::preview::encode_jpeg(&small, CHIP_QUALITY)
            .map_err(|e| format!("chip {n} encode: {e}"))?;
        chips.push((preview.face_chip_path(photo_id, *n, revision), bytes));
    }
    Ok(chips)
}

/// Bake `{id}-f{n}r{rev}.jpg` chips from the already-decoded preview. Chips
/// are person-agnostic crops — assignment changes never invalidate them.
///
/// `revision` is the `chip_revision` the commit that produced these rects
/// wrote: publication is refused unless the photo's scan is still `'ok'` at
/// exactly that revision, so a bake from a superseded detection can never
/// publish crops of rects the DB no longer holds. A zero-face detection
/// publishes an empty set — that publish is what sweeps the previous
/// detection's files, under the same lock and guards as any other. On any
/// failure nothing is published; the previous files stay under their OLD
/// revision names, which no current URL can reach, and repair retries.
fn bake_chips(
    conn: &mut rusqlite::Connection,
    preview: &PreviewState,
    target: &BakeTarget<'_>,
    img: &RgbImage,
    rects: &[[f32; 4]],
) {
    let photo_id = target.photo_id;
    let indexed: Vec<(i64, [f32; 4])> = rects
        .iter()
        .enumerate()
        .map(|(n, r)| (n as i64, *r))
        .collect();
    let chips = match encode_chips(preview, photo_id, target.revision, img, &indexed) {
        Ok(chips) => chips,
        Err(e) => {
            eprintln!("faces: chip encode failed for {photo_id}: {e}");
            return; // nothing reaches the disk; repair retries on demand
        }
    };
    if let Err(e) = facestore::publish_chips(
        conn,
        preview.cache_dir(),
        photo_id,
        target.epoch,
        target.revision,
        &chips,
    ) {
        eprintln!("faces: chip publish failed for {photo_id}: {e}");
    }
}

/// The photo a bake belongs to, and the scan identity it publishes against.
struct BakeTarget<'a> {
    photo_id: &'a str,
    epoch: i64,
    /// `face_scan.chip_revision` as the commit that produced these rects left
    /// it — see `facestore::publish_chips`.
    revision: i64,
}

/// Privacy-delete cleanup, run AFTER the wipe transaction commits.
///
/// Safe by construction, in both directions: chips only ever reach the disk
/// inside `facestore::publish_chips`'s write-lock transaction, which re-reads
/// the committed rows and the epoch. So an in-flight bake or repair — in this
/// process or the other one, and no matter where it was paused — has either
/// already published (and this sweep takes the files) or refuses from the
/// wipe's commit onward and writes nothing at all. There is no staged file to
/// leak, because staging never touched the disk.
///
/// Failure is reported, not swallowed: the caller keeps the DB's
/// `chip_sweep_pending` flag set so the next launch retries.
pub fn purge_chips(preview: &PreviewState) -> Result<usize, String> {
    preview.delete_all_face_chips()
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

/// Repair can legitimately produce nothing: the scan is stale, or the preview
/// it would crop from is gone (janitor-evicted, or the photo is off-disk).
/// Neither is an error, but a chip that will never heal must not fail
/// silently — the route keeps 404ing and the panel keeps re-enqueuing, so say
/// why once per photo per launch.
fn note_unrepairable(photo_id: &str, why: &str) {
    static NOTED: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    let mut noted = NOTED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap();
    if noted.insert(photo_id.to_string()) {
        eprintln!("faces: chip repair for {photo_id} produced nothing — {why}");
    }
}

/// One decode regenerates every missing chip of the photo; returns how many
/// preview decodes it cost, which is the plan's "one decode per photo, no
/// matter how many chips are missing" made checkable.
///
/// Repair validates the artifacts' IDENTITY, not their existence: a file is
/// current only if its name carries the photo's present `chip_revision`.
/// Files under any other name — the previous detection's chips after a crash
/// between its replacement's commit and bake, or the pre-revision legacy
/// shape — are stale, and finding them is itself repair work: the guarded
/// publish sweeps them even when nothing needs re-encoding.
///
/// `pause` runs at the exact point the follow-up review named: after the
/// preview is decoded and every crop is encoded, before publication. In
/// production it does nothing; the privacy tests park a real thread there.
fn repair_photo_inner(
    preview: &PreviewState,
    store: &crate::store::Store,
    photo_id: &str,
    pause: &dyn Fn(),
) -> usize {
    // One consistent read of everything publication is judged against: the
    // epoch, the chip revision that produced the current rects, and the rects.
    let Ok(Some(src)) = store.face_chip_source(photo_id) else {
        note_unrepairable(photo_id, "no servable scan (status not 'ok')");
        return 0;
    };
    let expected: HashSet<String> = src
        .rects
        .iter()
        .map(|(n, _)| crate::preview::face_chip_name(photo_id, *n, src.revision))
        .collect();
    let missing: Vec<(i64, [f32; 4])> = src
        .rects
        .iter()
        .filter(|(n, _)| !preview.face_chip_path(photo_id, *n, src.revision).exists())
        .copied()
        .collect();
    // "Couldn't enumerate" must never read as "nothing stale": an unreadable
    // cache dir (or entry) is treated as stale-work-present, so the guarded
    // publish runs and ITS sweep raises the enumeration failure loudly
    // instead of repair silently deciding there is nothing to do.
    let stale = match std::fs::read_dir(preview.cache_dir()) {
        Ok(entries) => {
            let mut found = false;
            for entry in entries {
                match entry {
                    Ok(e) => {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if crate::preview::is_photo_chip_file(&name, photo_id)
                            && !expected.contains(&name)
                        {
                            found = true;
                            break;
                        }
                    }
                    Err(err) => {
                        eprintln!(
                            "faces: cache dir entry unreadable during repair of {photo_id}: {err}"
                        );
                        found = true;
                        break;
                    }
                }
            }
            found
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            eprintln!("faces: cache dir unreadable during repair of {photo_id}: {e}");
            true
        }
    };
    if missing.is_empty() {
        if stale {
            // Every current artifact exists — only leftovers to take. The
            // empty publish runs the same guards and sweeps under the lock.
            pause();
            if let Err(e) = store.publish_face_chips(
                preview.cache_dir(),
                photo_id,
                src.epoch,
                src.revision,
                &[],
            ) {
                eprintln!("faces: stale chip sweep failed for {photo_id}: {e}");
            }
        }
        return 0;
    }
    let Some(img) = std::fs::read(preview.preview_path_for_id(photo_id))
        .ok()
        .and_then(|b| image::load_from_memory(&b).ok())
        .map(|i| i.to_rgb8())
    else {
        note_unrepairable(photo_id, "no cached preview to crop from");
        return 0;
    };
    let chips = match encode_chips(preview, photo_id, src.revision, &img, &missing) {
        Ok(chips) => chips,
        Err(e) => {
            eprintln!("faces: chip repair encode failed for {photo_id}: {e}");
            return 1;
        }
    };
    pause();
    if let Err(e) = store.publish_face_chips(
        preview.cache_dir(),
        photo_id,
        src.epoch,
        src.revision,
        &chips,
    ) {
        eprintln!("faces: chip repair failed for {photo_id}: {e}");
    }
    1
}

pub(crate) fn repair_photo(preview: &PreviewState, store: &crate::store::Store, photo_id: &str) {
    repair_photo_inner(preview, store, photo_id, &|| {});
}

// ---------- index worker ----------

/// Does this photo need (re)scanning? `row` is its `face_scan` row (None =
/// never scanned), `fresh` its display file's current (mtime, size), and
/// `retryable` whether the session retry policy allows claiming its `error`
/// row right now (`SessionRetries::eligible`): an errored photo is reclaimed
/// on its 5s/30s/180s schedule, parks for the session once three tries are
/// spent, and is owed a whole fresh round by the next session — never simply
/// re-claimed by coming around the loop again.
pub fn needs_scan(
    row: Option<&facestore::ScanRow>,
    gen: i64,
    fresh: Option<(i64, i64)>,
    retryable: bool,
) -> bool {
    let Some(row) = row else {
        return true; // never scanned
    };
    match row.status.as_str() {
        "stale" => true,
        "error" => retryable,
        // Compare against a FRESH stat, not the scan-time entry stat: XMP
        // rewrites (ratings) bump the file and refresh face_scan — matching
        // fresh-vs-row means "no work"; the stale entry stat would re-index
        // that photo forever.
        _ => row.model_gen != gen || fresh.is_some_and(|s| s != (row.mtime, row.size)),
    }
}

/// The worker's whole per-photo eligibility test, in one place so it can be
/// checked directly rather than re-derived: real work to do (`needs_scan`) AND
/// no metadata rewrite of this file queued or running.
///
/// The second half is the round-3 requirement made cross-thread. A rating or
/// tag write changes the JPEG's mtime and size without changing a pixel, and
/// `xmp.rs` refreshes `face_scan`'s stat afterwards. A worker that claimed the
/// photo inside that window would see the new stat, conclude the pixels
/// changed, and re-index for nothing. The queue row spans the whole window
/// (xmp.rs refreshes the stat BEFORE clearing it), so consulting it at the
/// moment of the claim closes the race.
pub fn claim_photo(
    conn: &rusqlite::Connection,
    photo_id: &str,
    gen: i64,
    fresh: Option<(i64, i64)>,
    retryable: bool,
) -> bool {
    let Ok(row) = facestore::scan_row(conn, photo_id) else {
        return false;
    };
    needs_scan(row.as_ref(), gen, fresh, retryable)
        && !facestore::xmp_write_pending(conn, photo_id).unwrap_or(false)
}

/// Hash the bundled model files. Done before the worker claims any job or
/// registers anything, and WITHOUT loading the ONNX sessions: the DB decides
/// from these three values whether this binary owns the library's embedding
/// space, so an older worker has to be identifiable before it can act.
fn model_shas(models_dir: &Path) -> Result<(String, String), String> {
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
    Ok((det_sha, rec_sha))
}

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
                    emit_terminal_error(&app);
                    return;
                }
            };
            let force = force_enabled();
            let mut engine: Option<FaceEngine> = None;
            let mut gen: i64 = 0;
            // Our models' hashes. Computed from the FILES, before any job is
            // claimed and before any generation is registered — an older
            // binary must be recognizable as older while it still has the
            // chance to park (§facestore::claim_state).
            let mut my_shas: Option<(String, String)> = None;
            // Session retry state (three tries at 5s/30s/180s, fresh per
            // session); the DB keeps only the lifetime attempts diagnostic.
            let mut retries = SessionRetries::new();
            // Force-mode pass tracking (gate harness).
            let mut pass_done: HashSet<String> = HashSet::new();
            let mut batch: Vec<String> = Vec::new();
            loop {
                if STOP.load(Ordering::SeqCst) {
                    IDLE.store(true, Ordering::SeqCst);
                    return;
                }
                IDLE.store(false, Ordering::SeqCst);
                // Everything that decides whether this worker may touch the
                // library at all, BEFORE any job is claimed and before any
                // model generation is registered: DB-authoritative enabled
                // state (a requeued job cannot resume under a new epoch, in
                // either process — round 3) and model ownership.
                let candidates = preview.entries_by_distance();
                if my_shas.is_none() {
                    // Deferred until there is real work: hashing 38MB of model
                    // files has no business competing with a cold folder open.
                    let any_preview = candidates
                        .iter()
                        .any(|e| preview.preview_path_for_id(&e.id).exists());
                    if !any_preview || !facestore::index_enabled(&conn).unwrap_or(false) {
                        IDLE.store(true, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    match model_shas(&models_dir) {
                        Ok(shas) => my_shas = Some(shas),
                        Err(e) => {
                            eprintln!("faces: models unreadable: {e}");
                            *ENGINE_ERROR.lock().unwrap() = Some(format!("models unreadable: {e}"));
                            emit_terminal_error(&app);
                            return;
                        }
                    }
                }
                let (det_sha, rec_sha) = my_shas.as_ref().unwrap();
                let mine = (det_sha.as_str(), rec_sha.as_str(), PREP_VERSION);
                let claim =
                    match facestore::claim_state(&conn, mine.0, mine.1, mine.2, MODEL_RELEASE) {
                        Ok(facestore::ClaimState::Unregistered) => {
                            // A model UPGRADE: our release outranks the current
                            // owner (or there is none), so we may claim the
                            // library (bumps the epoch and stale-marks older
                            // scans). `register_gen` re-checks everything under
                            // the writer lock, so a binary whose rank was
                            // overtaken between the two calls still comes back
                            // Park — registration order never beats release order.
                            facestore::register_gen(
                                &mut conn,
                                mine.0,
                                mine.1,
                                mine.2,
                                MODEL_RELEASE,
                            )
                            .unwrap_or(facestore::ClaimState::Park)
                        }
                        Ok(other) => other,
                        Err(e) => {
                            eprintln!("faces: claim check failed: {e}");
                            facestore::ClaimState::Park
                        }
                    };
                match claim {
                    facestore::ClaimState::Ready(g) => {
                        if g != gen {
                            eprintln!("faces: working under model generation {g}");
                            gen = g;
                        }
                    }
                    facestore::ClaimState::Disabled => {
                        if engine.take().is_some() {
                            retries.clear();
                            publish_exhausted(&retries);
                            eprintln!("faces: indexing disabled — worker parked");
                        }
                        flush_progress(&app, &conn, &mut batch);
                        IDLE.store(true, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(1000));
                        continue;
                    }
                    // A generation outranking ours owns the library. Every
                    // commit we made would be discarded against `current_gen`,
                    // and re-registering would start a registration war (each
                    // side stale-marking the other's scans forever).
                    facestore::ClaimState::Park | facestore::ClaimState::Unregistered => {
                        if engine.take().is_some() {
                            retries.clear();
                            publish_exhausted(&retries);
                            eprintln!(
                                "faces: another process owns a different model generation — \
                                 parking this worker"
                            );
                        }
                        flush_progress(&app, &conn, &mut batch);
                        IDLE.store(true, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(1000));
                        continue;
                    }
                }
                let now = Instant::now();
                let job = candidates.into_iter().find(|e| {
                    if !preview.preview_path_for_id(&e.id).exists() {
                        return false;
                    }
                    if force && !pass_done.contains(&e.id) {
                        return true;
                    }
                    claim_photo(
                        &conn,
                        &e.id,
                        gen,
                        display_stat(e),
                        retries.eligible(&e.id, now),
                    )
                });
                let Some(entry) = job else {
                    flush_progress(&app, &conn, &mut batch);
                    if force && !pass_done.is_empty() {
                        PASSES.fetch_add(1, Ordering::Relaxed);
                        pass_done.clear();
                        continue;
                    }
                    // Scanning outranks sweeping; a requested sweep runs when
                    // the queue is otherwise empty.
                    if SWEEP.swap(false, Ordering::SeqCst) && gen > 0 {
                        run_sweep(&app, &mut conn, gen, &cfg);
                        continue;
                    }
                    IDLE.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                };
                if engine.is_none() {
                    // Lazy ONNX init. The generation is already settled above,
                    // so this only loads sessions — it can no longer register
                    // anything, which is what kept an older worker from
                    // claiming the library on its way in.
                    match FaceEngine::new(&models_dir, cfg.min_det_score) {
                        Ok(e) => {
                            eprintln!(
                                "faces: sessions ready in {}ms (gen {gen}), rss={}MB",
                                INIT_MS.load(Ordering::Relaxed),
                                rss_mb()
                            );
                            engine = Some(e);
                        }
                        Err(err) => {
                            // Terminal: one log + a visible non-blocking notice.
                            eprintln!("faces: model init FAILED: {err}");
                            *ENGINE_ERROR.lock().unwrap() = Some(err);
                            emit_terminal_error(&app);
                            return;
                        }
                    }
                }
                let id = entry.id.clone();
                // A claim that is NOT an error retry (never scanned, stale,
                // pixel change, model change) starts the photo's session
                // budget over: an exhausted photo the user explicitly
                // rescanned must really run, with fresh tries.
                let is_error_row = facestore::scan_row(&conn, &id)
                    .ok()
                    .flatten()
                    .is_some_and(|r| r.status == "error");
                if !is_error_row && retries.on_fresh_claim(&id) {
                    publish_exhausted(&retries);
                }
                match process_photo(
                    &mut conn,
                    engine.as_mut().unwrap(),
                    &preview,
                    &entry,
                    gen,
                    &cfg,
                ) {
                    Ok(outcome) => {
                        retries.on_success(&id);
                        publish_exhausted(&retries);
                        if outcome == CommitOutcome::Discarded {
                            DISCARDS.fetch_add(1, Ordering::Relaxed);
                        }
                        batch.push(id.clone());
                    }
                    Err(e) => {
                        ERRORS.fetch_add(1, Ordering::Relaxed);
                        let _ = facestore::record_scan_error(&conn, &id, gen, &e);
                        let terminal = retries.on_failure(&id, Instant::now());
                        publish_exhausted(&retries);
                        if terminal {
                            eprintln!(
                                "faces: {id} failed {} tries this session — parked until \
                                 relaunch: {e}",
                                RETRY_DELAYS.len()
                            );
                        } else {
                            eprintln!("faces: {id} error (will retry): {e}");
                        }
                        // Failures are progress too: without this the last (or
                        // only) photo failing leaves an open People panel on
                        // "Scanning…" forever, its error count never arriving.
                        batch.push(id.clone());
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
    cfg: &FacesCfg,
) -> Result<CommitOutcome, String> {
    let id = &entry.id;
    let (mtime, size) = display_stat(entry).ok_or("display file missing")?;
    // In-process early discard (plan rev 4): the preview this work is about to
    // read can be invalidated and regenerated while we infer. Cheaper than
    // finding out at the DB guards, which remain the actual guarantee.
    let preview_gen = preview.preview_gen(id);
    let snap = facestore::snapshot(conn, id, gen).map_err(|e| e.to_string())?;

    let t0 = Instant::now();
    let img = std::fs::read(preview.preview_path_for_id(id))
        .ok()
        .and_then(|b| image::load_from_memory(&b).ok())
        .map(|i| i.to_rgb8())
        .ok_or("preview unreadable")?;
    DECODE_US.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);

    let faces = engine.detect_embed(&img)?;
    FACES.fetch_add(faces.len() as u64, Ordering::Relaxed);

    // Early discards, before the prototype work: the display file changed
    // under us, or the preview we inferred from was superseded. Neither is the
    // guarantee — `commit_scan` re-verifies the source inside its transaction.
    if display_stat(entry) != Some((mtime, size)) || preview.preview_gen(id) != preview_gen {
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

    // Embed-time matching (Slice B), computed lock-free before the commit:
    // faces that will carry an assignment or the ignored flag are skipped;
    // carried rejections are absolute. Guards inside commit_scan discard the
    // whole result if anything moved underneath us.
    let plan = facestore::plan_carry_over(&snap, &new);
    let protos = facestore::person_prototypes(conn, gen, 5).map_err(|e| e.to_string())?;
    let empty = std::collections::HashSet::new();
    let raw: Vec<(usize, i64, f32)> = new
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            if protos.is_empty() {
                return None;
            }
            let carried = plan.get(&i).map(|&oi| &snap.old[oi]);
            // Carried assignments and ignored flags are not ours to overwrite.
            if carried.is_some_and(|c| c.person_id.is_some() || c.ignored) {
                return None;
            }
            // Transferred "not X" excludes X here too.
            let rejected: std::collections::HashSet<i64> = carried
                .map(|c| c.rejections.iter().copied().collect())
                .unwrap_or_else(|| empty.clone());
            facedet::match_face(
                &f.embedding,
                &protos,
                &rejected,
                cfg.auto_assign_threshold,
                cfg.auto_assign_margin,
            )
            .map(|(pid, score)| (i, pid, score))
        })
        .collect();
    // One person cannot be two faces in one photo: carried assignments hold
    // their person, and among the remaining proposals the strongest wins.
    let present: std::collections::HashSet<i64> = plan
        .values()
        .filter_map(|&oi| snap.old[oi].person_id)
        .collect();
    let mut proposals: Vec<Option<(i64, f32)>> = vec![None; new.len()];
    for (i, pid, score) in facedet::one_face_per_person(raw, &present) {
        proposals[i] = Some((pid, score));
    }

    let outcome = facestore::commit_scan(
        conn,
        id,
        &snap,
        &new,
        &proposals,
        mtime,
        size,
        // Re-verified inside the write transaction: the pre-inference check
        // above is only an early discard.
        &|| display_stat(entry) == Some((mtime, size)),
    )
    .map_err(|e| e.to_string())?;
    if let CommitOutcome::Committed(revision) = outcome {
        let rects: Vec<[f32; 4]> = faces.iter().map(|f| f.rect).collect();
        bake_chips(
            conn,
            preview,
            &BakeTarget {
                photo_id: id,
                epoch: snap.epoch,
                revision,
            },
            &img,
            &rects,
        );
    }
    Ok(outcome)
}

/// Post-naming global sweep: match every remaining unassigned face against
/// the (user-confirmed) prototypes, short per-photo conditional commits,
/// then one progress event so the panel refreshes.
fn run_sweep(app: &tauri::AppHandle, conn: &mut rusqlite::Connection, gen: i64, cfg: &FacesCfg) {
    let protos = match facestore::person_prototypes(conn, gen, 5) {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };
    let candidates = facestore::sweep_candidates(conn, gen).unwrap_or_default();
    let mut assigned = 0usize;
    let mut touched: Vec<String> = Vec::new();
    for photo_id in candidates {
        if STOP.load(Ordering::SeqCst) || !facestore::index_enabled(conn).unwrap_or(false) {
            break;
        }
        match facestore::sweep_photo(
            conn,
            &photo_id,
            gen,
            &protos,
            cfg.auto_assign_threshold,
            cfg.auto_assign_margin,
        ) {
            Ok(n) if n > 0 => {
                assigned += n;
                touched.push(photo_id);
            }
            _ => {}
        }
    }
    if assigned > 0 {
        eprintln!("faces: sweep auto-assigned {assigned} faces");
        let mut batch = touched;
        flush_progress(app, conn, &mut batch);
    }
}

/// Terminal failure (model init, model registration, DB open): the People
/// panel may be sitting on a stale "Scanning…" with no further events ever
/// coming. An empty progress event makes it refetch status, where
/// `engineError` is waiting. `folderId: null` = "concerns whatever folder you
/// have open" — both listeners accept it.
fn emit_terminal_error(app: &tauri::AppHandle) {
    let _ = app.emit(
        "faces-progress",
        serde_json::json!({
            "folderId": serde_json::Value::Null,
            "scanned": 0,
            "total": 0,
            "photoIds": Vec::<String>::new(),
        }),
    );
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

    /// Store + a separate worker connection on one DB (the two-connection,
    /// and in spirit two-process, reality) plus a cache dir, with one photo's
    /// faces already committed — the "crash before bake" state.
    fn chip_setup() -> (
        PathBuf,
        std::sync::Arc<crate::store::Store>,
        rusqlite::Connection,
        std::sync::Arc<PreviewState>,
        i64,
    ) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "emberchip-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        let db = dir.join("t.sqlite3");
        let store = crate::store::Store::new(&db).unwrap();
        let folder = store.open_folder("/tmp/chip-x").unwrap();
        store
            .sync_photos(
                folder.folder_id,
                &[std::sync::Arc::new(crate::scanner::PhotoEntry {
                    id: PID.into(),
                    dir: "/tmp/chip-x".into(),
                    stem: "P1".into(),
                    jpeg: Some("/tmp/chip-x/P1.JPG".into()),
                    raf: None,
                    mtime: 1,
                    size: 1,
                })],
            )
            .unwrap();
        let mut worker = rusqlite::Connection::open(&db).unwrap();
        worker.pragma_update(None, "foreign_keys", "ON").unwrap();
        worker.pragma_update(None, "busy_timeout", 5000).unwrap();
        let gen = facestore::ensure_gen(&mut worker, "d", "r", 1).unwrap();
        let snap = facestore::snapshot(&worker, PID, gen).unwrap();
        let e = vec![0.1f32; facedet::EMBED_DIM];
        let out = facestore::commit_scan(
            &mut worker,
            PID,
            &snap,
            &[
                NewFace {
                    rect: CHIP_RECTS[0],
                    det_score: 0.9,
                    embedding: e.clone(),
                },
                NewFace {
                    rect: CHIP_RECTS[1],
                    det_score: 0.85,
                    embedding: e,
                },
            ],
            &[],
            1,
            1,
            &|| true,
        )
        .unwrap();
        let CommitOutcome::Committed(revision) = out else {
            panic!("setup scan must commit");
        };
        let preview = std::sync::Arc::new(PreviewState::new(
            dir.join("cache"),
            crate::exposure::BlinkiesCfg::default(),
        ));
        (dir, std::sync::Arc::new(store), worker, preview, revision)
    }

    /// The privacy-delete mirror closure, when settings.toml isn't the subject.
    fn no_mirror(_: bool) -> std::io::Result<()> {
        Ok(())
    }

    /// Ids are 16 hex chars in production, and the chip sweep matches on that
    /// shape — a test id must look like one.
    const PID: &str = "00deadbeef00cafe";
    const CHIP_RECTS: [[f32; 4]; 2] = [[0.1, 0.1, 0.2, 0.3], [0.6, 0.5, 0.2, 0.3]];

    fn flat_preview(preview: &PreviewState) -> RgbImage {
        let img = RgbImage::from_pixel(400, 300, image::Rgb([90, 120, 150]));
        crate::preview::write_jpeg(&img, &preview.preview_path_for_id(PID), 85).unwrap();
        img
    }

    /// Nothing chip-shaped (published or staged) is left in the cache dir.
    fn cache_is_chip_free(dir: &Path) -> bool {
        std::fs::read_dir(dir.join("cache"))
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().contains("-f"))
    }

    /// A two-thread rendezvous: `arrive()` blocks the worker at the dangerous
    /// point until the test has done its part, and `wait()` blocks the test
    /// until the worker is really parked there. No sleeps, no timing luck.
    struct Barrier {
        reached: crossbeam_channel::Sender<()>,
        go: crossbeam_channel::Receiver<()>,
    }
    struct Control {
        reached: crossbeam_channel::Receiver<()>,
        go: crossbeam_channel::Sender<()>,
    }
    fn rendezvous() -> (Barrier, Control) {
        let (rt, rr) = crossbeam_channel::bounded(1);
        let (gt, gr) = crossbeam_channel::bounded(1);
        (
            Barrier {
                reached: rt,
                go: gr,
            },
            Control {
                reached: rr,
                go: gt,
            },
        )
    }
    impl Barrier {
        fn arrive(&self) {
            self.reached.send(()).unwrap();
            let _ = self.go.recv();
        }
    }
    impl Control {
        fn wait(&self) {
            self.reached
                .recv_timeout(Duration::from_secs(10))
                .expect("the writer must reach the barrier");
        }
        fn release(&self) {
            let _ = self.go.send(());
        }
    }

    /// Cache-dir delete (documented safe) and crash-between-commit-and-bake
    /// both leave committed face rows without chips — repair must rebake from
    /// stored rects alone: no ONNX, and ONE preview decode however many chips
    /// are missing (the plan's dedup requirement, now counted rather than
    /// asserted in prose).
    #[test]
    fn chip_repair_rebakes_from_rects_with_one_decode_and_no_inference() {
        let (dir, store, _worker, preview, rev) = chip_setup();
        flat_preview(&preview);

        let decodes = repair_photo_inner(&preview, &store, PID, &|| {});
        assert_eq!(decodes, 1, "two missing chips, one preview decode");
        for n in 0..2 {
            let chip = preview.face_chip_path(PID, n, rev);
            assert!(chip.exists(), "chip {n} rebaked");
            let decoded = image::open(&chip).unwrap().to_rgb8();
            assert_eq!(decoded.dimensions(), (CHIP_SIZE, CHIP_SIZE));
        }
        // Nothing missing → no decode at all.
        assert_eq!(repair_photo_inner(&preview, &store, PID, &|| {}), 0);
        // Wipe one chip (partial cache delete) → one decode, only it rebaked.
        std::fs::remove_file(preview.face_chip_path(PID, 1, rev)).unwrap();
        assert_eq!(repair_photo_inner(&preview, &store, PID, &|| {}), 1);
        assert!(preview.face_chip_path(PID, 1, rev).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Repair validates artifact IDENTITY, not existence: files from an older
    /// detection (or the pre-revision filename era) count as damage even when
    /// every current artifact is present, and get swept without a decode.
    #[test]
    fn chip_repair_removes_stale_artifacts_even_with_nothing_missing() {
        let (dir, store, _worker, preview, rev) = chip_setup();
        flat_preview(&preview);
        assert_eq!(repair_photo_inner(&preview, &store, PID, &|| {}), 1);

        // A survivor from a previous scan, and one in the legacy shape.
        let old_rev = dir.join("cache").join(format!("{PID}-f0r{}.jpg", rev - 1));
        let legacy = dir.join("cache").join(format!("{PID}-f0.jpg"));
        std::fs::write(&old_rev, b"previous detection").unwrap();
        std::fs::write(&legacy, b"pre-upgrade chip").unwrap();

        assert_eq!(
            repair_photo_inner(&preview, &store, PID, &|| {}),
            0,
            "identity cleanup costs no decode"
        );
        assert!(!old_rev.exists(), "the older revision's crop is gone");
        assert!(!legacy.exists(), "so is the legacy-name crop");
        for n in 0..2 {
            assert!(preview.face_chip_path(PID, n, rev).exists(), "current kept");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// **The privacy blocker, as the follow-up review described it.** A repair
    /// is parked on its own thread at the exact moment after it has decoded
    /// the preview and encoded the crops, before publication. The delete then
    /// runs to completion — wipe transaction AND chip sweep — and returns. The
    /// parked writer is only then released.
    ///
    /// Two things are asserted that the previous sequential test could not:
    /// while the writer is parked mid-operation, the cache contains no chip
    /// and no temp (staging never touches the disk); and when it resumes after
    /// the sweep, it still writes nothing.
    #[test]
    fn a_repair_parked_before_publication_cannot_outrun_a_delete() {
        let (dir, store, _worker, preview, _rev) = chip_setup();
        flat_preview(&preview);
        let (barrier, control) = rendezvous();

        let writer = {
            let (store, preview) = (store.clone(), preview.clone());
            std::thread::spawn(move || {
                repair_photo_inner(&preview, &store, PID, &|| barrier.arrive())
            })
        };
        control.wait(); // the repair is now holding encoded face crops in RAM
        assert!(
            cache_is_chip_free(&dir),
            "encoded chips must not exist on disk before publication"
        );

        // …and now the whole privacy delete, exactly as the command runs it.
        store.delete_face_data(&no_mirror).unwrap();
        assert_eq!(purge_chips(&preview).unwrap(), 0, "nothing to sweep");
        assert!(cache_is_chip_free(&dir));

        control.release();
        writer.join().unwrap();
        assert!(
            cache_is_chip_free(&dir),
            "a writer released after the sweep must publish nothing"
        );
        assert!(store.face_chip_source(PID).unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The same seam for the index worker's bake, on its own thread and the
    /// OTHER connection (the second Ember process): the worker is really
    /// parked mid-operation — crops encoded in RAM, publication not yet
    /// attempted — while the delete runs to completion, and only then
    /// released. Sequentially "simulating" this ordering is exactly what the
    /// round-3 review called out; the rendezvous makes the interleaving real.
    #[test]
    fn a_bake_parked_before_publication_cannot_outrun_a_delete() {
        let (dir, store, worker, preview, revision) = chip_setup();
        let img = flat_preview(&preview);
        let epoch = facestore::state_get(&worker, "index_epoch").unwrap();
        let (barrier, control) = rendezvous();

        let baker = {
            let preview = preview.clone();
            let mut worker = worker; // the second process's own connection
            std::thread::spawn(move || {
                let indexed: Vec<(i64, [f32; 4])> = CHIP_RECTS
                    .iter()
                    .enumerate()
                    .map(|(n, r)| (n as i64, *r))
                    .collect();
                let chips = encode_chips(&preview, PID, revision, &img, &indexed).unwrap();
                assert_eq!(chips.len(), 2, "encoded, and only in RAM");
                barrier.arrive(); // parked here while the delete runs
                facestore::publish_chips(
                    &mut worker,
                    preview.cache_dir(),
                    PID,
                    epoch,
                    revision,
                    &chips,
                )
                .unwrap()
            })
        };
        control.wait(); // the bake now holds encoded face crops in RAM
        assert!(
            cache_is_chip_free(&dir),
            "nothing on disk while the bake is parked before publication"
        );

        // The whole privacy delete, exactly as the command runs it.
        store.delete_face_data(&no_mirror).unwrap();
        assert_eq!(purge_chips(&preview).unwrap(), 0, "nothing to sweep");
        assert!(cache_is_chip_free(&dir));

        control.release();
        assert_eq!(
            baker.join().unwrap(),
            facestore::PublishOutcome::Refused,
            "a bake released after the wipe must be refused"
        );
        assert!(cache_is_chip_free(&dir), "not one chip, not one temp");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Process death between encoding and publication: there is nothing on
    /// disk to survive it, which is the whole point of keeping the bytes in
    /// memory. The writer thread panics at the barrier — as violent an exit as
    /// a thread can stage — and the cache is untouched before and after.
    #[test]
    fn a_writer_that_dies_before_publication_leaves_no_biometric_file() {
        let (dir, store, _worker, preview, _rev) = chip_setup();
        flat_preview(&preview);
        let (barrier, control) = rendezvous();

        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // the panic below is deliberate
        let writer = {
            let (store, preview) = (store.clone(), preview.clone());
            std::thread::spawn(move || {
                repair_photo_inner(&preview, &store, PID, &|| {
                    barrier.arrive();
                    panic!("writer death between encoding and publication");
                })
            })
        };
        control.wait();
        assert!(cache_is_chip_free(&dir), "nothing staged on disk");
        control.release();
        assert!(writer.join().is_err(), "the writer really did die");
        std::panic::set_hook(hook);

        assert!(
            cache_is_chip_free(&dir),
            "a dead writer leaves no chip and no temp behind"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// …and the ordinary path still works, with the delete's sweep removing
    /// chips that WERE published before it — including a temp left by a
    /// process killed inside `publish_bytes`.
    #[test]
    fn chips_bake_normally_and_the_delete_sweeps_them() {
        let (dir, store, mut worker, preview, revision) = chip_setup();
        let img = flat_preview(&preview);
        let epoch = facestore::state_get(&worker, "index_epoch").unwrap();

        bake_chips(
            &mut worker,
            &preview,
            &BakeTarget {
                photo_id: PID,
                epoch,
                revision,
            },
            &img,
            &CHIP_RECTS,
        );
        for n in 0..2 {
            assert!(
                preview.face_chip_path(PID, n, revision).exists(),
                "chip {n} baked"
            );
        }
        std::fs::write(dir.join("cache").join(format!("{PID}-f9r2.tmp999-1")), b"x").unwrap();

        store.delete_face_data(&no_mirror).unwrap();
        assert_eq!(purge_chips(&preview).unwrap(), 3);
        assert!(cache_is_chip_free(&dir), "the sweep takes chips AND temps");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A sweep that cannot remove every biometric file must not let the delete
    /// report success, and the DB must keep the cleanup owed until one does.
    #[test]
    fn a_failed_chip_sweep_keeps_the_cleanup_owed() {
        let (dir, store, mut worker, preview, revision) = chip_setup();
        let img = flat_preview(&preview);
        let epoch = facestore::state_get(&worker, "index_epoch").unwrap();
        bake_chips(
            &mut worker,
            &preview,
            &BakeTarget {
                photo_id: PID,
                epoch,
                revision,
            },
            &img,
            &CHIP_RECTS,
        );
        // A directory where a chip file should be: `remove_file` cannot take
        // it, on any platform and as any user.
        std::fs::create_dir(dir.join("cache").join(format!("{PID}-f7.jpg"))).unwrap();

        store.delete_face_data(&no_mirror).unwrap();
        assert!(
            store.chip_sweep_pending().unwrap(),
            "the wipe records that chips are still owed"
        );
        let err = purge_chips(&preview).expect_err("the sweep must report failure");
        assert!(err.contains("-f7.jpg"), "{err}");
        assert!(
            store.chip_sweep_pending().unwrap(),
            "a failed sweep does not clear the debt"
        );

        // The next launch retries it; only a clean sweep clears the flag.
        std::fs::remove_dir(dir.join("cache").join(format!("{PID}-f7.jpg"))).unwrap();
        purge_chips(&preview).unwrap();
        store.clear_chip_sweep_pending().unwrap();
        assert!(!store.chip_sweep_pending().unwrap());
        assert!(cache_is_chip_free(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Publication is tied to the exact scan that produced the crops, not just
    /// to "some face row exists". Pixel invalidation marks a photo stale
    /// without moving the epoch, and a re-detection replaces its rows inside
    /// the same epoch — an older bake must refuse in both cases rather than
    /// publish crops of rects the DB no longer holds.
    #[test]
    fn a_bake_from_a_superseded_scan_refuses_to_publish() {
        let (dir, store, mut worker, preview, revision) = chip_setup();
        let img = flat_preview(&preview);
        let epoch = facestore::state_get(&worker, "index_epoch").unwrap();
        let indexed: Vec<(i64, [f32; 4])> = CHIP_RECTS
            .iter()
            .enumerate()
            .map(|(n, r)| (n as i64, *r))
            .collect();
        let chips = encode_chips(&preview, PID, revision, &img, &indexed).unwrap();

        // 1. Pixel invalidation: same epoch, but the scan is no longer 'ok'.
        store.mark_face_stale(PID).unwrap();
        assert_eq!(
            facestore::publish_chips(
                &mut worker,
                preview.cache_dir(),
                PID,
                epoch,
                revision,
                &chips
            )
            .unwrap(),
            facestore::PublishOutcome::Refused,
        );
        assert!(cache_is_chip_free(&dir));

        // 2. The photo is re-detected (different rects). The older bake still
        //    matches epoch and status, and is still wrong.
        let gen = facestore::current_gen(&worker).unwrap().unwrap().0;
        let snap = facestore::snapshot(&worker, PID, gen).unwrap();
        let moved = [[0.3f32, 0.3, 0.2, 0.3], [0.7, 0.6, 0.2, 0.3]];
        let e = vec![0.1f32; facedet::EMBED_DIM];
        let out = facestore::commit_scan(
            &mut worker,
            PID,
            &snap,
            &[
                NewFace {
                    rect: moved[0],
                    det_score: 0.9,
                    embedding: e.clone(),
                },
                NewFace {
                    rect: moved[1],
                    det_score: 0.9,
                    embedding: e,
                },
            ],
            &[],
            1,
            1,
            &|| true,
        )
        .unwrap();
        let CommitOutcome::Committed(fresh_revision) = out else {
            panic!("the rescan must commit");
        };
        assert_ne!(
            fresh_revision, revision,
            "a re-detection moves the identity"
        );
        assert_eq!(
            facestore::publish_chips(
                &mut worker,
                preview.cache_dir(),
                PID,
                epoch,
                revision,
                &chips
            )
            .unwrap(),
            facestore::PublishOutcome::Refused,
            "chips of the previous detection are not this photo's chips"
        );
        assert!(cache_is_chip_free(&dir));

        // The bake belonging to the new scan publishes normally.
        let fresh = encode_chips(
            &preview,
            PID,
            fresh_revision,
            &img,
            &[(0, moved[0]), (1, moved[1])],
        )
        .unwrap();
        assert_eq!(
            facestore::publish_chips(
                &mut worker,
                preview.cache_dir(),
                PID,
                epoch,
                fresh_revision,
                &fresh
            )
            .unwrap(),
            facestore::PublishOutcome::Published,
        );
        assert!(preview.face_chip_path(PID, 0, fresh_revision).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Publication failure part-way through multiple outputs: the second
    /// destination is blocked (a directory sits where the file must land, so
    /// the rename fails after the first chip already published). The failure
    /// is reported, the landed chip is rolled back, and no temp survives —
    /// there is no path an observer could mistake for a valid artifact of
    /// this revision.
    #[test]
    fn a_publish_that_fails_midway_rolls_back_and_cleans_its_temps() {
        let (dir, _store, mut worker, preview, revision) = chip_setup();
        let img = flat_preview(&preview);
        let epoch = facestore::state_get(&worker, "index_epoch").unwrap();
        let indexed: Vec<(i64, [f32; 4])> = CHIP_RECTS
            .iter()
            .enumerate()
            .map(|(n, r)| (n as i64, *r))
            .collect();
        let chips = encode_chips(&preview, PID, revision, &img, &indexed).unwrap();

        let blocked = preview.face_chip_path(PID, 1, revision);
        std::fs::create_dir(&blocked).unwrap();
        let err = facestore::publish_chips(
            &mut worker,
            preview.cache_dir(),
            PID,
            epoch,
            revision,
            &chips,
        )
        .expect_err("a blocked rename must be reported, not swallowed");
        assert!(err.contains("-f1r"), "{err}");
        assert!(
            !preview.face_chip_path(PID, 0, revision).exists(),
            "the chip that DID land is rolled back"
        );
        std::fs::remove_dir(&blocked).unwrap();
        assert!(cache_is_chip_free(&dir), "and no temp survives any failure");

        // Temp-write failure (unwritable cache dir): reported, nothing left.
        use std::os::unix::fs::PermissionsExt;
        let cache = dir.join("cache");
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = facestore::publish_chips(
            &mut worker,
            preview.cache_dir(),
            PID,
            epoch,
            revision,
            &chips,
        )
        .expect_err("an unwritable cache dir must fail the publish");
        assert!(err.contains("chip write failed"), "{err}");
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(cache_is_chip_free(&dir));
        std::fs::remove_dir_all(&dir).unwrap();

        // On resize/encode failure: no input constructible from a decodable
        // preview can make them fail (chip_crop clamps every crop inside the
        // image, and the resize/encode targets are fixed small dimensions —
        // even a synthetic 0×0 image resizes without error). encode_chips
        // still propagates any such error with `?`, so a partial set cannot
        // leave it — and `an_incomplete_chip_set_cannot_publish` proves a
        // partial set could not publish even if one did.
    }

    /// "Couldn't look" must never read as "nothing stale": a cache dir the
    /// stale sweep cannot enumerate fails the publish loudly, before a single
    /// byte lands.
    #[test]
    fn an_unenumerable_cache_dir_fails_the_publish() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _store, mut worker, preview, revision) = chip_setup();
        let img = flat_preview(&preview);
        let epoch = facestore::state_get(&worker, "index_epoch").unwrap();
        let indexed: Vec<(i64, [f32; 4])> = CHIP_RECTS
            .iter()
            .enumerate()
            .map(|(n, r)| (n as i64, *r))
            .collect();
        let chips = encode_chips(&preview, PID, revision, &img, &indexed).unwrap();
        let cache = dir.join("cache");
        // Executable but not readable: files remain statable/writable, the
        // directory cannot be listed.
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o311)).unwrap();
        let err = facestore::publish_chips(
            &mut worker,
            preview.cache_dir(),
            PID,
            epoch,
            revision,
            &chips,
        )
        .expect_err("an unenumerable cache dir must fail the publish");
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.contains("unreadable"), "{err}");
        assert!(cache_is_chip_free(&dir), "nothing was written");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Publication is all-expected-artifacts: a subset that neither supplies
    /// nor finds every artifact of the revision on disk is an incomplete bake
    /// and must be an error — never a "published" photo with holes that an
    /// older file could appear to fill.
    #[test]
    fn an_incomplete_chip_set_cannot_publish() {
        let (dir, _store, mut worker, preview, revision) = chip_setup();
        let img = flat_preview(&preview);
        let epoch = facestore::state_get(&worker, "index_epoch").unwrap();
        let only_first =
            encode_chips(&preview, PID, revision, &img, &[(0, CHIP_RECTS[0])]).unwrap();
        let err = facestore::publish_chips(
            &mut worker,
            preview.cache_dir(),
            PID,
            epoch,
            revision,
            &only_first,
        )
        .expect_err("one of two artifacts is an incomplete bake");
        assert!(err.contains("incomplete"), "{err}");
        assert!(cache_is_chip_free(&dir));

        // The same subset IS a valid repair once the other artifact exists on
        // disk under this revision's name (a name only a publish of this very
        // revision can have created).
        let full = encode_chips(
            &preview,
            PID,
            revision,
            &img,
            &CHIP_RECTS
                .iter()
                .enumerate()
                .map(|(n, r)| (n as i64, *r))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        facestore::publish_chips(
            &mut worker,
            preview.cache_dir(),
            PID,
            epoch,
            revision,
            &full,
        )
        .unwrap();
        std::fs::remove_file(preview.face_chip_path(PID, 0, revision)).unwrap();
        assert_eq!(
            facestore::publish_chips(
                &mut worker,
                preview.cache_dir(),
                PID,
                epoch,
                revision,
                &only_first
            )
            .unwrap(),
            facestore::PublishOutcome::Published,
            "repairing just the missing artifact is complete"
        );
        assert!(preview.face_chip_path(PID, 0, revision).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn claim_setup() -> (PathBuf, PathBuf) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "emberclaim-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.sqlite3");
        (dir, db)
    }

    fn open_conn(db: &Path) -> rusqlite::Connection {
        let c = rusqlite::Connection::open(db).unwrap();
        c.pragma_update(None, "busy_timeout", 5000).unwrap();
        c
    }

    /// Release triples for the ownership tests: (det, rec, prep, release).
    const OLD_M: (&str, &str, i64, i64) = ("det-v1", "rec-v1", PREP_VERSION, 1);
    const NEW_M: (&str, &str, i64, i64) = ("det-v2", "rec-v1", PREP_VERSION, 2);

    fn claim(c: &rusqlite::Connection, m: (&str, &str, i64, i64)) -> facestore::ClaimState {
        facestore::claim_state(c, m.0, m.1, m.2, m.3).unwrap()
    }

    fn register(c: &mut rusqlite::Connection, m: (&str, &str, i64, i64)) -> facestore::ClaimState {
        facestore::register_gen(c, m.0, m.1, m.2, m.3).unwrap()
    }

    /// **Blocker 2, in the order the round-3 review said the old gate could
    /// not distinguish**: the NEW model initializes a fresh library first, and
    /// an old model this DB has *never seen* arrives second. Absence from
    /// history proves nothing — the release rank must park it. Equal rank
    /// with different hashes must park too, not win by arrival.
    #[test]
    fn an_unseen_older_model_can_never_take_a_library_from_a_newer_one() {
        let (dir, db) = claim_setup();
        let _store = crate::store::Store::new(&db).unwrap();
        let mut old_worker = open_conn(&db);
        let mut new_worker = open_conn(&db);

        assert_eq!(
            claim(&new_worker, NEW_M),
            facestore::ClaimState::Unregistered
        );
        let g_new = match register(&mut new_worker, NEW_M) {
            facestore::ClaimState::Ready(g) => g,
            other => panic!("{other:?}"),
        };

        // The v1 binary starts against this DB for the first time. Its hashes
        // are absent from face_model_gens — under the old hash-history gate
        // that read as "upgrade" and it would have registered generation 2,
        // stale-marking every real scan and parking the newer worker.
        assert_eq!(claim(&old_worker, OLD_M), facestore::ClaimState::Park);
        assert_eq!(
            register(&mut old_worker, OLD_M),
            facestore::ClaimState::Park,
            "even called directly, the unseen old model cannot register"
        );
        assert_eq!(
            facestore::current_gen(&old_worker).unwrap().unwrap().0,
            g_new
        );

        // Two different builds mislabeled with the SAME rank: neither may
        // displace the sitting owner, whichever arrives second.
        let sibling = ("det-v2-variant", "rec-v1", PREP_VERSION, NEW_M.3);
        assert_eq!(claim(&new_worker, sibling), facestore::ClaimState::Park);
        assert_eq!(
            register(&mut new_worker, sibling),
            facestore::ClaimState::Park
        );
        assert_eq!(
            facestore::current_gen(&new_worker).unwrap().unwrap().0,
            g_new
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The worker's whole job-claim gate, driven through TWO connections on
    /// one DB — the two-process reality, not a pure decision function. The
    /// other registration order: old first, then the genuine upgrade; plus
    /// the disabled-library guard in every direction.
    #[test]
    fn the_claim_gate_parks_an_older_worker_and_refuses_a_disabled_library() {
        let (dir, db) = claim_setup();
        let store = crate::store::Store::new(&db).unwrap();
        // Two independent connections: "this process" and "the other one".
        let mut old_worker = open_conn(&db);
        let mut new_worker = open_conn(&db);

        // Fresh library: the old binary arrives first and registers.
        assert_eq!(
            claim(&old_worker, OLD_M),
            facestore::ClaimState::Unregistered
        );
        let g1 = match register(&mut old_worker, OLD_M) {
            facestore::ClaimState::Ready(g) => g,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            claim(&old_worker, OLD_M),
            facestore::ClaimState::Ready(g1),
            "our own generation: keep working"
        );

        // A newer release upgrades the library.
        assert_eq!(
            claim(&new_worker, NEW_M),
            facestore::ClaimState::Unregistered
        );
        let g2 = match register(&mut new_worker, NEW_M) {
            facestore::ClaimState::Ready(g) => g,
            other => panic!("{other:?}"),
        };
        assert!(g2 > g1);

        // The older worker — whether it has been running all along or is only
        // starting now, since neither has more than hashes and a rank — parks,
        // and may not re-register its models on top of the newer generation.
        assert_eq!(claim(&old_worker, OLD_M), facestore::ClaimState::Park);
        assert_eq!(
            register(&mut old_worker, OLD_M),
            facestore::ClaimState::Park,
            "even called directly, the older binary cannot take the library back"
        );
        assert_eq!(
            facestore::current_gen(&old_worker).unwrap().unwrap().0,
            g2,
            "ownership did not move"
        );

        // Disabled: no worker may claim anything, whatever its models.
        store.set_faces_enabled(false, &no_mirror).unwrap();
        for (c, m) in [(&new_worker, NEW_M), (&old_worker, OLD_M)] {
            assert_eq!(claim(c, m), facestore::ClaimState::Disabled);
        }
        assert_eq!(
            facestore::register_gen(&mut new_worker, "det-v3", "rec-v1", PREP_VERSION, 3).unwrap(),
            facestore::ClaimState::Disabled,
            "a disabled library cannot even be re-registered into"
        );
        assert_eq!(facestore::current_gen(&new_worker).unwrap().unwrap().0, g2);

        // Re-enabled: the newest models resume where they were.
        store.set_faces_enabled(true, &no_mirror).unwrap();
        assert_eq!(claim(&new_worker, NEW_M), facestore::ClaimState::Ready(g2));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Registration RACING, both orders, sequenced by a rendezvous rather than
    /// scheduler luck. The dangerous interleaving: the old binary reads its
    /// claim decision ("library is empty — I may register") and is parked;
    /// the new binary registers in the gap; the old register must then come
    /// back Park from its own in-lock re-check — its stale decision may not
    /// win by landing last.
    #[test]
    fn registration_order_cannot_beat_release_order() {
        // Interleaved order: old decides first, lands last.
        let (dir, db) = claim_setup();
        let _store = crate::store::Store::new(&db).unwrap();
        let (barrier, control) = rendezvous();
        let old_thread = {
            let db = db.clone();
            std::thread::spawn(move || {
                let mut conn = open_conn(&db);
                let decided = claim(&conn, OLD_M);
                barrier.arrive(); // parked between decision and registration
                (decided, register(&mut conn, OLD_M))
            })
        };
        control.wait();
        let mut new_conn = open_conn(&db);
        let g_new = match register(&mut new_conn, NEW_M) {
            facestore::ClaimState::Ready(g) => g,
            other => panic!("{other:?}"),
        };
        control.release();
        let (old_decided, old_registered) = old_thread.join().unwrap();
        assert_eq!(
            old_decided,
            facestore::ClaimState::Unregistered,
            "the old binary really did decide it could register"
        );
        assert_eq!(
            old_registered,
            facestore::ClaimState::Park,
            "…and the writer-lock re-check still parked it"
        );
        let owner = facestore::current_gen(&new_conn).unwrap().unwrap();
        assert_eq!((owner.0, owner.4), (g_new, NEW_M.3));
        std::fs::remove_dir_all(&dir).unwrap();

        // The reverse order on a fresh library: old lands first, the new
        // binary arrives second and upgrades over it.
        let (dir, db) = claim_setup();
        let _store = crate::store::Store::new(&db).unwrap();
        let mut old_conn = open_conn(&db);
        assert!(matches!(
            register(&mut old_conn, OLD_M),
            facestore::ClaimState::Ready(_)
        ));
        let new_thread = {
            let db = db.clone();
            std::thread::spawn(move || {
                let mut conn = open_conn(&db);
                register(&mut conn, NEW_M)
            })
        };
        assert!(matches!(
            new_thread.join().unwrap(),
            facestore::ClaimState::Ready(_)
        ));
        let owner = facestore::current_gen(&old_conn).unwrap().unwrap();
        assert_eq!(owner.4, NEW_M.3, "the newer release owns the library");
        assert_eq!(claim(&old_conn, OLD_M), facestore::ClaimState::Park);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The plan's retry contract — three tries per session at 5s/30s/180s,
    /// truthful status at every stage, terminal only when spent, and a whole
    /// fresh round after relaunch — driven through the worker's REAL decision
    /// path: `claim_photo` (the claim gate), `SessionRetries` (the session
    /// policy object the loop consults), `record_scan_error`/`commit_scan`
    /// (persistence) and `face_scan_status` (what the panel sees), in exactly
    /// the order the worker loop calls them, under a controlled clock.
    /// (Spinning the loop thread itself needs a Tauri AppHandle; every retry
    /// decision it makes routes through what is exercised here.)
    #[test]
    fn retries_run_three_per_session_then_terminal_and_reset_on_relaunch() {
        let (dir, db) = claim_setup();
        let store = crate::store::Store::new(&db).unwrap();
        let folder = store.open_folder("/tmp/retry-x").unwrap();
        store
            .sync_photos(
                folder.folder_id,
                &[std::sync::Arc::new(crate::scanner::PhotoEntry {
                    id: "p1".into(),
                    dir: "/tmp/retry-x".into(),
                    stem: "P1".into(),
                    jpeg: Some("/tmp/retry-x/P1.JPG".into()),
                    raf: None,
                    mtime: 1,
                    size: 1,
                })],
            )
            .unwrap();
        let mut worker = open_conn(&db);
        worker.pragma_update(None, "foreign_keys", "ON").unwrap();
        let gen = facestore::ensure_gen(&mut worker, "d", "r", 1).unwrap();
        let sec = Duration::from_secs;
        let claimable = |worker: &rusqlite::Connection, retries: &SessionRetries, at: Instant| {
            claim_photo(worker, "p1", gen, Some((1, 1)), retries.eligible("p1", at))
        };
        let status = |retries: &SessionRetries| {
            let s = store
                .face_scan_status(folder.folder_id, &retries.exhausted())
                .unwrap();
            (s.errors, s.retrying, s.pending)
        };

        let mut retries = SessionRetries::new();
        let t0 = Instant::now();
        assert!(claimable(&worker, &retries, t0), "never scanned: claimable");

        // 1st failure: waiting out the 5s backoff — RETRYING, not failed.
        facestore::record_scan_error(&worker, "p1", gen, "decode failed").unwrap();
        assert!(!retries.on_failure("p1", t0), "one failure is not terminal");
        assert!(
            !claimable(&worker, &retries, t0 + sec(1)),
            "no hot loop: the backoff really waits"
        );
        assert_eq!(
            status(&retries),
            (0, 1, 1),
            "the panel sees a retrying photo as pending work, not a failure"
        );
        assert!(
            claimable(&worker, &retries, t0 + sec(6)),
            "5s elapsed: retry"
        );

        // 2nd failure: the 30s slot.
        facestore::record_scan_error(&worker, "p1", gen, "decode failed").unwrap();
        assert!(!retries.on_failure("p1", t0 + sec(6)));
        assert!(!claimable(&worker, &retries, t0 + sec(16)), "inside 30s");
        assert!(claimable(&worker, &retries, t0 + sec(40)));

        // 3rd failure: the session budget is spent — only now terminal.
        facestore::record_scan_error(&worker, "p1", gen, "decode failed").unwrap();
        assert!(
            retries.on_failure("p1", t0 + sec(40)),
            "third try was the last"
        );
        assert!(
            !claimable(&worker, &retries, t0 + sec(100_000)),
            "exhausted: never claimed again this session"
        );
        assert_eq!(
            status(&retries),
            (1, 0, 0),
            "terminal: the panel stops scanning and reports the failure"
        );
        let attempts: i64 = worker
            .query_row(
                "SELECT attempts FROM face_scan WHERE photo_id = 'p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 3, "the lifetime diagnostic keeps counting");

        // Relaunch: an empty session map owes the persisted error a fresh
        // round — under the old gate (`retryable = backoff.contains_key`)
        // this claim was false forever and the row was never retried again.
        let mut fresh_session = SessionRetries::new();
        assert!(
            claimable(&worker, &fresh_session, t0),
            "a new session retries the persisted error"
        );
        assert_eq!(status(&fresh_session), (0, 1, 1), "…and says so");

        // Success after a retry: everything clears.
        let snap = facestore::snapshot(&worker, "p1", gen).unwrap();
        assert!(matches!(
            facestore::commit_scan(&mut worker, "p1", &snap, &[], &[], 1, 1, &|| true).unwrap(),
            CommitOutcome::Committed(_)
        ));
        fresh_session.on_success("p1");
        assert!(!claimable(&worker, &fresh_session, t0), "no work when ok");
        assert_eq!(status(&fresh_session), (0, 0, 0));

        // A user rescan outranks a spent budget: the stale claim resets it.
        store.mark_face_stale("p1").unwrap();
        assert!(
            claimable(&worker, &retries, t0),
            "stale is fresh work even for the session that exhausted the photo"
        );
        assert!(retries.on_fresh_claim("p1"), "…and the budget starts over");
        assert!(retries.eligible("p1", t0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn needs_scan_claims_only_real_work() {
        let row = |status: &str, gen: i64| facestore::ScanRow {
            mtime: 10,
            size: 20,
            model_gen: gen,
            status: status.into(),
        };
        assert!(needs_scan(None, 1, Some((10, 20)), false), "never scanned");
        assert!(!needs_scan(Some(&row("ok", 1)), 1, Some((10, 20)), false));
        assert!(needs_scan(Some(&row("stale", 1)), 1, Some((10, 20)), false));
        // An errored photo waits for its own backoff slot, not for another lap.
        assert!(!needs_scan(
            Some(&row("error", 1)),
            1,
            Some((10, 20)),
            false
        ));
        assert!(needs_scan(Some(&row("error", 1)), 1, Some((10, 20)), true));
        // Superseded model generation, and real pixel changes.
        assert!(needs_scan(Some(&row("ok", 1)), 2, Some((10, 20)), false));
        assert!(needs_scan(Some(&row("ok", 1)), 1, Some((11, 20)), false));
        // An unreadable file is not a reason to re-index forever.
        assert!(!needs_scan(Some(&row("ok", 1)), 1, None, false));
    }

    /// Diagnostic: print every YuNet detection and its score for one image,
    /// for tuning `min_det_score` against real misses.
    /// `EMBER_DET_IMAGE=/path/to.jpg cargo test -- --ignored detection_scores --nocapture`
    #[test]
    #[ignore]
    fn detection_scores_for_env_image() {
        let Ok(path) = std::env::var("EMBER_DET_IMAGE") else {
            eprintln!("set EMBER_DET_IMAGE");
            return;
        };
        let mut engine = FaceEngine::new(&models_dir(None), 0.05).expect("models load");
        let img = image::open(&path).expect("image").to_rgb8();
        let faces = engine.detect_embed(&img).expect("inference");
        eprintln!("{}: {} detections ≥0.05", path, faces.len());
        let mut scores: Vec<f32> = faces.iter().map(|f| f.score).collect();
        scores.sort_by(|a, b| b.total_cmp(a));
        for (i, f) in faces.iter().enumerate() {
            eprintln!(
                "  #{i} score={:.3} rect=[{:.3},{:.3},{:.3},{:.3}]",
                f.score, f.rect[0], f.rect[1], f.rect[2], f.rect[3]
            );
        }
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
