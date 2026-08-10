use std::path::Path;

use crate::exposure::BlinkiesCfg;

/// `[faces]` — thresholds re-read on relaunch. `enabled` is special: the app
/// itself also writes it (Delete-all, the panel toggle); the DB copy is the
/// cross-process authority and this file is the user-facing mirror that wins
/// at launch (a hand-edit expresses intent).
#[derive(Debug, Clone, Copy)]
pub struct FacesCfg {
    pub enabled: bool,
    pub auto_assign_threshold: f32,
    pub auto_assign_margin: f32,
    pub cluster_threshold: f32,
    pub min_det_score: f32,
}

impl Default for FacesCfg {
    fn default() -> Self {
        Self {
            // Calibrated on real X-T50 photos (2026-08-10, 145 pos/145 neg):
            // same-person min 0.504 / p5 0.537 / median 0.758; cross-person
            // max 0.322; strongest stranger 0.377. 0.45 sits 0.07+ over every
            // observed non-match and under every observed match; the margin is
            // generous because true matches led the runner-up by ≥0.18.
            enabled: true,
            auto_assign_threshold: 0.45,
            auto_assign_margin: 0.08,
            // 0.45 let junky detections (turned heads) bridge two people into
            // one cluster on real photos; 0.50 fragments instead — cheap,
            // since naming both fragments the same name merges them.
            cluster_threshold: 0.50,
            min_det_score: 0.8,
        }
    }
}

/// `[cache]` — preview/thumb/chip cache budget. 0 disables cleanup.
#[derive(Debug, Clone, Copy)]
pub struct CacheCfg {
    pub max_mb: u64,
}

impl Default for CacheCfg {
    fn default() -> Self {
        Self { max_mb: 2048 }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Settings {
    pub blinkies: BlinkiesCfg,
    pub faces: FacesCfg,
    pub cache: CacheCfg,
}

fn default_file_contents() -> String {
    "# Ember settings — edit and relaunch the app to apply.\n\n\
     [blinkies]\n\
     # Highlights flash when ANY channel is at or above this value (0-255).\n\
     highlight = 250\n\
     # Shadows flash when ALL channels are at or below this value (0-255).\n\
     shadow = 5\n\
     # Overlay colors as hex (changing these regenerates cached masks).\n\
     highlight_color = \"#FF4646\"\n\
     shadow_color = \"#5082FF\"\n\n\
     [faces]\n\
     # On-device face indexing (People panel). The app also writes this line\n\
     # when you toggle faces or use \"Delete all face data\".\n\
     enabled = true\n\
     # Detection confidence floor (0-1) and clustering similarity threshold.\n\
     min_det_score = 0.8\n\
     cluster_threshold = 0.5\n\
     # Auto-recognition thresholds (calibrated on real photos 2026-08).\n\
     auto_assign_threshold = 0.45\n\
     auto_assign_margin = 0.08\n\n\
     [cache]\n\
     # Preview/thumbnail cache budget in MB (~1MB per photo). Once per launch,\n\
     # photos from the least-recently-opened folders are pruned back under\n\
     # this. Everything regenerates on demand. 0 = never clean up.\n\
     max_mb = 2048\n"
        .to_string()
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Load settings.toml (created with commented defaults on first run).
pub fn load(config_dir: &Path) -> Settings {
    let path = config_dir.join("settings.toml");
    if !path.exists() {
        let _ = std::fs::write(&path, default_file_contents());
    }
    let parsed: Option<toml::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok());

    let blink = parsed.as_ref().and_then(|v| v.get("blinkies"));
    let get_u8 = |key: &str| -> Option<u8> {
        blink?
            .get(key)?
            .as_integer()
            .filter(|v| (0..=255).contains(v))
            .map(|v| v as u8)
    };
    let get_color =
        |key: &str| -> Option<[u8; 3]> { blink?.get(key)?.as_str().and_then(parse_hex) };
    let d = BlinkiesCfg::default();
    let blinkies = BlinkiesCfg {
        highlight: get_u8("highlight").unwrap_or(d.highlight),
        shadow: get_u8("shadow").unwrap_or(d.shadow),
        highlight_color: get_color("highlight_color").unwrap_or(d.highlight_color),
        shadow_color: get_color("shadow_color").unwrap_or(d.shadow_color),
    };

    let f = parsed.as_ref().and_then(|v| v.get("faces"));
    let get_f32 = |key: &str, lo: f32, hi: f32| -> Option<f32> {
        f?.get(key)?
            .as_float()
            .map(|v| v as f32)
            .filter(|v| (lo..=hi).contains(v))
    };
    let fd = FacesCfg::default();
    let faces = FacesCfg {
        enabled: f
            .and_then(|s| s.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(fd.enabled),
        auto_assign_threshold: get_f32("auto_assign_threshold", 0.0, 1.0)
            .unwrap_or(fd.auto_assign_threshold),
        auto_assign_margin: get_f32("auto_assign_margin", 0.0, 1.0)
            .unwrap_or(fd.auto_assign_margin),
        cluster_threshold: get_f32("cluster_threshold", 0.0, 1.0).unwrap_or(fd.cluster_threshold),
        min_det_score: get_f32("min_det_score", 0.0, 1.0).unwrap_or(fd.min_det_score),
    };

    let cache = CacheCfg {
        max_mb: parsed
            .as_ref()
            .and_then(|v| v.get("cache"))
            .and_then(|s| s.get("max_mb"))
            .and_then(|v| v.as_integer())
            .filter(|v| *v >= 0)
            .map(|v| v as u64)
            .unwrap_or(CacheCfg::default().max_mb),
    };
    Settings {
        blinkies,
        faces,
        cache,
    }
}

/// Mirror an app-driven enabled/disabled transition into settings.toml —
/// a targeted line edit so user comments survive. Appends the section or the
/// key when absent (e.g. a settings file that predates faces).
pub fn write_faces_enabled(config_dir: &Path, enabled: bool) -> std::io::Result<()> {
    let path = config_dir.join("settings.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let line = format!("enabled = {enabled}");
    let mut out: Vec<String> = Vec::new();
    let mut in_faces = false;
    let mut written = false;
    for l in text.lines() {
        let trimmed = l.trim();
        if trimmed.starts_with('[') {
            // Leaving [faces] without having seen the key → insert it.
            if in_faces && !written {
                out.push(line.clone());
                written = true;
            }
            in_faces = trimmed == "[faces]";
        } else if in_faces && trimmed.split('=').next().map(str::trim) == Some("enabled") {
            out.push(line.clone());
            written = true;
            continue;
        }
        out.push(l.to_string());
    }
    if !written {
        if !in_faces {
            if !out.is_empty() && !out.last().unwrap().is_empty() {
                out.push(String::new());
            }
            out.push("[faces]".into());
        }
        out.push(line);
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    std::fs::write(&path, joined)
}

/// Load the tag vocabulary from tags.toml (created with commented defaults).
/// Read on every palette open, so edits apply without a relaunch.
pub fn load_tag_vocab(config_dir: &Path) -> Vec<String> {
    let path = config_dir.join("tags.toml");
    if !path.exists() {
        let _ = std::fs::write(
            &path,
            "# Ember tag vocabulary — the tag palette offers these, in order.\n\
             # Free-form strings; they write to XMP dc:subject (see docs/METADATA.md).\n\n\
             tags = [\"portfolio\", \"album\", \"print\", \"share\", \"revisit\"]\n",
        );
    }
    let parsed: Option<toml::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok());
    parsed
        .and_then(|v| v.get("tags").and_then(|t| t.as_array().cloned()))
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_written_and_overridable() {
        let dir = std::env::temp_dir().join(format!("a2set-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = load(&dir).blinkies;
        assert_eq!((t.highlight, t.shadow), (250, 5));
        assert_eq!(t.highlight_color, [0xFF, 0x46, 0x46]);
        std::fs::write(
            dir.join("settings.toml"),
            "[blinkies]\nhighlight = 254\nshadow_color = \"#00FF00\"\n",
        )
        .unwrap();
        let t = load(&dir).blinkies;
        assert_eq!(
            (t.highlight, t.shadow),
            (254, 5),
            "partial override keeps defaults"
        );
        assert_eq!(t.shadow_color, [0, 255, 0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn faces_defaults_partial_override_and_range_guard() {
        let dir = std::env::temp_dir().join(format!("a2setf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = load(&dir).faces;
        assert!(f.enabled);
        assert!((f.auto_assign_threshold - 0.45).abs() < 1e-6);
        assert!((f.cluster_threshold - 0.50).abs() < 1e-6);
        // Partial override; out-of-range values fall back to defaults.
        std::fs::write(
            dir.join("settings.toml"),
            "[faces]\nenabled = false\ncluster_threshold = 0.6\nmin_det_score = 7.0\n",
        )
        .unwrap();
        let f = load(&dir).faces;
        assert!(!f.enabled);
        assert!((f.cluster_threshold - 0.6).abs() < 1e-6);
        assert!(
            (f.min_det_score - 0.8).abs() < 1e-6,
            "out of range → default"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_faces_enabled_patches_in_place_and_appends() {
        let dir = std::env::temp_dir().join(format!("a2setw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Existing file WITHOUT a [faces] section: section appended, comments kept.
        std::fs::write(
            dir.join("settings.toml"),
            "# my notes\n[blinkies]\nhighlight = 254\n",
        )
        .unwrap();
        write_faces_enabled(&dir, false).unwrap();
        let s = load(&dir);
        assert!(!s.faces.enabled);
        assert_eq!(s.blinkies.highlight, 254, "other sections untouched");
        let text = std::fs::read_to_string(dir.join("settings.toml")).unwrap();
        assert!(text.contains("# my notes"), "comments survive");

        // Patch back in place (no duplicate keys/sections).
        write_faces_enabled(&dir, true).unwrap();
        assert!(load(&dir).faces.enabled);
        let text = std::fs::read_to_string(dir.join("settings.toml")).unwrap();
        assert_eq!(text.matches("[faces]").count(), 1);
        assert_eq!(text.matches("enabled").count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
