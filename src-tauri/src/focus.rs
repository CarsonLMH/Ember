use std::sync::Arc;

use image::RgbImage;
use tauri::Emitter;

use crate::metadata;
use crate::preview::PreviewState;
use crate::store::Store;

// Focus check: sharpness (Laplacian variance) of a patch around the camera's
// recorded AF point, computed from the cached preview. The score is
// texture-dependent — a sharp flat wall scores lower than out-of-focus grass —
// so Ember stores and displays it but never turns it into a verdict; any
// threshold that lights up chrome is the user's, in settings.toml.
//
// Calibration anchor (X-T50 burst pair, 2600px previews, 2026-08-20): a
// clearly missed frame scored ~90 where its sharp burst-mate scored ~270,
// while patches away from the AF point matched within ±2% across the pair.

/// What the MakerNotes say about where (and whether) to measure.
#[derive(Debug, PartialEq)]
pub enum AfInfo {
    /// Display-normalized AF point + the camera's AF box size setting (1-6).
    Point {
        nx: f64,
        ny: f64,
        point_size: Option<i64>,
    },
    /// No meaningful point to score; the &str is the stored status.
    Skip(&'static str),
}

fn str_value<'a>(v: &'a serde_json::Value, suffix: &str) -> Option<&'a str> {
    v.as_object()?
        .iter()
        .find(|(k, _)| k.ends_with(suffix))
        .and_then(|(_, val)| val.as_str())
}

/// Decide from a cached MakerNotes dump whether this photo has a scoreable AF
/// point. Manual focus records a stale FocusPixel and Wide/Tracking has no
/// single point, so both park with an honest status instead of a wrong score.
pub fn af_analysis(json: &str) -> AfInfo {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return AfInfo::Skip("no_af");
    };
    if str_value(&v, ":FocusMode").is_some_and(|s| s.to_ascii_lowercase().contains("manual")) {
        return AfInfo::Skip("manual");
    }
    if str_value(&v, ":AFAreaMode").is_some_and(|s| s.contains("Tracking")) {
        return AfInfo::Skip("tracking");
    }
    let Some((nx, ny)) = metadata::focus_point(json) else {
        return AfInfo::Skip("no_af");
    };
    let point_size = v
        .as_object()
        .and_then(|o| o.iter().find(|(k, _)| k.ends_with(":AFAreaPointSize")))
        .and_then(|(_, val)| {
            val.as_i64()
                .or_else(|| val.as_str().and_then(|s| s.parse().ok()))
        });
    AfInfo::Point { nx, ny, point_size }
}

/// Half-edge of the measured patch, scaled from the camera's AF box size
/// setting (1-6, 3 = default → ~5% of the long edge, the calibrated size).
pub fn patch_half(w: u32, h: u32, point_size: Option<i64>) -> u32 {
    let long = w.max(h) as f64;
    let size = point_size.filter(|s| (1..=6).contains(s)).unwrap_or(3) as f64;
    let half = long * 0.026 * (size / 3.0);
    let cap = ((long * 0.05) as u32).max(24);
    (half as u32).clamp(24, cap)
}

/// Variance of the 4-neighbor Laplacian over the patch centered on (cx, cy),
/// clamped inside the image. Higher = sharper, for the same subject texture.
pub fn laplacian_variance(img: &RgbImage, cx: u32, cy: u32, half: u32) -> f64 {
    let (w, h) = img.dimensions();
    if w < 4 || h < 4 {
        return 0.0;
    }
    let side = (2 * half).min(w - 2).min(h - 2);
    let x0 = (cx.saturating_sub(half)).min(w - 2 - side).max(1);
    let y0 = (cy.saturating_sub(half)).min(h - 2 - side).max(1);
    let luma = |x: u32, y: u32| -> f64 {
        let [r, g, b] = img.get_pixel(x, y).0;
        (r as f64 + g as f64 + b as f64) / 3.0
    };
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    let mut n = 0.0;
    for y in y0..y0 + side {
        for x in x0..x0 + side {
            let lap = luma(x - 1, y) + luma(x + 1, y) + luma(x, y - 1) + luma(x, y + 1)
                - 4.0 * luma(x, y);
            sum += lap;
            sum_sq += lap * lap;
            n += 1.0;
        }
    }
    if n == 0.0 {
        return 0.0;
    }
    let mean = sum / n;
    sum_sq / n - mean * mean
}

/// Score one photo from its cached preview (already display-oriented, like the
/// normalized AF point). Returns the id when a row was written, for the
/// progress batch. A preview_gen change under us means the pixels are
/// superseded — write nothing; the photo stays unclaimed and comes around
/// again with fresh pixels.
fn process_photo(store: &Store, preview: &PreviewState, id: &str) -> Option<String> {
    let gen = preview.preview_gen(id);
    if gen == 0 {
        return None; // no longer listed
    }
    let json = store.metadata_json(id).ok().flatten()?;
    match af_analysis(&json) {
        AfInfo::Skip(reason) => {
            store
                .set_focus_scan(id, reason, None, None, None, None)
                .ok()?;
            Some(id.to_string())
        }
        AfInfo::Point { nx, ny, point_size } => {
            let img = std::fs::read(preview.preview_path_for_id(id))
                .ok()
                .and_then(|b| image::load_from_memory(&b).ok())
                .map(|i| i.to_rgb8());
            let Some(img) = img else {
                if preview.preview_gen(id) != gen {
                    return None; // preview regenerating — transient, retry later
                }
                store
                    .set_focus_scan(id, "error", None, None, None, None)
                    .ok()?;
                return Some(id.to_string());
            };
            let (w, h) = img.dimensions();
            let half = patch_half(w, h, point_size);
            let cx = (nx * (w - 1) as f64).round() as u32;
            let cy = (ny * (h - 1) as f64).round() as u32;
            let score = laplacian_variance(&img, cx, cy, half);
            if preview.preview_gen(id) != gen {
                return None;
            }
            store
                .set_focus_scan(
                    id,
                    "ok",
                    Some(score),
                    Some(nx),
                    Some(ny),
                    Some(2 * half as i64),
                )
                .ok()?;
            Some(id.to_string())
        }
    }
}

/// Emit `focus-progress` for the batch's folder — the HUD chip, filmstrip
/// dots and focus filter refresh on this signal (debounced frontend-side).
fn flush_progress(app: &tauri::AppHandle, store: &Store, batch: &mut Vec<String>) {
    if batch.is_empty() {
        return;
    }
    let folder_id = store.photo_folder_id(&batch[0]).ok().flatten();
    let _ = app.emit(
        "focus-progress",
        serde_json::json!({
            "folderId": folder_id,
            "photoIds": std::mem::take(batch),
        }),
    );
}

/// Background sweep: cursor-prioritized, strictly behind the preview and
/// metadata sweeps (it claims a photo only once both inputs exist), one row
/// per photo per session of that photo's life. Derived data: two instances
/// racing on the same DB write identical rows, so no cross-process claim
/// protocol is needed.
pub fn spawn_worker(app: tauri::AppHandle, store: Arc<Store>, preview: Arc<PreviewState>) {
    std::thread::Builder::new()
        .name("focus".into())
        .spawn(move || {
            let mut batch: Vec<String> = Vec::new();
            loop {
                let have = store.focus_ids().unwrap_or_default();
                let meta_have = store.metadata_ids().unwrap_or_default();
                let ordered = preview.entries_by_distance();
                let todo: Vec<_> = ordered
                    .into_iter()
                    .filter(|e| !have.contains(&e.id))
                    .filter(|e| meta_have.contains(&e.id))
                    .filter(|e| preview.preview_path_for_id(&e.id).exists())
                    .take(8)
                    .collect();
                if todo.is_empty() {
                    flush_progress(&app, &store, &mut batch);
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    continue;
                }
                for e in &todo {
                    if let Some(id) = process_photo(&store, &preview, &e.id) {
                        batch.push(id);
                    }
                    if batch.len() >= 4 {
                        flush_progress(&app, &store, &mut batch);
                    }
                }
            }
        })
        .expect("spawn focus worker");
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// High-frequency checkerboard: strong Laplacian response.
    fn sharp_img(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            let v = if (x + y) % 2 == 0 { 220 } else { 30 };
            Rgb([v, v, v])
        })
    }

    /// The same pattern through a 5×5 box blur — what defocus does.
    fn blurred(src: &RgbImage) -> RgbImage {
        let (w, h) = src.dimensions();
        RgbImage::from_fn(w, h, |x, y| {
            let mut sum = 0u32;
            let mut n = 0u32;
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let sx = x as i32 + dx;
                    let sy = y as i32 + dy;
                    if sx >= 0 && sy >= 0 && (sx as u32) < w && (sy as u32) < h {
                        sum += src.get_pixel(sx as u32, sy as u32).0[0] as u32;
                        n += 1;
                    }
                }
            }
            let v = (sum / n) as u8;
            Rgb([v, v, v])
        })
    }

    #[test]
    fn sharp_scores_far_above_blurred() {
        let sharp = sharp_img(128, 128);
        let soft = blurred(&sharp);
        let a = laplacian_variance(&sharp, 64, 64, 32);
        let b = laplacian_variance(&soft, 64, 64, 32);
        assert!(a > b * 4.0, "sharp {a} vs blurred {b}");
    }

    #[test]
    fn patch_clamps_at_edges_without_panicking() {
        let img = sharp_img(64, 48);
        // Corners, out-of-range center, oversized patch: all must stay in bounds.
        for (cx, cy, half) in [(0, 0, 16), (63, 47, 16), (200, 200, 16), (32, 24, 500)] {
            let v = laplacian_variance(&img, cx, cy, half);
            assert!(v.is_finite());
        }
        assert_eq!(laplacian_variance(&sharp_img(3, 3), 1, 1, 8), 0.0);
    }

    #[test]
    fn af_analysis_parses_point_and_skips() {
        let ok = r#"{"FujiFilm:FocusPixel":"3864 2576","File:ImageWidth":7728,
            "File:ImageHeight":5152,"IFD0:Orientation":1,
            "FujiFilm:FocusMode":"Auto","FujiFilm:AFAreaMode":"Single Point",
            "FujiFilm:AFAreaPointSize":3}"#;
        match af_analysis(ok) {
            AfInfo::Point { nx, ny, point_size } => {
                assert!((nx - 0.5).abs() < 1e-6 && (ny - 0.5).abs() < 1e-6);
                assert_eq!(point_size, Some(3));
            }
            other => panic!("expected point, got {other:?}"),
        }
        // Manual focus: FocusPixel is stale — skip.
        let manual = r#"{"FujiFilm:FocusPixel":"1 1","File:ImageWidth":100,
            "File:ImageHeight":100,"FujiFilm:FocusMode":"Manual"}"#;
        assert_eq!(af_analysis(manual), AfInfo::Skip("manual"));
        // Wide/Tracking: the point isn't a point — skip.
        let tracking = r#"{"FujiFilm:FocusPixel":"1 1","File:ImageWidth":100,
            "File:ImageHeight":100,"FujiFilm:AFAreaMode":"Wide/Tracking"}"#;
        assert_eq!(af_analysis(tracking), AfInfo::Skip("tracking"));
        // No FocusPixel at all (other camera, no MakerNotes).
        assert_eq!(af_analysis("{}"), AfInfo::Skip("no_af"));
    }

    /// Score a real preview at a display-normalized AF point:
    /// `EMBER_FOCUS_IMAGE=/path.jpg EMBER_FOCUS_POINT="0.65 0.67" \
    ///  cargo test -- --ignored reference_score --nocapture`
    /// Calibration anchor: DSCF2084 (missed) ≈ 89, DSCF2085 (sharp) ≈ 270.
    #[test]
    #[ignore]
    fn reference_score() {
        let Ok(path) = std::env::var("EMBER_FOCUS_IMAGE") else {
            eprintln!("set EMBER_FOCUS_IMAGE");
            return;
        };
        let point = std::env::var("EMBER_FOCUS_POINT").unwrap_or("0.5 0.5".into());
        let mut it = point.split_whitespace().map(|s| s.parse::<f64>().unwrap());
        let (nx, ny) = (it.next().unwrap(), it.next().unwrap());
        let img = image::open(&path).unwrap().to_rgb8();
        let (w, h) = img.dimensions();
        let half = patch_half(w, h, None);
        let cx = (nx * (w - 1) as f64).round() as u32;
        let cy = (ny * (h - 1) as f64).round() as u32;
        let score = laplacian_variance(&img, cx, cy, half);
        eprintln!(
            "{path}: {w}x{h} patch {}px @({cx},{cy}) score {score:.1}",
            2 * half
        );
    }

    #[test]
    fn patch_scales_with_af_point_size_and_clamps() {
        let base = patch_half(2600, 1733, Some(3));
        assert_eq!(base, 67, "calibrated size at 2600px");
        assert!(patch_half(2600, 1733, Some(1)) < base);
        assert!(patch_half(2600, 1733, Some(6)) > base);
        assert_eq!(patch_half(2600, 1733, None), base, "absent → default");
        assert_eq!(patch_half(2600, 1733, Some(99)), base, "junk → default");
        assert!(patch_half(2600, 1733, Some(6)) <= 130, "≤5% of long edge");
        assert_eq!(patch_half(200, 100, Some(1)), 24, "floor for tiny previews");
    }
}
