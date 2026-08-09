use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use image::RgbImage;

use crate::fastexif;
use crate::scanner::PhotoEntry;

/// Long edge of generated previews. Sized for retina fit-view display;
/// full-res originals are only decoded in zoom mode (slice 4).
const PREVIEW_LONG_EDGE: u32 = 2600;
const JPEG_QUALITY: u8 = 85;
/// Filmstrip thumbnails — decoding 2600px previews for 100px rows would cost
/// ~25MB of decoded pixels per visible row in the webview.
const THUMB_LONG_EDGE: u32 = 240;
const THUMB_QUALITY: u8 = 80;

struct Inner {
    /// Entries in the frontend's current display order (anchor distances follow it).
    entries: Vec<Arc<PhotoEntry>>,
    by_id: HashMap<String, Arc<PhotoEntry>>,
    anchor: usize,
    in_flight: HashSet<String>,
    done: HashSet<String>,
}

pub struct PreviewState {
    cache_dir: PathBuf,
    blinkies: crate::exposure::BlinkiesCfg,
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl PreviewState {
    /// Masks bake threshold+color values; when the settings fingerprint
    /// changes, every cached mask is stale and gets regenerated lazily.
    pub fn new(cache_dir: PathBuf, blinkies: crate::exposure::BlinkiesCfg) -> Self {
        let fp_file = cache_dir.join("blinkies.fingerprint");
        let current = blinkies.fingerprint();
        if std::fs::read_to_string(&fp_file).ok().as_deref() != Some(current.as_str()) {
            if let Ok(entries) = std::fs::read_dir(&cache_dir) {
                for e in entries.filter_map(Result::ok) {
                    if e.file_name().to_string_lossy().ends_with("-m.png") {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            let _ = std::fs::write(&fp_file, &current);
        }
        Self {
            cache_dir,
            blinkies,
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                by_id: HashMap::new(),
                anchor: 0,
                in_flight: HashSet::new(),
                done: HashSet::new(),
            }),
            cv: Condvar::new(),
        }
    }

    /// Keyed by id only; validity (mtime/size) is tracked in the store so an
    /// XMP rewrite — which changes mtime but not pixels — keeps the preview.
    pub fn preview_path(&self, entry: &PhotoEntry) -> PathBuf {
        self.preview_path_for_id(&entry.id)
    }

    /// Previews outlive their entries (trashed photos leave the scan but the
    /// trash panel still needs thumbnails), so lookup by id must not require one.
    pub fn preview_path_for_id(&self, id: &str) -> PathBuf {
        self.cache_dir.join(format!("{id}.jpg"))
    }

    pub fn thumb_path_for_id(&self, id: &str) -> PathBuf {
        self.cache_dir.join(format!("{id}-t.jpg"))
    }

    fn raf_source_path(&self, id: &str) -> PathBuf {
        self.cache_dir.join(format!("{id}-raf.jpg"))
    }

    /// RAF-only photos display via the full-size JPEG embedded in the RAF,
    /// extracted once into the cache. One-shot exiftool (binary-safe), not the
    /// stay_open worker (which is line-oriented).
    fn ensure_raf_source(&self, entry: &PhotoEntry) -> Option<PathBuf> {
        let out = self.raf_source_path(&entry.id);
        if out.exists() {
            return Some(out);
        }
        let raf = entry.raf.as_ref()?;
        // Fuji RAFs expose the full-size embedded JPEG as PreviewImage.
        for tag in ["-PreviewImage", "-JpgFromRaw"] {
            let result = std::process::Command::new(crate::xmp::exiftool_bin())
                .args(["-b", tag])
                .arg(raf)
                .output()
                .ok()?;
            if result.status.success() && result.stdout.len() > 1024 {
                let tmp = unique_tmp(&out);
                std::fs::write(&tmp, &result.stdout).ok()?;
                std::fs::rename(&tmp, &out).ok()?;
                return Some(out);
            }
        }
        None
    }

    /// The full-resolution source served by /orig: the JPEG, or for RAF-only
    /// photos the extracted embedded JPEG.
    pub fn orig_source(&self, id: &str) -> Option<PathBuf> {
        let entry = self.entry(id)?;
        entry
            .jpeg
            .clone()
            .or_else(|| self.ensure_raf_source(&entry))
    }

    /// Lazily derive a thumbnail from an existing preview (covers cache entries
    /// generated before thumbnails existed). Cheap: 2600px decode + resize.
    pub fn ensure_thumb(&self, id: &str) -> Option<PathBuf> {
        let thumb = self.thumb_path_for_id(id);
        if thumb.exists() {
            return Some(thumb);
        }
        let preview = self.preview_path_for_id(id);
        let bytes = std::fs::read(preview).ok()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgb8();
        write_thumb(&img, &thumb).ok()?;
        Some(thumb)
    }

    pub fn hist_path_for_id(&self, id: &str) -> PathBuf {
        self.cache_dir.join(format!("{id}-h.json"))
    }

    pub fn mask_path_for_id(&self, id: &str) -> PathBuf {
        self.cache_dir.join(format!("{id}-m.png"))
    }

    /// Histogram + clipping mask, lazily derived from the preview when the
    /// sweep hasn't written them (or the cache predates this feature).
    pub fn ensure_exposure(&self, id: &str) -> Option<()> {
        let hist = self.hist_path_for_id(id);
        let mask = self.mask_path_for_id(id);
        if hist.exists() && mask.exists() {
            return Some(());
        }
        let bytes = std::fs::read(self.preview_path_for_id(id)).ok()?;
        let img = image::load_from_memory(&bytes).ok()?.to_rgb8();
        self.write_exposure(id, &img).ok()?;
        Some(())
    }

    fn write_exposure(&self, id: &str, preview: &RgbImage) -> std::io::Result<()> {
        let (hist, mask) = crate::exposure::compute(preview, self.blinkies);
        let hist_out = self.hist_path_for_id(id);
        let tmp = unique_tmp(&hist_out);
        std::fs::write(&tmp, serde_json::to_vec(&hist)?)?;
        std::fs::rename(&tmp, &hist_out)?;
        let mask_out = self.mask_path_for_id(id);
        let tmp = unique_tmp(&mask_out);
        mask.save_with_format(&tmp, image::ImageFormat::Png)
            .map_err(std::io::Error::other)?;
        std::fs::rename(&tmp, &mask_out)?;
        Ok(())
    }

    pub fn entry(&self, id: &str) -> Option<Arc<PhotoEntry>> {
        self.inner.lock().unwrap().by_id.get(id).cloned()
    }

    /// Snapshot of entries ordered by distance from the cursor (nearest first,
    /// forward-biased) — for background workers that want cursor priority.
    pub fn entries_by_distance(&self) -> Vec<Arc<PhotoEntry>> {
        let inner = self.inner.lock().unwrap();
        let anchor = inner.anchor as i64;
        let mut indexed: Vec<(i64, Arc<PhotoEntry>)> = inner
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let d = i as i64 - anchor;
                (if d >= 0 { d } else { -d * 3 }, e.clone())
            })
            .collect();
        indexed.sort_by_key(|(score, _)| *score);
        indexed.into_iter().map(|(_, e)| e).collect()
    }

    pub fn set_entries(&self, entries: Vec<Arc<PhotoEntry>>) {
        let mut inner = self.inner.lock().unwrap();
        inner.by_id = entries.iter().map(|e| (e.id.clone(), e.clone())).collect();
        inner.entries = entries;
        inner.anchor = 0;
        inner.done.clear();
        drop(inner);
        self.cv.notify_all();
    }

    /// Reorder to match the frontend's sort so anchor distance = strip distance.
    pub fn set_order(&self, ids: &[String]) {
        let mut inner = self.inner.lock().unwrap();
        let mut reordered = Vec::with_capacity(inner.entries.len());
        let mut seen = HashSet::new();
        for id in ids {
            if let Some(e) = inner.by_id.get(id) {
                reordered.push(e.clone());
                seen.insert(id.clone());
            }
        }
        for e in &inner.entries {
            if !seen.contains(&e.id) {
                reordered.push(e.clone());
            }
        }
        inner.entries = reordered;
        drop(inner);
        self.cv.notify_all();
    }

    pub fn set_anchor(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(idx) = inner.entries.iter().position(|e| e.id == *id) {
            inner.anchor = idx;
        }
        drop(inner);
        self.cv.notify_all();
    }

    /// Pick the ungenerated entry closest to the anchor (forward-biased 3:1).
    fn next_job(&self) -> Arc<PhotoEntry> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            let anchor = inner.anchor as i64;
            let mut best: Option<(i64, &Arc<PhotoEntry>)> = None;
            for (i, e) in inner.entries.iter().enumerate() {
                if (e.jpeg.is_none() && e.raf.is_none())
                    || inner.done.contains(&e.id)
                    || inner.in_flight.contains(&e.id)
                {
                    continue;
                }
                let d = i as i64 - anchor;
                let score = if d >= 0 { d } else { -d * 3 };
                if best.is_none_or(|(s, _)| score < s) {
                    best = Some((score, e));
                }
            }
            if let Some((_, e)) = best {
                let e = e.clone();
                inner.in_flight.insert(e.id.clone());
                return e;
            }
            inner = self.cv.wait(inner).unwrap();
        }
    }

    fn finish_job(&self, id: &str, ok: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.in_flight.remove(id);
        // Failures are marked done too: retrying a corrupt file forever helps no one.
        let _ = ok;
        inner.done.insert(id.to_string());
    }

    fn ensure_preview(&self, entry: &PhotoEntry) -> std::io::Result<()> {
        let out = self.preview_path(entry);
        if out.exists() {
            return Ok(());
        }
        let src = match &entry.jpeg {
            Some(p) => p.clone(),
            None => self
                .ensure_raf_source(entry)
                .ok_or_else(|| std::io::Error::other("no displayable source"))?,
        };
        let img = generate_preview(&src)?;
        // Atomic-ish publish: write sibling temp file, then rename.
        let tmp = unique_tmp(&out);
        {
            let file = std::fs::File::create(&tmp)?;
            let mut w = std::io::BufWriter::new(file);
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, JPEG_QUALITY);
            enc.encode_image(&img).map_err(std::io::Error::other)?;
        }
        std::fs::rename(&tmp, &out)?;
        // Thumbnail + histogram + clipping mask ride along from the one decode.
        let _ = write_thumb(&img, &self.thumb_path_for_id(&entry.id));
        let _ = self.write_exposure(&entry.id, &img);
        Ok(())
    }
}

/// Unique temp path per write: concurrent workers deriving the same artifact
/// (e.g. two thumb requests racing) must never interleave into one temp file.
fn unique_tmp(out: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    out.with_extension(format!(
        "tmp{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Decode → downscale (NEON) → apply EXIF orientation. Returns display-ready RGB.
fn generate_preview(src: &Path) -> std::io::Result<RgbImage> {
    let bytes = std::fs::read(src)?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(std::io::Error::other)?
        .to_rgb8();
    let (w, h) = decoded.dimensions();

    let long = w.max(h);
    let resized = if long > PREVIEW_LONG_EDGE {
        let scale = PREVIEW_LONG_EDGE as f64 / long as f64;
        let dw = ((w as f64 * scale).round() as u32).max(1);
        let dh = ((h as f64 * scale).round() as u32).max(1);
        resize_rgb(&decoded, dw, dh).map_err(std::io::Error::other)?
    } else {
        decoded
    };

    Ok(apply_orientation(resized, fastexif::orientation(src)))
}

fn write_thumb(preview: &RgbImage, out: &Path) -> std::io::Result<()> {
    let (w, h) = preview.dimensions();
    let long = w.max(h).max(1);
    let scale = THUMB_LONG_EDGE as f64 / long as f64;
    let dw = ((w as f64 * scale).round() as u32).max(1);
    let dh = ((h as f64 * scale).round() as u32).max(1);
    let small = resize_rgb(preview, dw, dh).map_err(std::io::Error::other)?;
    let tmp = unique_tmp(out);
    {
        let file = std::fs::File::create(&tmp)?;
        let mut wtr = std::io::BufWriter::new(file);
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut wtr, THUMB_QUALITY);
        enc.encode_image(&small).map_err(std::io::Error::other)?;
    }
    std::fs::rename(&tmp, out)?;
    Ok(())
}

fn resize_rgb(src: &RgbImage, dw: u32, dh: u32) -> Result<RgbImage, String> {
    use fast_image_resize::images::Image;
    use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

    let (w, h) = src.dimensions();
    let src_img = Image::from_vec_u8(w, h, src.as_raw().clone(), PixelType::U8x3)
        .map_err(|e| e.to_string())?;
    let mut dst = Image::new(dw, dh, PixelType::U8x3);
    Resizer::new()
        .resize(
            &src_img,
            &mut dst,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .map_err(|e| e.to_string())?;
    RgbImage::from_raw(dw, dh, dst.into_vec()).ok_or_else(|| "resize buffer mismatch".into())
}

fn apply_orientation(img: RgbImage, orientation: u32) -> RgbImage {
    use image::imageops;
    match orientation {
        2 => imageops::flip_horizontal(&img),
        3 => imageops::rotate180(&img),
        4 => imageops::flip_vertical(&img),
        5 => imageops::rotate90(&imageops::flip_horizontal(&img)),
        6 => imageops::rotate90(&img),
        7 => imageops::rotate270(&imageops::flip_horizontal(&img)),
        8 => imageops::rotate270(&img),
        _ => img,
    }
}

/// Background sweep: N workers generating previews prioritized around the cursor.
/// Successful generations record source (mtime, size) in the store for validity.
pub fn spawn_workers(state: Arc<PreviewState>, store: Arc<crate::store::Store>, n: usize) {
    for i in 0..n {
        let state = state.clone();
        let store = store.clone();
        std::thread::Builder::new()
            .name(format!("preview-{i}"))
            .spawn(move || loop {
                let entry = state.next_job();
                let ok = state.ensure_preview(&entry).is_ok();
                if ok {
                    if let Some(src) = entry.display_file() {
                        if let Ok(meta) = std::fs::metadata(src) {
                            let mtime = meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let _ = store.set_preview_stat(&entry.id, mtime, meta.len());
                        }
                    }
                }
                state.finish_job(&entry.id, ok);
            })
            .expect("spawn preview worker");
    }
}
