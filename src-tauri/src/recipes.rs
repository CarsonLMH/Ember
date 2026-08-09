use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

// Fuzzy film-recipe matching. A recipe is a fingerprint over MakerNotes
// parameters; each parameter accepts an exact value, a set, a numeric range,
// or (by omission) anything. A photo matches when every specified parameter
// is satisfied. First match in file order wins the label.
//
// Values are exiftool's READABLE strings ("Classic Negative", "-2 (soft)");
// numeric comparisons parse the leading signed number of the string.

/// param name in recipes.toml → metadata JSON key suffixes (first present wins).
const PARAM_KEYS: &[(&str, &[&str])] = &[
    ("film_simulation", &["FujiFilm:FilmMode"]),
    ("wb", &["FujiFilm:WhiteBalance"]),
    ("wb_shift", &["FujiFilm:WhiteBalanceFineTune"]),
    (
        "dynamic_range",
        &[
            "FujiFilm:DynamicRangeSetting",
            "FujiFilm:DevelopmentDynamicRange",
        ],
    ),
    ("highlight_tone", &["FujiFilm:HighlightTone"]),
    ("shadow_tone", &["FujiFilm:ShadowTone"]),
    ("color", &["FujiFilm:Saturation"]),
    ("sharpness", &["FujiFilm:Sharpness"]),
    ("noise_reduction", &["FujiFilm:NoiseReduction"]),
    ("clarity", &["FujiFilm:Clarity"]),
    ("grain", &["FujiFilm:GrainEffectRoughness"]),
    ("grain_size", &["FujiFilm:GrainEffectSize"]),
    ("color_chrome", &["FujiFilm:ColorChromeEffect"]),
    ("color_chrome_blue", &["FujiFilm:ColorChromeFXBlue"]),
];

#[derive(Debug, Clone)]
enum Constraint {
    Str(String),
    Num(f64),
    Set(Vec<Constraint>),
    Range { min: f64, max: f64 },
}

#[derive(Debug, Clone)]
pub struct Recipe {
    pub name: String,
    params: BTreeMap<String, Constraint>,
}

fn leading_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let end = t
        .char_indices()
        .take_while(|(i, c)| {
            c.is_ascii_digit() || *c == '.' || (*i == 0 && (*c == '+' || *c == '-'))
        })
        .map(|(i, c)| i + c.len_utf8())
        .last()?;
    t[..end].parse().ok()
}

impl Constraint {
    fn from_toml(v: &toml::Value) -> Option<Constraint> {
        match v {
            toml::Value::String(s) => Some(Constraint::Str(s.clone())),
            toml::Value::Integer(n) => Some(Constraint::Num(*n as f64)),
            toml::Value::Float(n) => Some(Constraint::Num(*n)),
            toml::Value::Array(items) => Some(Constraint::Set(
                items.iter().filter_map(Constraint::from_toml).collect(),
            )),
            toml::Value::Table(t) => {
                let min = t
                    .get("min")
                    .and_then(|x| x.as_float().or(x.as_integer().map(|i| i as f64)))?;
                let max = t
                    .get("max")
                    .and_then(|x| x.as_float().or(x.as_integer().map(|i| i as f64)))?;
                Some(Constraint::Range { min, max })
            }
            _ => None,
        }
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Constraint::Str(s) => s.trim().eq_ignore_ascii_case(value.trim()),
            Constraint::Num(n) => leading_number(value)
                .map(|v| (v - n).abs() < 1e-9)
                .unwrap_or(false),
            Constraint::Set(items) => items.iter().any(|c| c.matches(value)),
            Constraint::Range { min, max } => leading_number(value)
                .map(|v| v >= *min && v <= *max)
                .unwrap_or(false),
        }
    }
}

impl Recipe {
    pub fn matches(&self, meta: &serde_json::Value) -> bool {
        self.params.iter().all(|(param, constraint)| {
            match photo_value(meta, param) {
                Some(value) => constraint.matches(&value),
                None => false, // recipe pins a param the photo doesn't carry
            }
        })
    }
}

pub fn photo_value(meta: &serde_json::Value, param: &str) -> Option<String> {
    let keys = PARAM_KEYS.iter().find(|(p, _)| *p == param)?.1;
    let obj = meta.as_object()?;
    for key in keys {
        if let Some(v) = obj.get(*key) {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

fn default_file_contents() -> String {
    r#"# Ember film recipes — matched top to bottom; first match wins.
# Values come from the metadata panel (Film / Recipe section) verbatim.
#
# Each parameter accepts:
#   exact string:   film_simulation = "Classic Negative"
#   exact number:   shadow_tone = -1        (matches "-1 (medium soft)")
#   a set:          grain = ["Weak", "Strong"]
#   a range:        color = { min = 2, max = 4 }
#   omitted         = any value matches
#
# Parameters: film_simulation, wb, wb_shift, dynamic_range, highlight_tone,
#   shadow_tone, color, sharpness, noise_reduction, clarity, grain,
#   grain_size, color_chrome, color_chrome_blue
#
# Example:
# [[recipe]]
# name = "Classic Neg Street"
# film_simulation = "Classic Negative"
# wb_shift = "Red +20, Blue -80"
# color = { min = 2, max = 4 }
# grain = ["Off", "Weak"]
"#
    .to_string()
}

/// recipes.toml loader with mtime-based hot reload: hand-edits apply on the
/// next lookup, no relaunch needed.
pub struct RecipeStore {
    path: PathBuf,
    cache: Mutex<(Option<std::time::SystemTime>, Vec<Recipe>)>,
}

impl RecipeStore {
    pub fn new(config_dir: &Path) -> Self {
        let path = config_dir.join("recipes.toml");
        if !path.exists() {
            let _ = std::fs::write(&path, default_file_contents());
        }
        Self {
            path,
            cache: Mutex::new((None, Vec::new())),
        }
    }

    pub fn recipes(&self) -> Vec<Recipe> {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        let mut cache = self.cache.lock().unwrap();
        if cache.0 != mtime || mtime.is_none() {
            cache.1 = std::fs::read_to_string(&self.path)
                .ok()
                .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
                .map(parse_recipes)
                .unwrap_or_default();
            cache.0 = mtime;
        }
        cache.1.clone()
    }

    /// Match a photo's metadata: (winner, all matching names).
    pub fn match_meta(&self, meta: &serde_json::Value) -> (Option<String>, Vec<String>) {
        let matches: Vec<String> = self
            .recipes()
            .iter()
            .filter(|r| r.matches(meta))
            .map(|r| r.name.clone())
            .collect();
        (matches.first().cloned(), matches)
    }

    pub fn names(&self) -> Vec<String> {
        self.recipes().into_iter().map(|r| r.name).collect()
    }

    /// "Save these settings as a new recipe": append a [[recipe]] block with
    /// the photo's exact current values (user widens to ranges by hand later).
    pub fn save_from_meta(&self, name: &str, meta: &serde_json::Value) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("recipe name is empty".into());
        }
        if self
            .names()
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name.trim()))
        {
            return Err(format!("recipe \"{}\" already exists", name.trim()));
        }
        let mut block = format!("\n[[recipe]]\nname = {:?}\n", name.trim());
        for (param, _) in PARAM_KEYS {
            if let Some(value) = photo_value(meta, param) {
                // Tone/color values write as bare numbers so ranges are easy
                // to widen; identity-ish strings stay verbatim.
                if let Some(n) = leading_number(&value) {
                    let numeric_params = [
                        "highlight_tone",
                        "shadow_tone",
                        "color",
                        "sharpness",
                        "noise_reduction",
                        "clarity",
                    ];
                    if numeric_params.contains(param) {
                        if n.fract() == 0.0 {
                            block.push_str(&format!("{param} = {}\n", n as i64));
                        } else {
                            block.push_str(&format!("{param} = {n}\n"));
                        }
                        continue;
                    }
                }
                block.push_str(&format!("{param} = {:?}\n", value));
            }
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|e| e.to_string())?;
        f.write_all(block.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn parse_recipes(root: toml::Value) -> Vec<Recipe> {
    let Some(list) = root.get("recipe").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let table = item.as_table()?;
            let name = table.get("name")?.as_str()?.to_string();
            let mut params = BTreeMap::new();
            for (key, value) in table {
                if key == "name" {
                    continue;
                }
                if PARAM_KEYS.iter().any(|(p, _)| p == key) {
                    if let Some(c) = Constraint::from_toml(value) {
                        params.insert(key.clone(), c);
                    }
                }
            }
            Some(Recipe { name, params })
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeResult {
    pub name: Option<String>,
    pub matches: Vec<String>,
    pub has_meta: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> serde_json::Value {
        serde_json::json!({
            "FujiFilm:FilmMode": "Classic Negative",
            "FujiFilm:WhiteBalanceFineTune": "Red +20, Blue -80",
            "FujiFilm:Saturation": "+3 (very high)",
            "FujiFilm:HighlightTone": "-2 (soft)",
            "FujiFilm:GrainEffectRoughness": "Strong",
        })
    }

    fn store_with(contents: &str) -> RecipeStore {
        let dir = std::env::temp_dir().join(format!(
            "a2rec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let s = RecipeStore::new(&dir);
        std::fs::write(&s.path, contents).unwrap();
        s
    }

    #[test]
    fn exact_set_range_and_first_wins() {
        let s = store_with(
            r#"
[[recipe]]
name = "CN wide"
film_simulation = "Classic Negative"
color = { min = 2, max = 4 }
grain = ["Weak", "Strong"]

[[recipe]]
name = "CN exact"
film_simulation = "Classic Negative"
wb_shift = "Red +20, Blue -80"
"#,
        );
        let (winner, all) = s.match_meta(&meta());
        assert_eq!(winner.as_deref(), Some("CN wide"), "first match wins");
        assert_eq!(all, vec!["CN wide", "CN exact"], "all matches listed");
    }

    #[test]
    fn numeric_range_uses_leading_number_and_mismatch_fails() {
        let s = store_with(
            r#"
[[recipe]]
name = "tight"
highlight_tone = { min = -1, max = 0 }
"#,
        );
        let (winner, _) = s.match_meta(&meta()); // -2 outside [-1, 0]
        assert!(winner.is_none(), "out-of-range param must not match");

        let s = store_with(
            r#"
[[recipe]]
name = "loose"
highlight_tone = { min = -2, max = 0 }
color = 3
"#,
        );
        let (winner, _) = s.match_meta(&meta());
        assert_eq!(winner.as_deref(), Some("loose"));
    }

    #[test]
    fn pinned_param_missing_from_photo_fails() {
        let s = store_with(
            r#"
[[recipe]]
name = "needs clarity"
clarity = 0
"#,
        );
        let (winner, _) = s.match_meta(&meta());
        assert!(winner.is_none());
    }

    #[test]
    fn save_from_meta_roundtrips_and_matches() {
        let s = store_with("");
        s.save_from_meta("My CN", &meta()).unwrap();
        let (winner, _) = s.match_meta(&meta());
        assert_eq!(winner.as_deref(), Some("My CN"));
        assert!(
            s.save_from_meta("my cn", &meta()).is_err(),
            "duplicate names rejected case-insensitively"
        );
        // Numeric params saved as bare numbers (easy to widen into ranges).
        let text = std::fs::read_to_string(&s.path).unwrap();
        assert!(text.contains("highlight_tone = -2"), "got: {text}");
        assert!(text.contains("film_simulation = \"Classic Negative\""));
    }
}
