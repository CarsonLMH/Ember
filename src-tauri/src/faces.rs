//! Faces — Slice 0 inference spike (SPEC §14, plan rev 4).
//!
//! `EMBER_FACES_FORCE=1` runs the real detect→embed loop continuously on one
//! background thread so the flip-storm gate measures against genuinely active
//! inference, never a timing assumption. No DB writes, no chips yet — this
//! slice exists to prove the perf budget and the packaging before Slice A
//! builds on it. All pure math lives in `facedet.rs`; this module owns the
//! ONNX Runtime sessions and the loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;
use sha2::{Digest, Sha256};

use crate::facedet;
use crate::preview::PreviewState;

/// Pinned model assets (opencv_zoo). A hash mismatch is logged loudly at init:
/// embeddings from a different export live in a different space (Slice A turns
/// this into the model-generation registry).
pub const YUNET_FILE: &str = "face_detection_yunet_2023mar.onnx";
pub const YUNET_SHA256: &str = "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4";
pub const SFACE_FILE: &str = "face_recognition_sface_2021dec.onnx";
pub const SFACE_SHA256: &str = "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79";

/// Spike default; ships in settings.toml `[faces] min_det_score` from Slice A.
const MIN_DET_SCORE: f32 = 0.8;

// ---------- spike stats (read by the harness via faces_spike_stats) ----------

static PHOTOS: AtomicU64 = AtomicU64::new(0);
static FACES: AtomicU64 = AtomicU64::new(0);
static PASSES: AtomicU64 = AtomicU64::new(0);
static DECODE_US: AtomicU64 = AtomicU64::new(0);
static DETECT_US: AtomicU64 = AtomicU64::new(0);
static EMBED_US: AtomicU64 = AtomicU64::new(0);
static INIT_MS: AtomicU64 = AtomicU64::new(0);
static ERRORS: AtomicU64 = AtomicU64::new(0);

pub fn force_enabled() -> bool {
    std::env::var("EMBER_FACES_FORCE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

pub fn spike_stats() -> serde_json::Value {
    let photos = PHOTOS.load(Ordering::Relaxed);
    let us = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1000.0 / photos.max(1) as f64;
    serde_json::json!({
        "photos": photos,
        "faces": FACES.load(Ordering::Relaxed),
        "passes": PASSES.load(Ordering::Relaxed),
        "errors": ERRORS.load(Ordering::Relaxed),
        "avgDecodeMs": us(&DECODE_US),
        "avgDetectMs": us(&DETECT_US),
        "avgEmbedMs": us(&EMBED_US),
        "avgTotalMs": us(&DECODE_US) + us(&DETECT_US) + us(&EMBED_US),
        "initMs": INIT_MS.load(Ordering::Relaxed),
        "rssMb": rss_mb(),
    })
}

fn rss_mb() -> u64 {
    // Spike diagnostic only — one `ps` every stats call is fine.
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

// Slice 0 only exercises the pipeline; Slice A persists these fields.
#[cfg_attr(not(test), allow(dead_code))]
pub struct FaceResult {
    /// Normalized [0,1] display-space rect (previews are orientation-applied).
    pub rect: [f32; 4],
    pub score: f32,
    /// Preview-pixel landmarks.
    pub landmarks: [[f32; 2]; 5],
    /// L2-normalized 128-d SFace embedding.
    pub embedding: Vec<f32>,
}

pub struct FaceEngine {
    det: Session,
    rec: Session,
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

impl FaceEngine {
    pub fn new(models_dir: &Path) -> Result<Self, String> {
        let t0 = Instant::now();
        for (file, pinned) in [(YUNET_FILE, YUNET_SHA256), (SFACE_FILE, SFACE_SHA256)] {
            let actual = sha256_hex(&models_dir.join(file))?;
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
                MIN_DET_SCORE,
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

// ---------- spike worker ----------

/// One thread, lowest priority in spirit: only photos whose preview file
/// already exists, cursor-prioritized via `entries_by_distance`. Loops over
/// the folder CONTINUOUSLY (`passes` counts full sweeps) so a concurrent
/// flip-storm gate always measures against live inference.
pub fn spawn_spike(preview: Arc<PreviewState>, models_dir: PathBuf) {
    std::thread::Builder::new()
        .name("faces-spike".into())
        .spawn(move || {
            let mut engine: Option<FaceEngine> = None;
            let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
            eprintln!("faces-spike: enabled, models at {}", models_dir.display());
            loop {
                let job = preview
                    .entries_by_distance()
                    .into_iter()
                    .find(|e| !done.contains(&e.id) && preview.preview_path_for_id(&e.id).exists());
                let Some(entry) = job else {
                    if done.is_empty() {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    } else {
                        PASSES.fetch_add(1, Ordering::Relaxed);
                        eprintln!(
                            "faces-spike: pass {} complete — {}",
                            PASSES.load(Ordering::Relaxed),
                            spike_stats()
                        );
                        done.clear();
                    }
                    continue;
                };
                if engine.is_none() {
                    match FaceEngine::new(&models_dir) {
                        Ok(e) => {
                            eprintln!(
                                "faces-spike: sessions ready in {}ms, rss={}MB",
                                INIT_MS.load(Ordering::Relaxed),
                                rss_mb()
                            );
                            engine = Some(e);
                        }
                        Err(e) => {
                            // Terminal for the spike: log once and park.
                            eprintln!("faces-spike: model init FAILED: {e}");
                            return;
                        }
                    }
                }
                let t0 = Instant::now();
                let decoded = std::fs::read(preview.preview_path_for_id(&entry.id))
                    .ok()
                    .and_then(|b| image::load_from_memory(&b).ok())
                    .map(|i| i.to_rgb8());
                DECODE_US.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
                match decoded {
                    Some(img) => match engine.as_mut().unwrap().detect_embed(&img) {
                        Ok(faces) => {
                            FACES.fetch_add(faces.len() as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            ERRORS.fetch_add(1, Ordering::Relaxed);
                            eprintln!("faces-spike: {} inference error: {e}", entry.id);
                        }
                    },
                    None => {
                        ERRORS.fetch_add(1, Ordering::Relaxed);
                    }
                }
                let n = PHOTOS.fetch_add(1, Ordering::Relaxed) + 1;
                done.insert(entry.id.clone());
                if n.is_multiple_of(25) {
                    eprintln!("faces-spike: {}", spike_stats());
                }
            }
        })
        .expect("spawn faces-spike worker");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Real models + a real photograph — validates the fixed 640×640
    /// letterbox strategy end to end (Slice 0 acceptance). `#[ignore]` only
    /// because it needs the 38MB model files; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn detects_known_face_and_embeds_deterministically() {
        let models = models_dir(None);
        let mut engine = FaceEngine::new(&models).expect("models load");
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
        // Rect sane and normalized.
        for v in f.rect {
            assert!((0.0..=1.0).contains(&v), "rect {:?}", f.rect);
        }
        // Landmarks inside (padded) rect, in preview pixels.
        let (w, h) = img.dimensions();
        let (rx, ry) = (f.rect[0] * w as f32, f.rect[1] * h as f32);
        let (rw, rh) = (f.rect[2] * w as f32, f.rect[3] * h as f32);
        for lm in f.landmarks {
            assert!(lm[0] > rx - rw * 0.25 && lm[0] < rx + rw * 1.25, "{lm:?}");
            assert!(lm[1] > ry - rh * 0.25 && lm[1] < ry + rh * 1.25, "{lm:?}");
        }
        // Embedding: 128-d, unit norm, deterministic across runs.
        assert_eq!(f.embedding.len(), facedet::EMBED_DIM);
        let norm: f32 = f.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
        let again = engine.detect_embed(&img).expect("second run");
        assert_eq!(faces.len(), again.len());
        let cos = facedet::cosine(&f.embedding, &again[0].embedding);
        assert!(cos > 0.9999, "same input must embed identically, cos={cos}");
    }
}
