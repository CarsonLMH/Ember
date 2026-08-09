use std::collections::HashSet;
use std::sync::Arc;

use crate::fastexif;
use crate::preview::PreviewState;
use crate::store::Store;
use crate::xmp::Exiftool;

/// Background exiftool sweep: full `-j -G1 -n` dumps (MakerNotes included),
/// cursor-prioritized, cached in the store keyed by photo id. One read serves
/// the AF point (slice 4), the metadata panel (slice 6), and recipes (slice 7).
pub fn spawn_worker(store: Arc<Store>, preview: Arc<PreviewState>, et: Arc<Exiftool>) {
    std::thread::Builder::new()
        .name("metadata".into())
        .spawn(move || loop {
            let have: HashSet<String> = store.metadata_ids().unwrap_or_default();
            let ordered = preview.entries_by_distance();
            let batch: Vec<_> = ordered
                .into_iter()
                .filter(|e| !have.contains(&e.id))
                .filter(|e| e.display_file().is_some())
                .take(8)
                .collect();
            if batch.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                continue;
            }
            // Readable values for the metadata panel + recipes ("Provia",
            // "+2"); Orientation stays numeric (the '#' suffix) for AF math.
            let mut args: Vec<String> = vec![
                "-j".into(),
                "-G1".into(),
                "-Orientation#".into(),
                "-FileSize#".into(),
                "-All".into(),
            ];
            for e in &batch {
                if let Some(p) = e.display_file() {
                    args.push(p.to_string_lossy().into_owned());
                }
            }
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = match et.run(&arg_refs) {
                Ok(o) => o,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    continue;
                }
            };
            let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap_or_default();
            for item in parsed {
                let Some(src) = item.get("SourceFile").and_then(|v| v.as_str()) else {
                    continue;
                };
                if let Some(e) = batch.iter().find(|e| {
                    e.display_file()
                        .map(|p| p.to_string_lossy() == src)
                        .unwrap_or(false)
                }) {
                    let _ = store.set_metadata_json(&e.id, &item.to_string());
                }
            }
            // Mark unparseable files too, so we don't grind on them forever.
            for e in &batch {
                if store.metadata_json(&e.id).ok().flatten().is_none() {
                    let _ = store.set_metadata_json(&e.id, "{}");
                }
            }
        })
        .expect("spawn metadata worker");
}

/// Fujifilm AF point in display-normalized coordinates (0..1), from the
/// MakerNotes FocusPixel (native sensor coords) mapped through orientation.
pub fn focus_point(json: &str) -> Option<(f64, f64)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    // -G1 group is the maker name ("FujiFilm:FocusPixel"); match by suffix so
    // group naming never breaks this again.
    let fp = v
        .as_object()?
        .iter()
        .find(|(k, _)| k.ends_with(":FocusPixel") || *k == "FocusPixel")
        .map(|(_, val)| val)?;
    let text = match fp {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut parts = text.split_whitespace();
    let x: f64 = parts.next()?.parse().ok()?;
    let y: f64 = parts.next()?.parse().ok()?;
    let w = dim(&v, &["File:ImageWidth", "File:Image Width"])?;
    let h = dim(&v, &["File:ImageHeight", "File:Image Height"])?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let orientation = v
        .get("IFD0:Orientation")
        .and_then(|o| o.as_u64())
        .unwrap_or(1) as u32;
    let (nx, ny) = ((x / w).clamp(0.0, 1.0), (y / h).clamp(0.0, 1.0));
    Some(fastexif::orient_point(nx, ny, orientation))
}

fn dim(v: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_f64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_point_parses_and_orients() {
        // Real exiftool -G1 key naming (verified against an X-T50 file).
        let json = r#"{"FujiFilm:FocusPixel":"3864 2576","File:ImageWidth":7728,"File:ImageHeight":5152,"IFD0:Orientation":1}"#;
        let (x, y) = focus_point(json).unwrap();
        assert!((x - 0.5).abs() < 1e-6 && (y - 0.5).abs() < 1e-6);

        // Portrait (orientation 6): top-center of sensor → right-center of display.
        let json = r#"{"FujiFilm:FocusPixel":"3864 515","File:ImageWidth":7728,"File:ImageHeight":5152,"IFD0:Orientation":6}"#;
        let (x, y) = focus_point(json).unwrap();
        assert!(x > 0.85 && (y - 0.5).abs() < 1e-6);

        assert!(focus_point("{}").is_none());
    }
}
