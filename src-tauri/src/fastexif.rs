use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use exif::{In, Tag, Value};
use rayon::prelude::*;

use crate::scanner::PhotoEntry;

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let file = File::open(path).ok()?;
    exif::Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()
}

fn ascii_field(exif: &exif::Exif, tag: Tag) -> Option<String> {
    match &exif.get_field(tag, In::PRIMARY)?.value {
        Value::Ascii(v) => v
            .first()
            .map(|b| String::from_utf8_lossy(b).trim().to_string()),
        _ => None,
    }
}

/// Capture time as epoch millis (naive local time — used for ordering only).
fn capture_time_ms(exif: &exif::Exif) -> Option<i64> {
    let dt = ascii_field(exif, Tag::DateTimeOriginal)?;
    let parsed = exif::DateTime::from_ascii(dt.as_bytes()).ok()?;
    let secs = days_from_civil(parsed.year as i64, parsed.month as u32, parsed.day as u32) * 86_400
        + parsed.hour as i64 * 3_600
        + parsed.minute as i64 * 60
        + parsed.second as i64;
    // SubSecTimeOriginal is a fractional-seconds string, e.g. "57" = .57s
    let sub_ms = ascii_field(exif, Tag::SubSecTimeOriginal)
        .map(|s| {
            let frac: String = s.chars().filter(|c| c.is_ascii_digit()).take(3).collect();
            let padded = format!("{frac:0<3}");
            padded.parse::<i64>().unwrap_or(0)
        })
        .unwrap_or(0);
    Some(secs * 1000 + sub_ms)
}

/// EXIF orientation (1–8), defaulting to 1.
pub fn orientation(path: &Path) -> u32 {
    read_exif(path)
        .and_then(|e| {
            e.get_field(Tag::Orientation, In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1)
}

/// Native pixel dimensions from the JPEG SOF marker (EXIF dimension tags lie
/// after crops; the SOF frame header never does).
fn jpeg_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut f = File::open(path).ok()?;
    let mut buf = vec![0u8; 1024 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    while i + 4 <= buf.len() {
        if buf[i] != 0xFF {
            return None;
        }
        let marker = buf[i + 1];
        // Standalone markers without length
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if i + 9 > buf.len() {
                return None;
            }
            let h = u16::from_be_bytes([buf[i + 5], buf[i + 6]]) as u32;
            let w = u16::from_be_bytes([buf[i + 7], buf[i + 8]]) as u32;
            return Some((w, h));
        }
        if marker == 0xDA {
            return None; // start of scan, no SOF found
        }
        i += 2 + len;
    }
    None
}

/// Map native (sensor) dimensions to display orientation.
pub fn oriented_dims(w: u32, h: u32, orientation: u32) -> (u32, u32) {
    match orientation {
        5..=8 => (h, w),
        _ => (w, h),
    }
}

/// Map a normalized point in native coordinates to display coordinates for
/// the given EXIF orientation (matching preview.rs::apply_orientation).
pub fn orient_point(nx: f64, ny: f64, orientation: u32) -> (f64, f64) {
    match orientation {
        2 => (1.0 - nx, ny),
        3 => (1.0 - nx, 1.0 - ny),
        4 => (nx, 1.0 - ny),
        5 => (ny, nx),
        6 => (1.0 - ny, nx),
        7 => (1.0 - ny, 1.0 - nx),
        8 => (ny, 1.0 - nx),
        _ => (nx, ny),
    }
}

pub struct FastMeta {
    pub ts: i64,
    /// Display-oriented pixel dimensions, when determinable.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Parallel fast pass: capture time (mtime fallback so sorting is total) and
/// display-oriented dimensions for every photo with a readable JPEG.
pub fn photo_meta(entries: &[Arc<PhotoEntry>]) -> HashMap<String, FastMeta> {
    entries
        .par_iter()
        .map(|e| {
            let exif = e.display_file().and_then(|p| read_exif(p));
            let ts = exif
                .as_ref()
                .and_then(capture_time_ms)
                .unwrap_or((e.mtime as i64) * 1000);
            let orientation = exif
                .as_ref()
                .and_then(|x| {
                    x.get_field(Tag::Orientation, In::PRIMARY)
                        .and_then(|f| f.value.get_uint(0))
                })
                .unwrap_or(1);
            let dims = e
                .display_file()
                .and_then(|p| jpeg_dimensions(p))
                .map(|(w, h)| oriented_dims(w, h, orientation));
            (
                e.id.clone(),
                FastMeta {
                    ts,
                    width: dims.map(|d| d.0),
                    height: dims.map(|d| d.1),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 8, 2), 20_667);
    }

    #[test]
    fn orientation_point_mapping() {
        // Portrait shot, camera rotated CW (orientation 6): a point near the
        // top of the sensor's long edge lands on the right of the display.
        let (dx, dy) = orient_point(0.5, 0.1, 6);
        assert!((dx - 0.9).abs() < 1e-9 && (dy - 0.5).abs() < 1e-9);
        let (dx, dy) = orient_point(0.5, 0.1, 8);
        assert!((dx - 0.1).abs() < 1e-9 && (dy - 0.5).abs() < 1e-9);
        assert_eq!(orient_point(0.25, 0.75, 1), (0.25, 0.75));
        assert_eq!(oriented_dims(7728, 5152, 6), (5152, 7728));
    }

    #[test]
    fn sof_dimensions_from_generated_jpeg() {
        let dir = std::env::temp_dir().join(format!("a2dims-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jpg");
        image::RgbImage::from_pixel(320, 200, image::Rgb([9, 9, 9]))
            .save(&p)
            .unwrap();
        assert_eq!(jpeg_dimensions(&p), Some((320, 200)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
