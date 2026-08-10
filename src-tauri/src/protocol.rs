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
    // face/{id}/{n}: baked chip, or 404 + a deduplicated repair enqueue — the
    // frontend's 404-retry img pattern picks the chip up when it lands. The
    // repair queue is a dedicated thread; these workers never block on it.
    if route == "face" {
        let mut sub = id_raw.splitn(2, '/');
        let (fid, n_raw) = (sub.next().unwrap_or(""), sub.next().unwrap_or(""));
        let n = n_raw.split('?').next().unwrap_or("");
        let valid = !fid.is_empty()
            && fid.chars().all(|c| c.is_ascii_hexdigit())
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit());
        let Some(n) = valid.then(|| n.parse::<i64>().ok()).flatten() else {
            return respond(StatusCode::NOT_FOUND, "text/plain", Vec::new());
        };
        let p = state.face_chip_path(fid, n);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (
        PreviewState,
        Arc<ChipRepair>,
        crossbeam_channel::Receiver<String>,
    ) {
        let dir = std::env::temp_dir().join(format!(
            "emberproto-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = PreviewState::new(dir, crate::exposure::BlinkiesCfg::default());
        let (repair, rx) = crate::faces::test_repair();
        (state, repair, rx)
    }

    const ID: &str = "00deadbeef00cafe";

    #[test]
    fn face_route_serves_baked_chip() {
        let (state, repair, rx) = setup();
        std::fs::write(state.face_chip_path(ID, 0), b"jpegbytes").unwrap();
        let resp = handle(&state, &repair, &format!("/face/{ID}/0"));
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body(), b"jpegbytes");
        assert!(rx.try_recv().is_err(), "a hit never enqueues repair");
    }

    #[test]
    fn face_route_miss_enqueues_repair_once() {
        let (state, repair, rx) = setup();
        // Retry query (?r=1) must parse like the plain form.
        for path in [format!("/face/{ID}/2"), format!("/face/{ID}/2?r=1")] {
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
            "/face/nothex!/0",         // non-hex id
            &format!("/face/{ID}"),    // missing index
            &format!("/face/{ID}/"),   // empty index
            &format!("/face/{ID}/x2"), // non-numeric index
            "/face//0",                // empty id
        ] {
            let resp = handle(&state, &repair, path);
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        }
        assert!(rx.try_recv().is_err(), "malformed never reaches repair");
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
