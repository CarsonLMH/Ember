use std::path::{Path, PathBuf};

/// Move a file back from the Trash: rename when same-volume, else copy +
/// xattrs + fsync + rename-into-place + best-effort source removal.
/// NSFileManager trashes to the file's own volume, so restores/rollbacks
/// across volumes hit EXDEV on a bare rename.
pub fn move_file(src: &Path, dst: &Path) -> Result<(), String> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => copy_fallback(src, dst),
        Err(e) => Err(format!("move {} failed: {e}", src.display())),
    }
}

fn copy_fallback(src: &Path, dst: &Path) -> Result<(), String> {
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("restore");
    let tmp = dst.with_file_name(format!("{name}.part-{}", std::process::id()));
    let fail = |what: &str, e: std::io::Error| {
        let _ = std::fs::remove_file(&tmp);
        format!("{what} {} failed: {e}", src.display())
    };
    std::fs::copy(src, &tmp).map_err(|e| fail("copy", e))?;
    // fs::copy drops xattrs, and kMDItemStarRating lives there.
    if let Ok(names) = xattr::list(src) {
        for attr in names {
            if let Ok(Some(v)) = xattr::get(src, &attr) {
                let _ = xattr::set(&tmp, &attr, &v);
            }
        }
    }
    std::fs::File::open(&tmp)
        .and_then(|f| f.sync_all())
        .map_err(|e| fail("fsync", e))?;
    std::fs::rename(&tmp, dst).map_err(|e| fail("publish", e))?;
    // Best-effort: a lingering Trash copy beats data loss.
    let _ = std::fs::remove_file(src);
    Ok(())
}

/// Move one file to the macOS system Trash via NSFileManager, returning where
/// it landed (needed for undo/restore and the session trash panel).
pub fn trash_file(path: &Path) -> Result<PathBuf, String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let s = path
        .to_str()
        .ok_or_else(|| format!("non-UTF8 path: {}", path.display()))?;
    let fm = NSFileManager::defaultManager();
    let url = NSURL::fileURLWithPath(&NSString::from_str(s));
    let mut resulting: Option<objc2::rc::Retained<NSURL>> = None;
    fm.trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
        .map_err(|e| e.localizedDescription().to_string())?;
    let landed = resulting
        .and_then(|u| u.path().map(|p| PathBuf::from(p.to_string())))
        .ok_or_else(|| "trash succeeded but no resulting URL".to_string())?;
    Ok(landed)
}

pub struct TrashedPair {
    pub jpeg: Option<PathBuf>,
    pub raf: Option<PathBuf>,
}

/// Pair-atomic trash: both files or neither. If the RAF fails after the JPEG
/// was trashed, restoring the JPEG is attempted before returning. A failed
/// rollback reports both errors and the paths needed for manual recovery.
pub fn trash_pair(jpeg: Option<&Path>, raf: Option<&Path>) -> Result<TrashedPair, String> {
    trash_pair_with(jpeg, raf, trash_file, restore_file)
}

fn trash_pair_with<T, R>(
    jpeg: Option<&Path>,
    raf: Option<&Path>,
    mut trash_one: T,
    mut restore_one: R,
) -> Result<TrashedPair, String>
where
    T: FnMut(&Path) -> Result<PathBuf, String>,
    R: FnMut(&Path, &Path) -> Result<(), String>,
{
    let trashed_jpeg = match jpeg {
        Some(p) => Some((p.to_path_buf(), trash_one(p)?)),
        None => None,
    };
    let trashed_raf = match raf {
        Some(p) => match trash_one(p) {
            Ok(t) => Some(t),
            Err(e) => {
                // Roll back: restore the JPEG from the Trash.
                if let Some((orig, in_trash)) = &trashed_jpeg {
                    return match restore_one(in_trash, orig) {
                        Ok(()) => Err(format!("RAF trash failed, JPEG restored: {e}")),
                        Err(rollback) => Err(format!(
                            "RAF trash failed: {e}; JPEG rollback failed: {rollback}. \
                             The JPEG may still be in the Trash at {}; verify it and restore it \
                             to {} before retrying",
                            in_trash.display(),
                            orig.display()
                        )),
                    };
                }
                return Err(format!("RAF trash failed: {e}"));
            }
        },
        None => None,
    };
    Ok(TrashedPair {
        jpeg: trashed_jpeg.map(|(_, t)| t),
        raf: trashed_raf,
    })
}

/// Restore a file from the Trash back to its original location.
pub fn restore_file(in_trash: &Path, original: &Path) -> Result<(), String> {
    if original.exists() {
        return Err(format!(
            "restore target already exists: {}",
            original.display()
        ));
    }
    move_file(in_trash, original).map_err(|e| format!("restore {}: {e}", original.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn trash_and_restore_roundtrip() {
        let dir = std::env::temp_dir().join(format!("a2trash-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("victim.txt");
        fs::write(&f, b"hello").unwrap();

        let landed = trash_file(&f).expect("trash should succeed");
        assert!(!f.exists(), "file should be gone from original location");
        assert!(landed.exists(), "file should exist in the Trash");

        restore_file(&landed, &f).expect("restore should succeed");
        assert!(f.exists());
        assert_eq!(fs::read(&f).unwrap(), b"hello");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copy_fallback_preserves_bytes_and_xattrs() {
        let dir = std::env::temp_dir().join(format!("a2exdev-{}", std::process::id()));
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();
        let src = dir.join("a/DSCF0001.JPG");
        let dst = dir.join("b/DSCF0001.JPG");
        fs::write(&src, b"pixels").unwrap();
        xattr::set(&src, "com.apple.metadata:kMDItemStarRating", b"\x03").unwrap();

        copy_fallback(&src, &dst).expect("fallback should succeed");
        assert_eq!(fs::read(&dst).unwrap(), b"pixels");
        assert_eq!(
            xattr::get(&dst, "com.apple.metadata:kMDItemStarRating")
                .unwrap()
                .as_deref(),
            Some(&b"\x03"[..]),
            "xattr rating must survive the copy path"
        );
        assert!(!src.exists(), "source should be removed after publish");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pair_rolls_back_when_raf_fails() {
        let dir = std::env::temp_dir().join(format!("a2pair-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let jpeg = dir.join("DSCF0001.JPG");
        fs::write(&jpeg, b"jpeg").unwrap();
        let missing_raf = dir.join("DSCF0001.RAF"); // never created → trash fails

        let result = trash_pair(Some(&jpeg), Some(&missing_raf));
        assert!(result.is_err());
        assert!(
            jpeg.exists(),
            "JPEG must be rolled back when RAF trash fails"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pair_reports_when_raf_trash_and_jpeg_rollback_both_fail() {
        let jpeg = Path::new("/photos/DSCF0001.JPG");
        let raf = Path::new("/photos/DSCF0001.RAF");
        let trashed_jpeg = PathBuf::from("/Trash/DSCF0001.JPG");

        let error = trash_pair_with(
            Some(jpeg),
            Some(raf),
            |path| {
                if path == jpeg {
                    Ok(trashed_jpeg.clone())
                } else {
                    Err("injected RAF trash failure".into())
                }
            },
            |_in_trash, _original| Err("injected JPEG restore failure".into()),
        )
        .err()
        .expect("the pair operation must fail");

        assert!(error.contains("injected RAF trash failure"));
        assert!(error.contains("injected JPEG restore failure"));
        assert!(error.contains("/Trash/DSCF0001.JPG"));
        assert!(error.contains("/photos/DSCF0001.JPG"));
        assert!(
            !error.contains("JPEG restored"),
            "a failed rollback must never be reported as restored: {error}"
        );
    }
}
