use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use tauri::http::{header, Response, StatusCode};
use tauri::UriSchemeResponder;

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

fn handle(state: &PreviewState, path: &str) -> Response<Vec<u8>> {
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let (route, id_raw) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
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
pub fn spawn_workers(state: Arc<PreviewState>, rx: Receiver<Job>, n: usize) {
    for i in 0..n {
        let state = state.clone();
        let rx = rx.clone();
        std::thread::Builder::new()
            .name(format!("photo-proto-{i}"))
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job.responder.respond(handle(&state, &job.path));
                }
            })
            .expect("spawn protocol worker");
    }
}
