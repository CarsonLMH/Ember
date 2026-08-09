use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use xxhash_rust::xxh3::xxh3_64;

/// One logical photo: a JPEG + RAF pair (same directory, same stem), or a singleton.
#[derive(Debug)]
pub struct PhotoEntry {
    pub id: String,
    pub dir: PathBuf,
    pub stem: String,
    pub jpeg: Option<PathBuf>,
    pub raf: Option<PathBuf>,
    /// mtime (secs) and size of the JPEG (or RAF for RAF-only) — cache-key inputs.
    pub mtime: u64,
    pub size: u64,
}

impl PhotoEntry {
    /// The file used for display and metadata: the JPEG when present, else
    /// the RAF (whose embedded preview and MakerNotes serve the same roles).
    pub fn display_file(&self) -> Option<&PathBuf> {
        self.jpeg.as_ref().or(self.raf.as_ref())
    }

    pub fn to_dto(&self, root: &Path) -> PhotoDto {
        let rel_dir = self
            .dir
            .strip_prefix(root)
            .unwrap_or(&self.dir)
            .to_string_lossy()
            .into_owned();
        PhotoDto {
            id: self.id.clone(),
            stem: self.stem.clone(),
            rel_dir,
            has_jpeg: self.jpeg.is_some(),
            has_raf: self.raf.is_some(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoDto {
    pub id: String,
    pub stem: String,
    pub rel_dir: String,
    pub has_jpeg: bool,
    pub has_raf: bool,
}

fn ext_kind(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("jpeg"),
        "raf" => Some("raf"),
        _ => None,
    }
}

/// Recursively scan `root`, pairing JPEG+RAF by (directory, lowercased stem).
/// Returns entries sorted by (stem, dir) — filename order for instant display.
pub fn scan(root: &Path) -> Vec<Arc<PhotoEntry>> {
    let mut groups: HashMap<(PathBuf, String), (Option<PathBuf>, Option<PathBuf>)> = HashMap::new();
    // Symlinked dirs are followed, so the same photo can be reached via two
    // paths; keying and storing by canonical dir collapses the duplicates.
    let mut canon: HashMap<PathBuf, PathBuf> = HashMap::new();

    for entry in walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(kind) = ext_kind(path) else { continue };
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let raw_dir = path.parent().unwrap_or(root);
        let dir = canon
            .entry(raw_dir.to_path_buf())
            .or_insert_with(|| {
                std::fs::canonicalize(raw_dir).unwrap_or_else(|_| raw_dir.to_path_buf())
            })
            .clone();
        let file = dir.join(path.file_name().unwrap_or(path.as_os_str()));
        let slot = groups.entry((dir, stem.to_ascii_lowercase())).or_default();
        match kind {
            "jpeg" => slot.0 = Some(file),
            _ => slot.1 = Some(file),
        }
    }

    let mut photos: Vec<Arc<PhotoEntry>> = groups
        .into_iter()
        .filter_map(|((dir, _), (jpeg, raf))| {
            let primary = jpeg.as_ref().or(raf.as_ref())?;
            let stem = primary.file_stem()?.to_string_lossy().into_owned();
            let id = format!("{:016x}", xxh3_64(primary.to_string_lossy().as_bytes()));
            let meta = std::fs::metadata(primary).ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let size = meta.map(|m| m.len()).unwrap_or(0);
            Some(Arc::new(PhotoEntry {
                id,
                dir,
                stem,
                jpeg,
                raf,
                mtime,
                size,
            }))
        })
        .collect();

    photos.sort_by(|a, b| (&a.stem, &a.dir).cmp(&(&b.stem, &b.dir)));
    photos
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        File::create(path).unwrap();
    }

    #[test]
    fn pairs_jpeg_and_raf_case_insensitive() {
        let tmp = std::env::temp_dir().join(format!("a2scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        touch(&tmp.join("DSCF0001.JPG"));
        touch(&tmp.join("DSCF0001.RAF"));
        touch(&tmp.join("DSCF0002.jpg"));
        touch(&tmp.join("sub/DSCF0003.raf"));
        touch(&tmp.join("sub/DSCF0001.jpg")); // same stem, different dir → distinct photo
        touch(&tmp.join("notes.txt"));

        let photos = scan(&tmp);
        assert_eq!(photos.len(), 4);

        // Stored dirs are canonical (macOS tempdirs live behind /private).
        let canon_tmp = std::fs::canonicalize(&tmp).unwrap();
        let pair = photos
            .iter()
            .find(|p| p.stem == "DSCF0001" && p.dir == canon_tmp)
            .unwrap();
        assert!(pair.jpeg.is_some() && pair.raf.is_some());

        let raf_only = photos.iter().find(|p| p.stem == "DSCF0003").unwrap();
        assert!(raf_only.jpeg.is_none() && raf_only.raf.is_some());

        let ids: std::collections::HashSet<_> = photos.iter().map(|p| p.id.clone()).collect();
        assert_eq!(ids.len(), 4, "ids must be unique across subfolders");

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn symlinked_dirs_dedup() {
        let tmp = std::env::temp_dir().join(format!("a2link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        touch(&tmp.join("real/DSCF0009.JPG"));
        touch(&tmp.join("real/DSCF0009.RAF"));
        std::os::unix::fs::symlink(tmp.join("real"), tmp.join("alias")).unwrap();

        let photos = scan(&tmp);
        assert_eq!(
            photos.len(),
            1,
            "the same photo reached via a symlinked dir must not duplicate"
        );
        assert!(photos[0].jpeg.is_some() && photos[0].raf.is_some());

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
