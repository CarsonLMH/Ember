use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use tauri::Emitter;

use crate::store::Store;

/// Absolute path to exiftool when it lives in a known Homebrew prefix, bare
/// name otherwise. Finder-launched apps get launchd's minimal PATH (no
/// /opt/homebrew/bin), so bare-name resolution only works from a terminal.
pub fn exiftool_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        ["/opt/homebrew/bin/exiftool", "/usr/local/bin/exiftool"]
            .iter()
            .map(Path::new)
            .find(|p| p.exists())
            .map_or_else(|| PathBuf::from("exiftool"), Path::to_path_buf)
    })
}

/// Persistent `exiftool -stay_open` worker. One process, serialized commands.
/// Used for embedded-JPEG writes and existing-sidecar updates; brand-new
/// sidecars are written directly (no exiftool involved).
pub struct Exiftool {
    inner: Mutex<Option<Child>>,
}

impl Exiftool {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    fn spawn() -> std::io::Result<Child> {
        Command::new(exiftool_bin())
            .args(["-stay_open", "True", "-@", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }

    /// Run one exiftool command (args, one per line) and return its output.
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut guard = self.inner.lock().unwrap();
        if guard.is_none() {
            *guard = Some(Self::spawn().map_err(|e| format!("exiftool spawn: {e}"))?);
        }
        let child = guard.as_mut().unwrap();
        let result = Self::round_trip(child, args);
        if result.is_err() {
            // Worker wedged or died — drop it; next call respawns.
            let _ = child.kill();
            *guard = None;
        }
        result
    }

    fn round_trip(child: &mut Child, args: &[&str]) -> Result<String, String> {
        let stdin = child.stdin.as_mut().ok_or("exiftool stdin gone")?;
        let mut cmd = String::new();
        for a in args {
            cmd.push_str(a);
            cmd.push('\n');
        }
        cmd.push_str("-execute\n");
        stdin
            .write_all(cmd.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("exiftool write: {e}"))?;

        let stdout = child.stdout.as_mut().ok_or("exiftool stdout gone")?;
        let mut reader = BufReader::new(stdout);
        let mut out = String::new();
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .map_err(|e| format!("exiftool read: {e}"))?;
            if n == 0 {
                return Err("exiftool exited unexpectedly".into());
            }
            if line.starts_with("{ready") {
                return Ok(out);
            }
            out.push_str(&line);
        }
    }
}

fn sidecar_path(raf: &Path) -> PathBuf {
    raf.with_extension("xmp")
}

/// Minimal valid XMP packet carrying only a rating — used ONLY when no sidecar
/// exists. Existing sidecars (Capture One edits!) are updated via exiftool so
/// their contents are preserved.
fn minimal_sidecar(rating: u8, tags: Option<&[String]>) -> String {
    let subject = match tags {
        Some(tags) if !tags.is_empty() => {
            let items: String = tags
                .iter()
                .map(|t| format!("<rdf:li>{}</rdf:li>", xml_escape(t)))
                .collect();
            format!("\n   <dc:subject><rdf:Bag>{items}</rdf:Bag></dc:subject>",)
        }
        _ => String::new(),
    };
    format!(
        r#"<?xpacket begin="\u{{FEFF}}" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Ember">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmp:Rating="{rating}">{subject}
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
    )
    .replace("\\u{FEFF}", "\u{FEFF}")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

const STAR_RATING_XATTR: &str = "com.apple.metadata:kMDItemStarRating";

/// ApolloOne (and Spotlight/Finder) keep the LIVE rating in this xattr as a
/// binary plist integer; embedded XMP lags behind in ApolloOne's workflow.
fn read_xattr_rating(path: &Path) -> Option<u8> {
    let bytes = xattr::get(path, STAR_RATING_XATTR).ok()??;
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let n = match value {
        plist::Value::Integer(i) => i.as_signed()?,
        plist::Value::Real(f) => f as i64,
        _ => return None,
    };
    (0..=5).contains(&n).then_some(n as u8)
}

fn write_xattr_rating(path: &Path, rating: u8) -> Result<(), String> {
    let mut buf = Vec::new();
    plist::Value::Integer((rating as i64).into())
        .to_writer_binary(&mut buf)
        .map_err(|e| e.to_string())?;
    xattr::set(path, STAR_RATING_XATTR, &buf).map_err(|e| e.to_string())
}

/// Write a rating everywhere consumers look:
/// - embedded XMP in the JPEG (Capture One, iCloud path; atomic exiftool rewrite)
/// - kMDItemStarRating xattr on both files (ApolloOne, Spotlight, Finder)
/// - .xmp sidecar: always for RAF; for JPEG-only photos only if one already
///   exists (never invent sidecars for plain JPEGs). Existing sidecars are
///   updated via exiftool so Capture One edits in them survive.
pub fn write_meta(
    et: &Exiftool,
    jpeg: Option<&Path>,
    raf: Option<&Path>,
    rating: u8,
    tags: Option<&[String]>,
) -> Result<(), String> {
    // dc:subject args: clear then re-add each keyword (exiftool list semantics).
    let mut tag_args: Vec<String> = Vec::new();
    if let Some(tags) = tags {
        tag_args.push("-XMP-dc:Subject=".into());
        for t in tags {
            tag_args.push(format!("-XMP-dc:Subject+={t}"));
        }
    }
    if let Some(jpeg) = jpeg {
        if jpeg.exists() {
            let arg = format!("-XMP-xmp:Rating={rating}");
            let mut args: Vec<&str> = vec!["-overwrite_original", "-m", &arg];
            args.extend(tag_args.iter().map(String::as_str));
            args.push(jpeg.to_str().ok_or("non-utf8 jpeg path")?);
            let out = et.run(&args)?;
            if !out.contains("1 image files updated") {
                return Err(format!("jpeg xmp write failed: {}", out.trim()));
            }
            write_xattr_rating(jpeg, rating)?;
        }
    }
    if let Some(raf) = raf {
        if raf.exists() {
            write_xattr_rating(raf, rating)?;
        }
    }
    // Sidecar: RAF's sidecar (created if missing), else an existing JPEG sidecar.
    let sidecar = match (raf, jpeg) {
        (Some(raf), _) if raf.exists() => Some((sidecar_path(raf), true)),
        (None, Some(jpeg)) => {
            let s = sidecar_path(jpeg);
            s.exists().then_some((s, false))
        }
        _ => None,
    };
    if let Some((sidecar, create_if_missing)) = sidecar {
        if sidecar.exists() {
            let arg = format!("-XMP-xmp:Rating={rating}");
            let mut args: Vec<&str> = vec!["-overwrite_original", "-m", &arg];
            args.extend(tag_args.iter().map(String::as_str));
            args.push(sidecar.to_str().ok_or("non-utf8 sidecar path")?);
            let out = et.run(&args)?;
            if !out.contains("1 image files updated") {
                return Err(format!("sidecar xmp update failed: {}", out.trim()));
            }
        } else if create_if_missing {
            let tmp = sidecar.with_extension("xmp.tmp");
            std::fs::write(&tmp, minimal_sidecar(rating, tags)).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &sidecar).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Rating extraction for adoption at first scan — no exiftool.
/// Precedence: xattr (ApolloOne's live value) > sidecar > embedded XMP,
/// because ApolloOne only flushes to XMP on demand and it goes stale.
pub fn read_rating(jpeg: Option<&Path>, raf: Option<&Path>) -> Option<u8> {
    for f in [jpeg, raf].into_iter().flatten() {
        if let Some(r) = read_xattr_rating(f) {
            return Some(r);
        }
    }
    for f in [raf, jpeg].into_iter().flatten() {
        let sidecar = sidecar_path(f);
        if let Ok(text) = std::fs::read_to_string(&sidecar) {
            if let Some(r) = extract_rating(&text) {
                return Some(r);
            }
        }
    }
    if let Some(jpeg) = jpeg {
        if let Ok(mut f) = std::fs::File::open(jpeg) {
            let mut head = vec![0u8; 256 * 1024];
            if let Ok(n) = f.read(&mut head) {
                head.truncate(n);
                let text = String::from_utf8_lossy(&head);
                if let Some(r) = extract_rating(&text) {
                    return Some(r);
                }
            }
        }
    }
    None
}

fn extract_rating(text: &str) -> Option<u8> {
    // Attribute form: xmp:Rating="3" — element form: <xmp:Rating>3</xmp:Rating>
    for pattern in ["xmp:Rating=\"", "<xmp:Rating>"] {
        if let Some(idx) = text.find(pattern) {
            let rest = &text[idx + pattern.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<i16>() {
                if (0..=5).contains(&v) {
                    return Some(v as u8);
                }
            }
        }
    }
    None
}

/// Block until the write-behind queue is empty or `timeout` elapses. Called on
/// quit so acknowledged verdicts reach their files before the process dies.
/// Parked (failed) rows never block; undrained rows resume on next launch.
pub fn drain_blocking(store: &Store, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let pending = store.xmp_status().map(|s| s.pending).unwrap_or(0);
        if pending == 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!("xmp drain timed out; {pending} writes resume on next launch");
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Background write-behind queue: drains xmp_queue seconds behind the journal.
/// Crash-safe (queue is table-backed); emits pending count for the HUD.
pub fn spawn_queue_worker(app: tauri::AppHandle, store: Arc<Store>, et: Arc<Exiftool>) {
    std::thread::Builder::new()
        .name("xmp-queue".into())
        .spawn(move || {
            let mut last_status: Option<crate::store::XmpStatus> = None;
            loop {
                let batch = store.xmp_take_batch(8).unwrap_or_default();
                if batch.is_empty() {
                    let status = store.xmp_status().unwrap_or_default();
                    if last_status.as_ref() != Some(&status) {
                        let _ = app.emit("xmp-pending", &status);
                        last_status = Some(status);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    continue;
                }
                for job in batch {
                    process_job(&store, &et, &job);
                }
                let status = store.xmp_status().unwrap_or_default();
                if last_status.as_ref() != Some(&status) {
                    let _ = app.emit("xmp-pending", &status);
                    last_status = Some(status);
                }
            }
        })
        .expect("spawn xmp queue worker");
}

/// One queue job, start to finish. The queue row is the coordination token:
/// it tells the face worker "this file is mid-rewrite, don't claim it"
/// (`facestore::xmp_write_pending`), so it may only be removed once BOTH the
/// exiftool rewrite AND the stat refresh have really happened. On any refresh
/// failure the row stays and the job goes back through the queue's normal
/// policy (retried while attempts remain, then parked visibly as `failed`) —
/// the rewrite is idempotent, so re-running it is harmless. An `xmp_done`
/// failure likewise leaves the row for the next pass instead of vanishing.
fn process_job(store: &Store, et: &Exiftool, job: &crate::store::XmpJob) {
    process_job_inner(store, et, job, &|| {});
}

/// `pause` runs between the exiftool rewrite and the stat refresh — the exact
/// window where the file's mtime has moved but `face_scan`'s stat has not.
/// Production does nothing there; the coordination tests park a real thread.
fn process_job_inner(store: &Store, et: &Exiftool, job: &crate::store::XmpJob, pause: &dyn Fn()) {
    let jpeg = job.jpeg.as_ref().map(PathBuf::from);
    let raf = job.raf.as_ref().map(PathBuf::from);
    match write_meta(
        et,
        jpeg.as_deref(),
        raf.as_deref(),
        job.rating,
        job.tags.as_deref(),
    ) {
        Ok(()) => {
            pause();
            // Stats FIRST, then the queue row — and the row survives unless
            // the refresh SUCCEEDED. Removing it after a failed refresh would
            // expose the rewritten mtime against the old face_scan stat: the
            // needless-reindex window this guard exists to close.
            let refreshed = match &jpeg {
                Some(j) => refresh_stats_after_rewrite(store, &job.photo_id, j),
                None => Ok(()), // no JPEG was rewritten; nothing to refresh
            };
            match refreshed {
                Ok(()) => {
                    if let Err(e) = store.xmp_done(&job.photo_id, job.rating) {
                        // The row stays; the next pass rewrites (idempotent)
                        // and tries again. Never silent.
                        eprintln!("xmp: completed write for {} but could not clear its queue row ({e}); will retry", job.photo_id);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "xmp: {} rewritten but stat refresh failed ({e}); keeping the queue row",
                        job.photo_id
                    );
                    let _ = store.xmp_error(&job.photo_id, &format!("stat refresh: {e}"));
                }
            }
        }
        Err(e) => {
            let _ = store.xmp_error(&job.photo_id, &e);
        }
    }
}

/// A metadata-only JPEG rewrite changed mtime/size but not one pixel: refresh
/// preview validity so the preview isn't regenerated — and the `face_scan`
/// stat so a star rating never triggers a face re-index (plan round 3).
/// Fallible on purpose: a caller that cannot prove the stats are refreshed
/// must not drop the queue row that guards the window.
fn refresh_stats_after_rewrite(store: &Store, photo_id: &str, jpeg: &Path) -> Result<(), String> {
    let meta =
        std::fs::metadata(jpeg).map_err(|e| format!("stat {} failed: {e}", jpeg.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if store
        .preview_stat(photo_id)
        .map_err(|e| format!("preview stat lookup: {e}"))?
        .is_some()
    {
        store
            .set_preview_stat(photo_id, mtime, meta.len())
            .map_err(|e| format!("preview stat refresh: {e}"))?;
    }
    store
        .face_scan_refresh_stat(photo_id, mtime as i64, meta.len() as i64)
        .map_err(|e| format!("face_scan stat refresh: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat_of(p: &Path) -> (i64, i64) {
        let m = std::fs::metadata(p).unwrap();
        (
            m.modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            m.len() as i64,
        )
    }

    /// Store + one JPEG photo, face-indexed at the file's current stat, plus a
    /// SEPARATE face-worker connection (the two-connection production shape).
    fn xmp_face_setup(
        tag: &str,
    ) -> (
        PathBuf,
        PathBuf,
        Arc<Store>,
        rusqlite::Connection,
        String,
        i64,
        i64,
    ) {
        assert!(
            Command::new(exiftool_bin()).arg("-ver").output().is_ok(),
            "exiftool is a prerequisite for this acceptance test (brew install exiftool)"
        );
        let dir = std::env::temp_dir().join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jpeg = dir.join("DSCF0001.JPG");
        image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8, y as u8, 90]))
            .save(&jpeg)
            .unwrap();
        let store = Arc::new(Store::new(&dir.join("t.sqlite3")).unwrap());
        let folder = store.open_folder(dir.to_str().unwrap()).unwrap();
        let entries = crate::scanner::scan(&dir);
        assert_eq!(entries.len(), 1);
        let id = entries[0].id.clone();
        store.sync_photos(folder.folder_id, &entries).unwrap();
        let (mtime, size) = stat_of(&jpeg);
        let gen = {
            let mut conn = store.lock_conn();
            let gen = crate::facestore::ensure_gen(&mut conn, "d", "r", 1).unwrap();
            let snap = crate::facestore::snapshot(&conn, &id, gen).unwrap();
            crate::facestore::commit_scan(&mut conn, &id, &snap, &[], &[], mtime, size, &|| true)
                .unwrap();
            gen
        };
        let worker = rusqlite::Connection::open(dir.join("t.sqlite3")).unwrap();
        worker.pragma_update(None, "busy_timeout", 5000).unwrap();
        (dir, jpeg, store, worker, id, gen, folder.folder_id)
    }

    /// The round-3 acceptance requirement, with the queue job running on its
    /// own thread and a rendezvous parking it INSIDE the dangerous window —
    /// after exiftool has rewritten the file (mtime moved), before the stat
    /// refresh — while a face worker's claim gate, on its own connection,
    /// interrogates the photo concurrently. It must refuse at every instant:
    /// queued, mid-window, and after the queue drains.
    ///
    /// exiftool is a documented prerequisite (CLAUDE.md), so its absence fails
    /// this test rather than quietly passing it.
    #[test]
    fn a_rating_write_never_costs_a_face_reindex() {
        let (dir, jpeg, store, worker, id, gen, folder_id) = xmp_face_setup("a2xmpface");
        let (mtime, size) = stat_of(&jpeg);
        // What the face worker would decide at this instant, on ITS conn.
        let would_claim =
            |fresh: (i64, i64)| crate::faces::claim_photo(&worker, &id, gen, Some(fresh), false);
        assert!(
            !would_claim((mtime, size)),
            "nothing to do before the write"
        );

        // The user rates the photo: the journal acks and the write is queued.
        store.set_rating(folder_id, &id, 4).unwrap();
        let mut batch = store.xmp_take_batch(8).unwrap();
        assert_eq!(batch.len(), 1, "the rating is queued");
        let job = batch.remove(0);
        assert!(
            !would_claim((mtime, size)),
            "a queued metadata write parks this photo"
        );

        // The queue worker's job runs on its own thread and parks mid-window.
        let (reached_tx, reached_rx) = crossbeam_channel::bounded::<()>(1);
        let (go_tx, go_rx) = crossbeam_channel::bounded::<()>(1);
        let queue_thread = {
            let store = store.clone();
            std::thread::spawn(move || {
                process_job_inner(&store, &Exiftool::new(), &job, &|| {
                    reached_tx.send(()).unwrap();
                    let _ = go_rx.recv();
                });
            })
        };
        reached_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the queue job must reach the post-rewrite window");
        // THIS is the dangerous instant, for real: the rewrite has happened
        // on the other thread, the stats have not been refreshed.
        let after = stat_of(&jpeg);
        assert_ne!(after, (mtime, size), "the rewrite did change the file");
        assert!(
            !would_claim(after),
            "a worker looking mid-window must refuse — the queue row is what \
             tells it these pixels did not change"
        );
        go_tx.send(()).unwrap();
        queue_thread.join().unwrap();

        // The job completed: stats refreshed FIRST, then the row removed.
        assert_eq!(store.xmp_status().unwrap().pending, 0, "queue drained");
        let row = crate::facestore::scan_row(&worker, &id).unwrap().unwrap();
        assert_eq!(row.status, "ok", "a rating never invalidates a scan");
        assert!(
            !would_claim(after),
            "and with the queue empty there is still no work: the stat matches"
        );

        // Control: a real pixel change IS claimed, so the guard above is not
        // simply "this worker never claims anything".
        std::fs::write(&jpeg, b"different bytes entirely").unwrap();
        assert!(would_claim(stat_of(&jpeg)), "changed pixels do re-index");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The failure boundary the round-3 review found missing: exiftool
    /// succeeds but the stat refresh FAILS (injected by whisking the file
    /// away inside the window). The queue row must survive — dropping it
    /// would expose the rewritten mtime against the old face_scan stat — and
    /// a later successful retry is what finally clears it.
    #[test]
    fn a_failed_stat_refresh_keeps_the_queue_guard_until_a_retry_lands() {
        let (dir, jpeg, store, worker, id, gen, folder_id) = xmp_face_setup("a2xmpfail");
        let would_claim =
            |fresh: (i64, i64)| crate::faces::claim_photo(&worker, &id, gen, Some(fresh), false);

        store.set_rating(folder_id, &id, 3).unwrap();
        let job = store.xmp_take_batch(8).unwrap().remove(0);
        let hidden = dir.join("hidden.JPG");
        process_job_inner(&store, &Exiftool::new(), &job, &|| {
            // The refresh's stat lookup will fail: the file is gone.
            std::fs::rename(&jpeg, &hidden).unwrap();
        });
        assert_eq!(
            store.xmp_status().unwrap().pending,
            1,
            "the queue row survives a failed refresh"
        );
        std::fs::rename(&hidden, &jpeg).unwrap();
        assert!(
            !would_claim(stat_of(&jpeg)),
            "the guard holds: the rewritten file is still not claimable"
        );

        // The queue's normal retry policy runs the job again; this time the
        // refresh lands and the guard is finally released.
        let retry = store.xmp_take_batch(8).unwrap().remove(0);
        process_job_inner(&store, &Exiftool::new(), &retry, &|| {});
        assert_eq!(store.xmp_status().unwrap().pending, 0, "guard removed");
        let row = crate::facestore::scan_row(&worker, &id).unwrap().unwrap();
        assert_eq!(row.status, "ok");
        assert!(
            !would_claim(stat_of(&jpeg)),
            "refreshed stat: a metadata-only rewrite still costs no re-index"
        );
        // Control: a genuine pixel change is still claimed.
        std::fs::write(&jpeg, b"new pixels").unwrap();
        assert!(would_claim(stat_of(&jpeg)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn drain_blocking_honors_queue_and_timeout() {
        let dir = std::env::temp_dir().join(format!("a2drain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir.join("t.sqlite3")).unwrap();
        let ms = std::time::Duration::from_millis;

        // Empty queue: returns immediately.
        assert!(drain_blocking(&store, ms(50)));

        // A pending write with no worker: times out false.
        let f = store.open_folder("/tmp/x").unwrap();
        let entry = std::sync::Arc::new(crate::scanner::PhotoEntry {
            id: "a".into(),
            dir: "/tmp/x".into(),
            stem: "A".into(),
            jpeg: Some("/tmp/x/A.JPG".into()),
            raf: None,
            mtime: 1,
            size: 1,
        });
        store.sync_photos(f.folder_id, &[entry]).unwrap();
        store.set_rating(f.folder_id, "a", 3).unwrap();
        assert!(!drain_blocking(&store, ms(250)));

        // Once the write is done, drain succeeds.
        store.xmp_done("a", 3).unwrap();
        assert!(drain_blocking(&store, ms(50)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn extracts_rating_in_both_forms() {
        assert_eq!(extract_rating(r#"... xmp:Rating="4" ..."#), Some(4));
        assert_eq!(extract_rating("<xmp:Rating>2</xmp:Rating>"), Some(2));
        assert_eq!(extract_rating("nothing here"), None);
        assert_eq!(
            extract_rating(r#"xmp:Rating="9""#),
            None,
            "out of range rejected"
        );
    }

    #[test]
    fn xattr_rating_roundtrip_and_precedence() {
        let dir = std::env::temp_dir().join(format!("a2xattr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("photo.jpg");
        std::fs::write(&f, b"not a real jpeg").unwrap();

        write_xattr_rating(&f, 4).unwrap();
        assert_eq!(read_xattr_rating(&f), Some(4));
        // xattr wins over embedded/sidecar (ApolloOne's live value).
        assert_eq!(read_rating(Some(&f), None), Some(4));

        // Parses ApolloOne's own bplist encoding (integer payload).
        let mut buf = Vec::new();
        plist::Value::Integer(3.into())
            .to_writer_binary(&mut buf)
            .unwrap();
        xattr::set(&f, STAR_RATING_XATTR, &buf).unwrap();
        assert_eq!(read_xattr_rating(&f), Some(3));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn minimal_sidecar_is_readable_back() {
        let xml = minimal_sidecar(3, Some(&["mood: bright".into()]));
        assert_eq!(extract_rating(&xml), Some(3));
        assert!(
            xml.contains("W5M0MpCehiHzreSzNTczkc9d"),
            "must be a valid xpacket"
        );
        assert!(
            xml.contains("<rdf:li>mood: bright</rdf:li>"),
            "subject bag present"
        );
    }

    /// End-to-end file-safety test: embedded XMP write leaves image data
    /// byte-identical (exiftool -ImageDataHash). Skips when exiftool is absent.
    #[test]
    fn jpeg_write_preserves_image_data() {
        if Command::new(exiftool_bin()).arg("-ver").output().is_err() {
            eprintln!("exiftool not installed; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("a2xmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jpeg = dir.join("test.jpg");
        let img = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8, y as u8, 128]));
        img.save(&jpeg).unwrap();

        let hash = |p: &Path| -> String {
            let out = Command::new(exiftool_bin())
                .args(["-ImageDataHash", "-s3", p.to_str().unwrap()])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let before = hash(&jpeg);
        assert!(!before.is_empty());

        let et = Exiftool::new();
        write_meta(
            &et,
            Some(&jpeg),
            None,
            5,
            Some(&["bright".into(), "opener".into()]),
        )
        .unwrap();
        assert_eq!(
            hash(&jpeg),
            before,
            "image data must be untouched by XMP writes"
        );

        // Rating must be readable back (what Capture One sees).
        let out = Command::new(exiftool_bin())
            .args(["-XMP-xmp:Rating", "-s3", jpeg.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_created_and_updated() {
        if Command::new(exiftool_bin()).arg("-ver").output().is_err() {
            eprintln!("exiftool not installed; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("a2side-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Fake RAF file — never touched, only its sidecar.
        let raf = dir.join("DSCF0001.RAF");
        std::fs::write(&raf, b"fake raf bytes").unwrap();
        let raf_before = std::fs::read(&raf).unwrap();

        let et = Exiftool::new();
        write_meta(&et, None, Some(&raf), 2, Some(&["melancholy".into()])).unwrap();
        let sidecar = dir.join("DSCF0001.xmp");
        assert!(sidecar.exists());
        assert_eq!(read_rating(None, Some(&raf)), Some(2));

        // Second write goes through exiftool update path, preserving the file.
        write_meta(&et, None, Some(&raf), 4, None).unwrap();
        assert_eq!(read_rating(None, Some(&raf)), Some(4));
        assert_eq!(
            std::fs::read(&raf).unwrap(),
            raf_before,
            "RAF must never be modified"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
