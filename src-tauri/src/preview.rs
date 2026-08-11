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
    /// Monotonic per-photo preview generation (absent = 0). Bumped whenever a
    /// photo's cached preview is invalidated, so background work derived from
    /// the old pixels can be discarded before it commits.
    preview_gen: HashMap<String, u64>,
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
                preview_gen: HashMap::new(),
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

    /// Face chip `{id}-f{n}r{rev}.jpg` — square crop baked by the face worker,
    /// self-healed by the repair queue (cache dir is safe to delete). The
    /// name carries the `chip_revision` of the detection that produced the
    /// crop: an artifact from an older scan has a different name and can never
    /// satisfy a current request, however the process died in between.
    pub fn face_chip_path(&self, id: &str, n: i64, revision: i64) -> PathBuf {
        self.cache_dir.join(face_chip_name(id, n, revision))
    }

    /// Pixel invalidation / stale marking: drop this photo's baked chips,
    /// whatever revisions they carry. Best-effort — anything left is
    /// unreachable by current URLs (revisioned names) and the next publish
    /// sweeps it under the write lock.
    pub fn delete_face_chips(&self, id: &str) {
        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return;
        };
        for e in entries.filter_map(Result::ok) {
            if is_photo_chip_file(&e.file_name().to_string_lossy(), id) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Delete-all-face-data cleanup: sweep every baked chip in the cache —
    /// **including temps**, which carry the same biometric crop and would
    /// otherwise outlive a privacy delete (a process killed inside
    /// `publish_bytes` leaves one behind).
    ///
    /// Fallible on purpose. A privacy delete that could not remove a file must
    /// not report success: the caller propagates the failure and the DB's
    /// `chip_sweep_pending` flag keeps the work owed until a sweep that really
    /// removed everything clears it.
    pub fn delete_all_face_chips(&self) -> Result<usize, String> {
        let entries = match std::fs::read_dir(&self.cache_dir) {
            Ok(e) => e,
            // No cache dir at all: there is nothing to delete, which is the
            // outcome we wanted. Any OTHER read failure hides unknown files.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(format!("cache dir unreadable: {e}")),
        };
        let mut removed = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for entry in entries {
            let e = match entry {
                Ok(e) => e,
                Err(err) => {
                    failures.push(format!("directory entry unreadable: {err}"));
                    continue;
                }
            };
            if !is_face_chip_file(&e.file_name().to_string_lossy()) {
                continue;
            }
            match std::fs::remove_file(e.path()) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => failures.push(format!("{}: {err}", e.path().display())),
            }
        }
        if failures.is_empty() {
            Ok(removed)
        } else {
            Err(failures.join("; "))
        }
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

    /// Current preview generation for a photo. **Every listed photo has a
    /// nonzero token**; 0 means "not listed" and is deliberately a value no
    /// snapshot can ever have held. The face worker snapshots it before
    /// decoding and re-checks before its commit — the plan's cheap in-process
    /// early discard, on top of (never instead of) the DB guards.
    pub fn preview_gen(&self, id: &str) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .preview_gen
            .get(id)
            .copied()
            .unwrap_or(0)
    }

    /// The cached preview for this photo is gone (source pixels changed, or it
    /// left the listing): anything still being computed from it is superseded.
    /// Tokens come from a process-global counter and are never reused, so a
    /// stale snapshot can never compare equal to a later one — and a photo
    /// dropped from the map reads 0, which no snapshot of a listed photo ever
    /// was.
    pub fn invalidate_preview(&self, id: &str) {
        let gen = next_preview_gen();
        self.inner
            .lock()
            .unwrap()
            .preview_gen
            .insert(id.to_string(), gen);
    }

    /// Ids of the folder open right now — the cache janitor's protected set.
    pub fn current_ids(&self) -> std::collections::HashSet<String> {
        self.inner.lock().unwrap().by_id.keys().cloned().collect()
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
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
        let by_id: HashMap<String, Arc<PhotoEntry>> =
            entries.iter().map(|e| (e.id.clone(), e.clone())).collect();
        let mut inner = self.inner.lock().unwrap();
        // Photos this listing doesn't contain lose their token and read 0
        // again — which is exactly how in-flight work for a folder we just
        // left gets discarded. Photos that stay keep theirs, so an
        // invalidation recorded moments ago (scan_folder invalidates before it
        // sets entries) still kills the work it was meant to. Photos that are
        // new to the listing get a fresh nonzero token: without one, a job
        // snapshotting 0 for a never-invalidated photo would still read 0
        // after the photo left, and would not discard.
        inner.preview_gen.retain(|id, _| by_id.contains_key(id));
        for id in by_id.keys() {
            if !inner.preview_gen.contains_key(id) {
                inner.preview_gen.insert(id.clone(), next_preview_gen());
            }
        }
        inner.by_id = by_id;
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

/// The chip filename scheme, shared with `facestore::publish_chips` (which
/// computes the expected artifact set from DB rows): index and the
/// `chip_revision` of the detection that produced the crop.
pub(crate) fn face_chip_name(id: &str, n: i64, revision: i64) -> String {
    format!("{id}-f{n}r{revision}.jpg")
}

/// `{16-hex}-f{n}r{rev}.jpg` (a published chip), the pre-revision legacy shape
/// `{16-hex}-f{n}.jpg`, or either with a `.tmp{pid}-{seq}` extension (staged
/// by a writer that died mid-publish). Nothing else in the cache dir matches:
/// sibling artifacts are `-t`, `-h`, `-m`, `-raf`.
fn is_face_chip_file(name: &str) -> bool {
    let Some((id, rest)) = name.split_once("-f") else {
        return false;
    };
    if id.len() != 16 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    chip_suffix_shape(rest)
}

/// Same shape test scoped to ONE photo (`{photo_id}-f…`), for per-photo
/// cleanup and the publish-time stale sweep. Unlike `is_face_chip_file` it
/// takes the id as given, so tests with short ids behave like production.
pub(crate) fn is_photo_chip_file(name: &str, photo_id: &str) -> bool {
    name.strip_prefix(photo_id)
        .and_then(|rest| rest.strip_prefix("-f"))
        .is_some_and(chip_suffix_shape)
}

/// `<digits>[r<digits>].jpg` or the same with a `tmp…` extension.
fn chip_suffix_shape(rest: &str) -> bool {
    let Some((stem, ext)) = rest.split_once('.') else {
        return false;
    };
    let (index, revision) = match stem.split_once('r') {
        Some((i, rev)) => (i, Some(rev)),
        None => (stem, None),
    };
    if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if let Some(rev) = revision {
        if rev.is_empty() || !rev.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    ext == "jpg" || ext.starts_with("tmp")
}

/// Process-global, never reused: two different states of a photo's preview can
/// never share a token, in either direction.
fn next_preview_gen() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_GEN: AtomicU64 = AtomicU64::new(1);
    NEXT_GEN.fetch_add(1, Ordering::Relaxed)
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
    write_jpeg(&small, out, THUMB_QUALITY)
}

/// Encode a JPEG **into memory**. Face chips are biometric data: they are
/// encoded here and stay in RAM until `facestore::publish_chips` has taken the
/// DB write lock and re-checked that a privacy delete hasn't happened, so
/// there is never a moment where a chip exists on disk unaccounted for.
pub(crate) fn encode_jpeg(img: &RgbImage, quality: u8) -> std::io::Result<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode_image(img).map_err(std::io::Error::other)?;
    Ok(buf)
}

/// Publish already-encoded bytes: unique temp beside the destination, then
/// rename, so concurrent writers can never interleave into one file and a
/// truncated write can never be mistaken for a finished artifact. The temp is
/// removed on EVERY failure path — a write that dies half-done (full disk)
/// must not leave a partial temp behind, least of all a face-chip one.
pub(crate) fn publish_bytes(out: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = unique_tmp(out);
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    match std::fs::rename(&tmp, out) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Atomic-ish JPEG publish — thumbs and any other derived image.
pub(crate) fn write_jpeg(img: &RgbImage, out: &Path, quality: u8) -> std::io::Result<()> {
    publish_bytes(out, &encode_jpeg(img, quality)?)
}

pub(crate) fn resize_rgb(src: &RgbImage, dw: u32, dh: u32) -> Result<RgbImage, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> Arc<PhotoEntry> {
        Arc::new(PhotoEntry {
            id: id.into(),
            dir: "/tmp/pv".into(),
            stem: id.to_uppercase(),
            jpeg: Some(format!("/tmp/pv/{id}.JPG").into()),
            raf: None,
            mtime: 1,
            size: 1,
        })
    }

    fn state() -> (PreviewState, PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "emberpv-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (
            PreviewState::new(dir.clone(), crate::exposure::BlinkiesCfg::default()),
            dir,
        )
    }

    /// The face worker's early-discard token: a photo whose preview was
    /// invalidated mid-inference must not compare equal to its snapshot.
    #[test]
    fn preview_generations_move_only_on_invalidation() {
        let (s, dir) = state();
        s.set_entries(vec![entry("a"), entry("b")]);
        let snapshot = s.preview_gen("a");
        assert_ne!(snapshot, 0, "a listed photo always has a real token");
        s.invalidate_preview("a");
        assert_ne!(
            s.preview_gen("a"),
            snapshot,
            "work from the old pixels dies"
        );
        let b_before = s.preview_gen("b");
        assert_ne!(b_before, 0);
        assert_eq!(s.preview_gen("b"), b_before, "other photos unaffected");

        // Re-listing the same folder (scan_folder invalidates, THEN sets
        // entries) must not lose the invalidation that just happened.
        let after_invalidate = s.preview_gen("a");
        s.set_entries(vec![entry("a"), entry("b")]);
        assert_eq!(s.preview_gen("a"), after_invalidate);

        // A photo that leaves the listing is forgotten — and because
        // generations are never reused, its old value can't come back as a
        // false match if it returns.
        s.set_entries(vec![entry("b")]);
        assert_eq!(s.preview_gen("a"), 0);
        s.set_entries(vec![entry("a"), entry("b")]);
        assert_ne!(
            s.preview_gen("a"),
            after_invalidate,
            "a fresh token on return"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The zero-value collision the follow-up review found: a photo that was
    /// NEVER invalidated is snapshotted while the folder is open, then the
    /// user opens another folder. Its bookkeeping is dropped — and if "absent"
    /// and "never invalidated" both read 0, the in-flight job compares equal
    /// and commits work about a folder that is gone.
    #[test]
    fn leaving_a_folder_discards_a_never_invalidated_photos_work() {
        let (s, dir) = state();
        s.set_entries(vec![entry("a"), entry("b")]);
        let snapshot = s.preview_gen("a"); // the worker's pre-inference read
        s.set_entries(vec![entry("c")]); // …the user opens another folder
        assert_ne!(
            s.preview_gen("a"),
            snapshot,
            "the in-flight job must not find its own token"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A privacy delete may not report success while biometric files remain,
    /// so the sweep is fallible. A directory sitting where a chip file should
    /// be makes `remove_file` fail on every platform and as any user.
    #[test]
    fn a_chip_that_cannot_be_removed_fails_the_sweep() {
        let (s, dir) = state();
        let id = "00deadbeef00cafe";
        std::fs::write(dir.join(format!("{id}-f0.jpg")), b"chip").unwrap();
        std::fs::create_dir(dir.join(format!("{id}-f1.jpg"))).unwrap();
        let err = s.delete_all_face_chips().expect_err("removal must fail");
        assert!(err.contains("-f1.jpg"), "the failure names the file: {err}");
        assert!(
            !dir.join(format!("{id}-f0.jpg")).exists(),
            "the removable chips still go"
        );
        // With the obstruction gone the sweep succeeds and reports the count.
        std::fs::remove_dir(dir.join(format!("{id}-f1.jpg"))).unwrap();
        std::fs::write(dir.join(format!("{id}-f2.tmp1-1")), b"temp").unwrap();
        assert_eq!(s.delete_all_face_chips().unwrap(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn face_chip_files_are_recognized_by_shape() {
        let id = "00deadbeef00cafe";
        assert!(
            is_face_chip_file(&face_chip_name(id, 0, 3)),
            "the current revisioned shape"
        );
        assert!(is_face_chip_file(&format!("{id}-f12r400.jpg")));
        assert!(
            is_face_chip_file(&format!("{id}-f0.jpg")),
            "the pre-revision legacy shape must still be swept on upgrade"
        );
        for tmp in [format!("{id}-f3.tmp1234-9"), format!("{id}-f3r7.tmp1234-9")] {
            assert!(
                is_face_chip_file(&tmp),
                "a staged temp holds the same crop and must be swept too: {tmp}"
            );
        }
        for other in [
            format!("{id}.jpg"),
            format!("{id}-t.jpg"),
            format!("{id}-h.json"),
            format!("{id}-m.png"),
            format!("{id}-raf.jpg"),
            format!("{id}-f.jpg"),
            format!("{id}-fx.jpg"),
            format!("{id}-f0r.jpg"),
            format!("{id}-f0rx.jpg"),
            "blinkies.fingerprint".to_string(),
        ] {
            assert!(!is_face_chip_file(&other), "{other} is not a chip");
        }
        // The per-photo variant scopes the same shape to one id.
        assert!(is_photo_chip_file(&face_chip_name(id, 1, 9), id));
        assert!(is_photo_chip_file(&format!("{id}-f1.jpg"), id));
        assert!(!is_photo_chip_file(&face_chip_name(id, 1, 9), "otherid"));
        assert!(!is_photo_chip_file(&format!("{id}-t.jpg"), id));
    }
}
