//! Cache janitor — keeps ~/Library/Caches/com.cleung.ember under a budget.
//!
//! The cache accretes ~1MB per photo (preview + thumb + hist + mask + chips)
//! for every folder ever opened and, before this, nothing ever cleaned it.
//! Policy: once per launch, well after startup, evict whole per-photo artifact
//! groups — orphans first (ids no longer in the DB), then oldest-opened
//! folders first (folders.updated_at) — until the cache fits `[cache] max_mb`.
//! The folder open in THIS session is never touched: everything here
//! regenerates on demand (the dir is documented safe to delete), but evicting
//! the active folder would cost the user regeneration time for no gain.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use crate::preview::PreviewState;
use crate::store::Store;

/// Late enough that cold open, the preview sweep and any gate storm are done.
const STARTUP_DELAY_SECS: u64 = 120;

/// One photo's cache artifacts: {id}.jpg, {id}-t.jpg, {id}-h.json,
/// {id}-m.png, {id}-raf.jpg, {id}-f{n}r{rev}.jpg — grouped by the 16-hex id
/// prefix (chip names carry the scan revision; grouping ignores the suffix).
struct Group {
    id: String,
    files: Vec<PathBuf>,
    bytes: u64,
}

fn collect_groups(cache_dir: &Path) -> Vec<Group> {
    let mut by_id: HashMap<String, Group> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    for e in entries.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        // Ids are exactly 16 hex chars, optionally followed by a suffix
        // beginning with '-' or '.'; anything else (blinkies.fingerprint,
        // in-flight .tmp files) is not ours to touch.
        if name.len() < 16 {
            continue;
        }
        let (id, rest) = name.split_at(16);
        if !id.chars().all(|c| c.is_ascii_hexdigit())
            || !(rest.starts_with('-') || rest.starts_with('.'))
        {
            continue;
        }
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        let g = by_id.entry(id.to_string()).or_insert_with(|| Group {
            id: id.to_string(),
            files: Vec::new(),
            bytes: 0,
        });
        g.files.push(e.path());
        g.bytes += size;
    }
    by_id.into_values().collect()
}

/// Evict until the cache fits the budget. Returns (evicted, kept) bytes.
///
/// `active_folder` is the folder this session actually has open, passed in
/// rather than inferred: `protected` holds only the photos currently LISTED,
/// which excludes the open folder's trashed ones — and inferring the folder
/// from that set fails exactly when it matters, in a folder where every photo
/// has been trashed. A trashed photo's files have left the folder, so nothing
/// can regenerate its preview, and the Trash panel serves it straight from
/// this cache.
pub fn run_once(
    cache_dir: &Path,
    photo_folders: &HashMap<String, i64>,
    folder_recency: &HashMap<i64, i64>,
    protected: &HashSet<String>,
    active_folder: Option<i64>,
    max_bytes: u64,
) -> (u64, u64) {
    let mut groups = collect_groups(cache_dir);
    let mut total: u64 = groups.iter().map(|g| g.bytes).sum();
    if total <= max_bytes {
        return (0, total);
    }
    // Orphans (no DB row → recency 0) evict first, then oldest folders.
    groups.sort_by_key(|g| {
        photo_folders
            .get(&g.id)
            .and_then(|fid| folder_recency.get(fid))
            .copied()
            .unwrap_or(0)
    });
    let mut evicted: u64 = 0;
    for g in &groups {
        if total <= max_bytes {
            break;
        }
        let in_active_folder = match (active_folder, photo_folders.get(&g.id)) {
            (Some(active), Some(fid)) => *fid == active,
            _ => false,
        };
        if protected.contains(&g.id) || in_active_folder {
            continue;
        }
        for f in &g.files {
            let _ = std::fs::remove_file(f);
        }
        total -= g.bytes;
        evicted += g.bytes;
    }
    (evicted, total)
}

/// One pass per launch, delayed off the startup path. `max_mb == 0` disables.
/// `active_folder` is written by `scan_folder`; -1 means no folder is open.
pub fn spawn(
    store: Arc<Store>,
    preview: Arc<PreviewState>,
    active_folder: Arc<AtomicI64>,
    max_mb: u64,
) {
    if max_mb == 0 {
        return;
    }
    std::thread::Builder::new()
        .name("cache-janitor".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(STARTUP_DELAY_SECS));
            let photo_folders = store.cache_photo_folders().unwrap_or_default();
            let folder_recency = store.folder_recency().unwrap_or_default();
            let protected = preview.current_ids();
            let active = match active_folder.load(Ordering::Relaxed) {
                id if id >= 0 => Some(id),
                _ => None,
            };
            let (evicted, kept) = run_once(
                preview.cache_dir(),
                &photo_folders,
                &folder_recency,
                &protected,
                active,
                max_mb * 1024 * 1024,
            );
            if evicted > 0 {
                eprintln!(
                    "cache janitor: evicted {}MB (cache now {}MB, budget {max_mb}MB)",
                    evicted / (1024 * 1024),
                    kept / (1024 * 1024),
                );
            }
        })
        .expect("spawn cache janitor");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, kb: usize) {
        std::fs::write(dir.join(name), vec![0u8; kb * 1024]).unwrap();
    }

    #[test]
    fn evicts_orphans_then_oldest_folder_never_protected() {
        let dir = std::env::temp_dir().join(format!(
            "emberjan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Four photo groups (~100KB each) + a non-photo file we must not touch.
        // aaaa… = orphan; bbbb… = old folder 1; cccc…/dddd… = recent folder 2.
        let a = "aaaaaaaaaaaaaaaa";
        let b = "bbbbbbbbbbbbbbbb";
        let c = "cccccccccccccccc";
        let d = "dddddddddddddddd";
        for id in [a, b, c, d] {
            write(&dir, &format!("{id}.jpg"), 90);
            write(&dir, &format!("{id}-t.jpg"), 8);
            write(&dir, &format!("{id}-f0.jpg"), 2);
        }
        write(&dir, "blinkies.fingerprint", 1);

        let photo_folders: HashMap<String, i64> =
            [(b.into(), 1), (c.into(), 2), (d.into(), 2)].into();
        let folder_recency: HashMap<i64, i64> = [(1, 100), (2, 200)].into();
        // Budget of 250KB forces evicting two groups (orphan + folder 1);
        // folder 2 (most recent) survives even though c is unprotected.
        let protected: HashSet<String> = [d.to_string()].into();
        let (evicted, kept) = run_once(
            &dir,
            &photo_folders,
            &folder_recency,
            &protected,
            Some(2),
            250 * 1024,
        );
        assert!(evicted > 0);
        assert!(kept <= 250 * 1024);
        assert!(
            !dir.join(format!("{a}.jpg")).exists(),
            "orphan evicted first"
        );
        assert!(!dir.join(format!("{b}.jpg")).exists(), "oldest folder next");
        assert!(
            !dir.join(format!("{b}-f0.jpg")).exists(),
            "whole group goes"
        );
        assert!(dir.join(format!("{c}.jpg")).exists(), "recent folder kept");
        assert!(dir.join(format!("{d}.jpg")).exists(), "protected kept");
        assert!(
            dir.join("blinkies.fingerprint").exists(),
            "non-photo untouched"
        );

        // Under budget → no-op.
        let (evicted, _) = run_once(
            &dir,
            &photo_folders,
            &folder_recency,
            &protected,
            Some(2),
            10 * 1024 * 1024,
        );
        assert_eq!(evicted, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A trashed photo of the OPEN folder is not in the live id set (its files
    /// left the folder), but the Trash panel serves its cached preview and
    /// nothing can regenerate one. Evicting it would break restore-me
    /// thumbnails mid-session, so the whole active folder is protected —
    /// **including when every photo in it has been trashed**, which is exactly
    /// the case where the folder could not be inferred from the live ids.
    #[test]
    fn the_open_folders_trashed_photos_are_never_evicted() {
        let dir = std::env::temp_dir().join(format!("emberjant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let open = "1111111111111111"; // listed in the open folder
        let trashed = "2222222222222222"; // same folder, trashed → not listed
        let elsewhere = "3333333333333333"; // a more recently opened folder
        for id in [open, trashed, elsewhere] {
            write(&dir, &format!("{id}.jpg"), 90);
        }
        let photo_folders: HashMap<String, i64> =
            [(open.into(), 7), (trashed.into(), 7), (elsewhere.into(), 8)].into();
        // Folder 8 is the more recent one, so ordinary recency would evict
        // the open folder's trashed photo first.
        let folder_recency: HashMap<i64, i64> = [(7, 100), (8, 200)].into();
        let protected: HashSet<String> = [open.to_string()].into();
        let (evicted, _) = run_once(
            &dir,
            &photo_folders,
            &folder_recency,
            &protected,
            Some(7),
            100 * 1024,
        );
        assert!(evicted > 0, "the budget forced an eviction");
        assert!(dir.join(format!("{open}.jpg")).exists());
        assert!(
            dir.join(format!("{trashed}.jpg")).exists(),
            "the trash panel still needs this preview"
        );
        assert!(
            !dir.join(format!("{elsewhere}.jpg")).exists(),
            "another folder's photo is the one that goes"
        );

        // The case the live-id inference could not see: the open folder now
        // has NO listed photo (everything in it is trashed), so `protected` is
        // empty and only the passed-in folder id can save its previews.
        write(&dir, &format!("{elsewhere}.jpg"), 90);
        let (evicted, _) = run_once(
            &dir,
            &photo_folders,
            &folder_recency,
            &HashSet::new(),
            Some(7),
            100 * 1024,
        );
        assert!(evicted > 0);
        assert!(
            dir.join(format!("{trashed}.jpg")).exists(),
            "an all-trashed open folder is still the open folder"
        );
        assert!(!dir.join(format!("{elsewhere}.jpg")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn protected_group_survives_even_at_zero_budget() {
        let dir = std::env::temp_dir().join(format!("emberjan0-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = "eeeeeeeeeeeeeeee";
        write(&dir, &format!("{id}.jpg"), 50);
        let protected: HashSet<String> = [id.to_string()].into();
        let (evicted, kept) = run_once(&dir, &HashMap::new(), &HashMap::new(), &protected, None, 1);
        assert_eq!(evicted, 0, "the open folder is never evicted");
        assert!(kept > 0);
        assert!(dir.join(format!("{id}.jpg")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
