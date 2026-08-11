use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use tauri::http::{header, Response, StatusCode};
use tauri::UriSchemeResponder;

use crate::faces::ChipRepair;
use crate::preview::PreviewState;

/// A photo:// request handed off the WebKit callback thread.
pub struct Job {
    pub path: String,
    pub responder: UriSchemeResponder,
}

pub fn channel() -> (Sender<Job>, Receiver<Job>) {
    crossbeam_channel::unbounded()
}

fn respond(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        // The webview origin differs between dev (http://localhost:1420) and
        // prod (tauri://localhost); without ACAO the canvas taints. Local app, "*" is fine.
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CONTENT_TYPE, content_type)
        // We manage caching ourselves (ImageBitmap LRU); double-caching wastes memory.
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .expect("static response headers are valid")
}

fn handle(state: &PreviewState, repair: &ChipRepair, path: &str) -> Response<Vec<u8>> {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let (route, id_raw) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    // face/{id}/{n}/{rev}: baked chip, or 404 + a deduplicated repair enqueue
    // — the frontend's 404-retry img pattern picks the chip up when it lands.
    // The URL names the exact detection (`chip_revision`) the chip must come
    // from: a file surviving from an older scan has a different name, so this
    // route can never serve it as the current chip — the DB is never
    // consulted here, the filename is the proof of identity. The repair queue
    // is a dedicated thread; these workers never block on it.
    if route == "face" {
        let mut sub = id_raw.splitn(3, '/');
        let (fid, n_raw, rev_raw) = (
            sub.next().unwrap_or(""),
            sub.next().unwrap_or(""),
            sub.next().unwrap_or(""),
        );
        let rev = rev_raw.split('?').next().unwrap_or("");
        let valid = !fid.is_empty()
            && fid.chars().all(|c| c.is_ascii_hexdigit())
            && !n_raw.is_empty()
            && n_raw.chars().all(|c| c.is_ascii_digit())
            && !rev.is_empty()
            && rev.chars().all(|c| c.is_ascii_digit());
        let parsed = valid
            .then(|| Some((n_raw.parse::<i64>().ok()?, rev.parse::<i64>().ok()?)))
            .flatten();
        let Some((n, rev)) = parsed else {
            return respond(StatusCode::NOT_FOUND, "text/plain", Vec::new());
        };
        let p = state.face_chip_path(fid, n, rev);
        return match std::fs::read(&p) {
            Ok(bytes) => respond(StatusCode::OK, "image/jpeg", bytes),
            Err(_) => {
                repair.enqueue(fid);
                respond(StatusCode::NOT_FOUND, "image/jpeg", Vec::new())
            }
        };
    }
    // Retry queries (?r=n) bust WebKit's negative cache; strip before lookup.
    let id = id_raw.split('?').next().unwrap_or("");
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return respond(StatusCode::NOT_FOUND, "text/plain", Vec::new());
    }
    let (file, content_type) = match route {
        // Served straight from the cache dir — no live entry required, so
        // trashed photos keep their thumbnails in the trash panel.
        // Not generated yet → 404; the frontend falls back to /orig and
        // records the flip as an "original-fallback" serve.
        "preview" => {
            let p = state.preview_path_for_id(id);
            (p.exists().then_some(p), "image/jpeg")
        }
        "thumb" => (state.ensure_thumb(id), "image/jpeg"),
        "orig" => (state.orig_source(id), "image/jpeg"),
        "hist" => (
            state
                .ensure_exposure(id)
                .map(|()| state.hist_path_for_id(id)),
            "application/json",
        ),
        "mask" => (
            state
                .ensure_exposure(id)
                .map(|()| state.mask_path_for_id(id)),
            "image/png",
        ),
        _ => (None, "text/plain"),
    };
    match file.and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => respond(StatusCode::OK, content_type, bytes),
        None => respond(StatusCode::NOT_FOUND, content_type, Vec::new()),
    }
}

/// Fixed worker pool: bounds read concurrency and keeps disk reads off
/// WebKit's scheme-handler callback thread.
pub fn spawn_workers(
    state: Arc<PreviewState>,
    repair: Arc<ChipRepair>,
    rx: Receiver<Job>,
    n: usize,
) {
    for i in 0..n {
        let state = state.clone();
        let repair = repair.clone();
        let rx = rx.clone();
        std::thread::Builder::new()
            .name(format!("photo-proto-{i}"))
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job.responder.respond(handle(&state, &repair, &job.path));
                }
            })
            .expect("spawn protocol worker");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (
        PreviewState,
        Arc<ChipRepair>,
        crossbeam_channel::Receiver<String>,
    ) {
        // Per-process counter, not a timestamp: parallel tests landing in the
        // same microsecond would share a directory.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "emberproto-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = PreviewState::new(dir, crate::exposure::BlinkiesCfg::default());
        let (repair, rx) = crate::faces::test_repair();
        (state, repair, rx)
    }

    const ID: &str = "00deadbeef00cafe";

    #[test]
    fn face_route_serves_baked_chip() {
        let (state, repair, rx) = setup();
        std::fs::write(state.face_chip_path(ID, 0, 7), b"jpegbytes").unwrap();
        let resp = handle(&state, &repair, &format!("/face/{ID}/0/7"));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body(), b"jpegbytes");
        assert!(rx.try_recv().is_err(), "a hit never enqueues repair");
    }

    /// The URL names the detection revision, and only a file of exactly that
    /// revision satisfies it — a surviving older chip 404s (and heals) instead
    /// of masquerading as the current crop.
    #[test]
    fn face_route_never_serves_another_revisions_chip() {
        let (state, repair, rx) = setup();
        std::fs::write(state.face_chip_path(ID, 0, 7), b"old-scan").unwrap();
        let resp = handle(&state, &repair, &format!("/face/{ID}/0/8"));
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(rx.try_recv().unwrap(), ID, "the stale chip triggers repair");
    }

    #[test]
    fn face_route_miss_enqueues_repair_once() {
        let (state, repair, rx) = setup();
        // Retry query (?r=1) must parse like the plain form.
        for path in [format!("/face/{ID}/2/1"), format!("/face/{ID}/2/1?r=1")] {
            let resp = handle(&state, &repair, &path);
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        }
        assert_eq!(rx.try_recv().unwrap(), ID, "repair enqueued");
        assert!(rx.try_recv().is_err(), "in-flight dedup: one job per photo");
    }

    #[test]
    fn face_route_rejects_malformed() {
        let (state, repair, rx) = setup();
        for path in [
            "/face/nothex!/0/1",         // non-hex id
            &format!("/face/{ID}"),      // missing index + revision
            &format!("/face/{ID}/0"),    // missing revision (the legacy form)
            &format!("/face/{ID}/0/"),   // empty revision
            &format!("/face/{ID}/x2/1"), // non-numeric index
            &format!("/face/{ID}/0/x1"), // non-numeric revision
            "/face//0/1",                // empty id
        ] {
            let resp = handle(&state, &repair, path);
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        }
        assert!(rx.try_recv().is_err(), "malformed never reaches repair");
    }

    /// **Blocker 1's end-to-end regression, through the production route and
    /// the production repair job.** Scan A publishes visibly distinguishable
    /// chips; scan B commits with moved rects; the process "dies" before B's
    /// bake, so A's files survive on disk — the exact crash gap repair is
    /// documented to heal. The route must never serve A's bytes as B's chip
    /// (under the old unversioned filenames it did, immediately and forever,
    /// because repair equated "path exists" with "current"), and repair must
    /// recognize A's surviving files as damage and publish B's own bytes.
    #[test]
    fn a_crash_between_two_scans_never_serves_the_old_chips_as_new() {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "emberxrev-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        let store = crate::store::Store::new(&dir.join("t.sqlite3")).unwrap();
        let folder = store.open_folder("/tmp/xrev").unwrap();
        store
            .sync_photos(
                folder.folder_id,
                &[std::sync::Arc::new(crate::scanner::PhotoEntry {
                    id: ID.into(),
                    dir: "/tmp/xrev".into(),
                    stem: "P1".into(),
                    jpeg: Some("/tmp/xrev/P1.JPG".into()),
                    raf: None,
                    mtime: 1,
                    size: 1,
                })],
            )
            .unwrap();
        let state = PreviewState::new(dir.join("cache"), crate::exposure::BlinkiesCfg::default());
        let (repair, rx) = crate::faces::test_repair();
        // A horizontal gradient: a crop from the left half and one from the
        // right half encode to visibly (and byte-wise) different JPEGs.
        let img = image::RgbImage::from_fn(400, 300, |x, _| {
            image::Rgb([(x % 256) as u8, 40, 200u8.saturating_sub((x / 2) as u8)])
        });
        crate::preview::write_jpeg(&img, &state.preview_path_for_id(ID), 85).unwrap();

        let mut worker = rusqlite::Connection::open(dir.join("t.sqlite3")).unwrap();
        worker.pragma_update(None, "foreign_keys", "ON").unwrap();
        worker.pragma_update(None, "busy_timeout", 5000).unwrap();
        let gen = crate::facestore::ensure_gen(&mut worker, "d", "r", 1).unwrap();
        let commit = |worker: &mut rusqlite::Connection, rect: [f32; 4]| -> i64 {
            let snap = crate::facestore::snapshot(worker, ID, gen).unwrap();
            let out = crate::facestore::commit_scan(
                worker,
                ID,
                &snap,
                &[crate::facestore::NewFace {
                    rect,
                    det_score: 0.9,
                    embedding: vec![0.1; crate::facedet::EMBED_DIM],
                }],
                &[],
                1,
                1,
                &|| true,
            )
            .unwrap();
            match out {
                crate::facestore::CommitOutcome::Committed(rev) => rev,
                other => panic!("{other:?}"),
            }
        };

        // Scan A commits and bakes (repair IS the bake path after a crash).
        let rev_a = commit(&mut worker, [0.05, 0.1, 0.2, 0.3]);
        crate::faces::repair_photo(&state, &store, ID);
        let chip_a = state.face_chip_path(ID, 0, rev_a);
        let bytes_a = std::fs::read(&chip_a).unwrap();
        let served_a = handle(&state, &repair, &format!("/face/{ID}/0/{rev_a}"));
        assert_eq!(served_a.status(), StatusCode::OK);
        assert_eq!(served_a.body(), &bytes_a, "scan A's chip serves normally");

        // Scan B replaces the rows (moved rect) — and the process dies before
        // B's bake. A's file is still sitting in the cache.
        let rev_b = commit(&mut worker, [0.7, 0.1, 0.2, 0.3]);
        assert_ne!(rev_b, rev_a);
        assert!(chip_a.exists(), "the crash gap: A's artifact survived");

        // The UI, refreshed from the DB, asks for B's chip. It must get a 404
        // and a repair enqueue — never A's bytes wearing B's identity.
        let resp = handle(&state, &repair, &format!("/face/{ID}/0/{rev_b}"));
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(resp.body().is_empty(), "no stale bytes leak through a 404");
        assert_eq!(rx.try_recv().unwrap(), ID, "the miss enqueued repair");

        // The repair job the queue would run: it must treat A's surviving
        // file as stale (wrong identity), rebake from B's rects, and sweep.
        crate::faces::repair_photo(&state, &store, ID);
        let resp = handle(&state, &repair, &format!("/face/{ID}/0/{rev_b}"));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_ne!(
            resp.body(),
            &bytes_a,
            "B's chip is B's own crop, not A's bytes under a new name"
        );
        assert!(!chip_a.exists(), "A's artifact did not outlive its scan");
        assert_eq!(
            handle(&state, &repair, &format!("/face/{ID}/0/{rev_a}")).status(),
            StatusCode::NOT_FOUND,
            "A's identity is gone from the protocol too"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// **Round-4 blocker 1: chip identity must survive privacy deletion.**
    /// The pre-delete scan's chip outlives a delete whose file sweep never
    /// completed (the fallible-sweep lifecycle the privacy protocol itself
    /// supports); the user re-enables indexing; the rescan commits and the
    /// process dies before its bake. Under per-photo revisions the rescan
    /// restarted at revision 1 and the survivor's name/URL matched the new
    /// detection exactly — the durable `scan_seq` counter is what makes this
    /// test pass: the new identity is minted ABOVE every pre-delete one, so
    /// the production route 404s and repair replaces the survivor.
    #[test]
    fn a_chip_surviving_a_failed_delete_sweep_never_serves_a_later_scan() {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "emberxwipe-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        let store = crate::store::Store::new(&dir.join("t.sqlite3")).unwrap();
        let folder = store.open_folder("/tmp/xwipe").unwrap();
        store
            .sync_photos(
                folder.folder_id,
                &[std::sync::Arc::new(crate::scanner::PhotoEntry {
                    id: ID.into(),
                    dir: "/tmp/xwipe".into(),
                    stem: "P1".into(),
                    jpeg: Some("/tmp/xwipe/P1.JPG".into()),
                    raf: None,
                    mtime: 1,
                    size: 1,
                })],
            )
            .unwrap();
        let state = PreviewState::new(dir.join("cache"), crate::exposure::BlinkiesCfg::default());
        let (repair, rx) = crate::faces::test_repair();
        let img = image::RgbImage::from_fn(400, 300, |x, _| {
            image::Rgb([(x % 256) as u8, 40, 200u8.saturating_sub((x / 2) as u8)])
        });
        crate::preview::write_jpeg(&img, &state.preview_path_for_id(ID), 85).unwrap();

        let mut worker = rusqlite::Connection::open(dir.join("t.sqlite3")).unwrap();
        worker.pragma_update(None, "foreign_keys", "ON").unwrap();
        worker.pragma_update(None, "busy_timeout", 5000).unwrap();
        let gen = crate::facestore::ensure_gen(&mut worker, "d", "r", 1).unwrap();
        let commit = |worker: &mut rusqlite::Connection, rect: [f32; 4]| -> i64 {
            let snap = crate::facestore::snapshot(worker, ID, gen).unwrap();
            match crate::facestore::commit_scan(
                worker,
                ID,
                &snap,
                &[crate::facestore::NewFace {
                    rect,
                    det_score: 0.9,
                    embedding: vec![0.1; crate::facedet::EMBED_DIM],
                }],
                &[],
                1,
                1,
                &|| true,
            )
            .unwrap()
            {
                crate::facestore::CommitOutcome::Committed(rev) => rev,
                other => panic!("{other:?}"),
            }
        };
        let no_mirror = |_: bool| -> std::io::Result<()> { Ok(()) };

        // Life before the delete: scan A published its chip.
        let rev_a = commit(&mut worker, [0.05, 0.1, 0.2, 0.3]);
        crate::faces::repair_photo(&state, &store, ID);
        let chip_a = state.face_chip_path(ID, 0, rev_a);
        let bytes_a = std::fs::read(&chip_a).unwrap();

        // The privacy delete commits — and its sweep never finishes (crash,
        // or the fallible sweep reported failure). The biometric file
        // survives, exactly as `chip_sweep_pending` records.
        store.delete_face_data(&no_mirror).unwrap();
        assert!(store.chip_sweep_pending().unwrap());
        assert!(chip_a.exists(), "the survivor the review described");

        // The user explicitly re-enables (nothing gates on the pending
        // sweep), the library rescans, and the process dies before baking.
        store.set_faces_enabled(true, &no_mirror).unwrap();
        let rev_b = commit(&mut worker, [0.7, 0.1, 0.2, 0.3]);
        assert_ne!(
            rev_b, rev_a,
            "THE fix: identity is minted from a counter that survives the \
             wipe — under per-photo revisions both scans were revision 1"
        );

        // The production route must 404 the new identity (and heal), never
        // serve the pre-delete survivor as the new detection.
        let resp = handle(&state, &repair, &format!("/face/{ID}/0/{rev_b}"));
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(resp.body().is_empty(), "no pre-delete bytes leak through");
        assert_eq!(rx.try_recv().unwrap(), ID);

        // Repair treats the survivor as stale, publishes B, sweeps A.
        crate::faces::repair_photo(&state, &store, ID);
        let resp = handle(&state, &repair, &format!("/face/{ID}/0/{rev_b}"));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_ne!(resp.body(), &bytes_a, "B's own crop, not the survivor");
        assert!(!chip_a.exists(), "the survivor is finally gone");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_routes_unaffected() {
        let (state, repair, _rx) = setup();
        std::fs::write(state.preview_path_for_id(ID), b"preview").unwrap();
        let resp = handle(&state, &repair, &format!("/preview/{ID}"));
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = handle(&state, &repair, "/preview/zz");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
