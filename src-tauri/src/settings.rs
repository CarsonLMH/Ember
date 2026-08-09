use std::path::Path;

use crate::exposure::BlinkiesCfg;

fn default_file_contents() -> String {
    "# Ember settings — edit and relaunch the app to apply.\n\n\
     [blinkies]\n\
     # Highlights flash when ANY channel is at or above this value (0-255).\n\
     highlight = 250\n\
     # Shadows flash when ALL channels are at or below this value (0-255).\n\
     shadow = 5\n\
     # Overlay colors as hex (changing these regenerates cached masks).\n\
     highlight_color = \"#FF4646\"\n\
     shadow_color = \"#5082FF\"\n"
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
pub fn load(config_dir: &Path) -> BlinkiesCfg {
    let path = config_dir.join("settings.toml");
    if !path.exists() {
        let _ = std::fs::write(&path, default_file_contents());
    }
    let parsed: Option<toml::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok());
    let section = parsed.as_ref().and_then(|v| v.get("blinkies"));
    let get_u8 = |key: &str| -> Option<u8> {
        section?
            .get(key)?
            .as_integer()
            .filter(|v| (0..=255).contains(v))
            .map(|v| v as u8)
    };
    let get_color =
        |key: &str| -> Option<[u8; 3]> { section?.get(key)?.as_str().and_then(parse_hex) };
    let d = BlinkiesCfg::default();
    BlinkiesCfg {
        highlight: get_u8("highlight").unwrap_or(d.highlight),
        shadow: get_u8("shadow").unwrap_or(d.shadow),
        highlight_color: get_color("highlight_color").unwrap_or(d.highlight_color),
        shadow_color: get_color("shadow_color").unwrap_or(d.shadow_color),
    }
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
        let t = load(&dir);
        assert_eq!((t.highlight, t.shadow), (250, 5));
        assert_eq!(t.highlight_color, [0xFF, 0x46, 0x46]);
        std::fs::write(
            dir.join("settings.toml"),
            "[blinkies]\nhighlight = 254\nshadow_color = \"#00FF00\"\n",
        )
        .unwrap();
        let t = load(&dir);
        assert_eq!(
            (t.highlight, t.shadow),
            (254, 5),
            "partial override keeps defaults"
        );
        assert_eq!(t.shadow_color, [0, 255, 0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
