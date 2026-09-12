use std::path::Path;

use serde::Serialize;

/// (action, default keys, human label). The keymap.toml file overrides keys
/// per action; unknown actions in the file are ignored, missing ones fall
/// back to these defaults.
const DEFAULTS: &[(&str, &[&str], &str)] = &[
    ("next", &["ArrowRight", "ArrowDown"], "Next photo"),
    ("prev", &["ArrowLeft", "ArrowUp"], "Previous photo"),
    ("home", &["Home"], "Jump to first photo"),
    ("end", &["End"], "Jump to last photo"),
    ("rate1", &["1"], "Rate ★"),
    ("rate2", &["2"], "Rate ★★"),
    ("rate3", &["3"], "Rate ★★★"),
    ("rate4", &["4"], "Rate ★★★★"),
    ("rate5", &["5"], "Rate ★★★★★"),
    ("rate0", &["0"], "Clear stars"),
    ("trash", &["x", "Backspace", "Delete"], "Trash photo pair"),
    ("undo", &["Cmd+z"], "Undo"),
    ("redo", &["Cmd+Shift+z"], "Redo"),
    (
        "filter_cycle",
        &["u"],
        "Cycle filter: all → unstarred → starred",
    ),
    ("filter_all", &["Shift+0"], "Filter: show all"),
    ("filter_star1", &["Shift+1"], "Filter: exactly ★"),
    ("filter_star2", &["Shift+2"], "Filter: exactly ★★"),
    ("filter_star3", &["Shift+3"], "Filter: exactly ★★★"),
    ("filter_star4", &["Shift+4"], "Filter: exactly ★★★★"),
    ("filter_star5", &["Shift+5"], "Filter: exactly ★★★★★"),
    ("filter_min1", &["Ctrl+Shift+1"], "Filter: ★ or more"),
    ("filter_min2", &["Ctrl+Shift+2"], "Filter: ★★ or more"),
    ("filter_min3", &["Ctrl+Shift+3"], "Filter: ★★★ or more"),
    ("filter_min4", &["Ctrl+Shift+4"], "Filter: ★★★★ or more"),
    ("filter_min5", &["Ctrl+Shift+5"], "Filter: ★★★★★ or more"),
    ("recipe_filter", &["c"], "Recipe filter quick-switcher"),
    ("tag_palette", &["g"], "Tag palette"),
    ("tag_filter", &["Shift+g"], "Tag filter quick-switcher"),
    ("people_panel", &["p"], "Toggle People panel"),
    (
        "person_filter",
        &["Shift+p"],
        "Person filter quick-switcher",
    ),
    ("focus_filter", &["Shift+a"], "Filter: soft at the AF point"),
    (
        "face_badges",
        &["Shift+f"],
        "Toggle face badges on the photo",
    ),
    (
        "sort_cycle",
        &["s"],
        "Cycle sort: date captured → filename → rating",
    ),
    ("sort_reverse", &["Shift+s"], "Reverse sort order"),
    ("zoom_100", &["z"], "Toggle fit ↔ 100%"),
    ("focus_zoom", &["f"], "100% at the AF point"),
    ("af_overlay", &["a"], "Toggle AF point overlay"),
    ("exif_panel", &["i"], "Toggle metadata panel"),
    (
        "histogram",
        &["h"],
        "Cycle histogram: off → luminance → RGB",
    ),
    ("blinkies", &["b"], "Toggle clipping warnings"),
    ("auto_advance", &["v"], "Toggle auto-advance"),
    ("filmstrip", &["t"], "Toggle filmstrip"),
    ("immersion", &["Shift+t"], "Toggle picture-only mode"),
    ("refresh", &["r"], "Rescan folder"),
    ("cheat_sheet", &["?"], "Show this cheat sheet"),
    ("perf_hud", &["`"], "Toggle performance HUD"),
    ("open_folder", &["Cmd+o"], "Open folder"),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyBinding {
    pub action: String,
    pub keys: Vec<String>,
    pub label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeymapResult {
    pub bindings: Vec<KeyBinding>,
    /// Set when keymap.toml exists but couldn't be used — the user's edits
    /// were ignored and defaults are active. Silence here would mean a
    /// hand-edit vanishing without a trace.
    pub warning: Option<String>,
}

fn default_file_contents() -> String {
    let mut s = String::from(
        "# Ember keybindings — edit and relaunch the app to apply.\n\
         # Each action takes a list of keys. Modifiers: Cmd+, Ctrl+, Alt+, Shift+ \n\
         # (in that order), e.g. \"Cmd+Shift+z\". Named keys: ArrowRight, Backspace,\n\
         # Delete, Home, End. Deleting a line restores that action's default.\n\n[keys]\n",
    );
    for (action, keys, label) in DEFAULTS {
        let list = keys
            .iter()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!("# {label}\n{action} = [{list}]\n"));
    }
    s
}

/// Load the keymap, creating the editable default file on first run.
pub fn load(config_dir: &Path) -> KeymapResult {
    let path = config_dir.join("keymap.toml");
    if !path.exists() {
        let _ = std::fs::write(&path, default_file_contents());
    }
    let mut warning = None;
    let overrides: Option<toml::Value> = match std::fs::read_to_string(&path) {
        Ok(s) => match toml::from_str(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                // First line of the toml error carries the line/column.
                let detail = e.to_string().lines().next().unwrap_or_default().to_string();
                warning = Some(format!(
                    "keymap.toml has a syntax error — using default keybindings. {detail}"
                ));
                None
            }
        },
        Err(e) => {
            warning = Some(format!(
                "keymap.toml could not be read — using default keybindings. ({e})"
            ));
            None
        }
    };
    let keys_table = overrides
        .as_ref()
        .and_then(|v| v.get("keys"))
        .and_then(|v| v.as_table())
        .cloned();

    let bindings = DEFAULTS
        .iter()
        .map(|(action, defaults, label)| {
            let keys = keys_table
                .as_ref()
                .and_then(|t| t.get(*action))
                .and_then(|v| match v {
                    toml::Value::String(s) => Some(vec![s.clone()]),
                    toml::Value::Array(a) => Some(
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect(),
                    ),
                    _ => None,
                })
                .filter(|v: &Vec<String>| !v.is_empty())
                .unwrap_or_else(|| defaults.iter().map(|s| s.to_string()).collect());
            KeyBinding {
                action: action.to_string(),
                keys,
                label: label.to_string(),
            }
        })
        .collect();
    KeymapResult { bindings, warning }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_file_and_overrides_apply() {
        let dir = std::env::temp_dir().join(format!("emberkeys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let map = load(&dir);
        assert!(dir.join("keymap.toml").exists(), "default file written");
        assert!(map.warning.is_none());
        let trash = map.bindings.iter().find(|b| b.action == "trash").unwrap();
        assert_eq!(trash.keys, vec!["x", "Backspace", "Delete"]);

        std::fs::write(dir.join("keymap.toml"), "[keys]\ntrash = [\"j\"]\n").unwrap();
        let map = load(&dir);
        assert!(map.warning.is_none());
        let trash = map.bindings.iter().find(|b| b.action == "trash").unwrap();
        assert_eq!(trash.keys, vec!["j"], "file overrides default");
        let next = map.bindings.iter().find(|b| b.action == "next").unwrap();
        assert_eq!(next.keys[0], "ArrowRight", "missing actions keep defaults");
        let minimum = map
            .bindings
            .iter()
            .find(|b| b.action == "filter_min3")
            .unwrap();
        assert_eq!(minimum.keys, vec!["Ctrl+Shift+3"]);
        let minimum_five = map
            .bindings
            .iter()
            .find(|b| b.action == "filter_min5")
            .unwrap();
        assert!(minimum_five.label.ends_with("or more"));
        let sort = map
            .bindings
            .iter()
            .find(|b| b.action == "sort_cycle")
            .unwrap();
        assert!(sort.label.contains("date captured"));
        let immersion = map
            .bindings
            .iter()
            .find(|b| b.action == "immersion")
            .unwrap();
        assert_eq!(immersion.keys, vec!["Shift+t"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn malformed_file_warns_and_falls_back() {
        let dir = std::env::temp_dir().join(format!("emberkeys-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Unclosed table header: a realistic hand-edit typo.
        std::fs::write(dir.join("keymap.toml"), "[keys\ntrash = \"j\"\n").unwrap();

        let map = load(&dir);
        let warning = map.warning.expect("a broken file must produce a warning");
        assert!(warning.contains("syntax error"), "warning: {warning}");
        let trash = map.bindings.iter().find(|b| b.action == "trash").unwrap();
        assert_eq!(
            trash.keys,
            vec!["x", "Backspace", "Delete"],
            "defaults active, not a half-applied file"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
