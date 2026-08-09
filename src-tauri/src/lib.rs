mod exposure;
mod fastexif;
mod keymap;
mod metadata;
mod preview;
mod protocol;
mod recipes;
mod scanner;
mod settings;
mod store;
mod trash;
mod xmp;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use store::{Delta, FolderState, Store, TrashPayload, TrashedPhoto};

pub struct AppState {
    store: Arc<Store>,
    preview: Arc<preview::PreviewState>,
    exiftool: Arc<xmp::Exiftool>,
    recipes: recipes::RecipeStore,
    proto_tx: crossbeam_channel::Sender<protocol::Job>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoOut {
    pub id: String,
    pub stem: String,
    pub rel_dir: String,
    pub has_jpeg: bool,
    pub has_raf: bool,
    pub rating: u8,
    pub tags: Vec<String>,
    /// On-disk file gone at last rescan; still listed, badged in the UI.
    pub missing: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub photos: Vec<PhotoOut>,
    pub state: FolderState,
    pub trashed_count: usize,
    pub missing_count: usize,
}

#[tauri::command]
fn scan_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    dir: String,
) -> Result<ScanResult, String> {
    let root = PathBuf::from(&dir);
    if !root.is_dir() {
        return Err(format!("Not a folder: {dir}"));
    }
    // Canonical root keeps rel_dirs clean when the folder itself was opened
    // through a symlink; the user-supplied string stays the folders-table key.
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let folder = state.store.open_folder(&dir).map_err(|e| e.to_string())?;
    let entries = scanner::scan(&root);
    // Must run before sync_photos so `existing` sees cleared trash flags.
    reconcile_external_restores(&state.store, folder.folder_id, &entries);

    // Invalidate previews whose source file changed (pixels, not just XMP).
    for e in &entries {
        match state.store.preview_stat(&e.id) {
            Ok(Some((m, s))) if m == e.mtime && s == e.size => {}
            Ok(Some(_)) => {
                for f in [
                    state.preview.preview_path(e),
                    state.preview.thumb_path_for_id(&e.id),
                    state.preview.hist_path_for_id(&e.id),
                    state.preview.mask_path_for_id(&e.id),
                ] {
                    let _ = std::fs::remove_file(f);
                }
                let _ = state.store.clear_preview(&e.id);
            }
            _ => {
                // No validity row → any leftover files are untrusted.
                let p = state.preview.preview_path(e);
                if p.exists() {
                    for f in [
                        p,
                        state.preview.thumb_path_for_id(&e.id),
                        state.preview.hist_path_for_id(&e.id),
                        state.preview.mask_path_for_id(&e.id),
                    ] {
                        let _ = std::fs::remove_file(f);
                    }
                }
            }
        }
    }

    let (existing, _new_ids) = state
        .store
        .sync_photos(folder.folder_id, &entries)
        .map_err(|e| e.to_string())?;

    let mut photos: Vec<PhotoOut> = entries
        .iter()
        .filter(|e| !existing.get(&e.id).map(|r| r.trashed).unwrap_or(false))
        .map(|e| {
            let dto = e.to_dto(&root);
            PhotoOut {
                id: dto.id,
                stem: dto.stem,
                rel_dir: dto.rel_dir,
                has_jpeg: dto.has_jpeg,
                has_raf: dto.has_raf,
                rating: existing.get(&e.id).map(|r| r.rating).unwrap_or(0),
                tags: existing
                    .get(&e.id)
                    .map(|r| r.tags.clone())
                    .unwrap_or_default(),
                missing: false,
            }
        })
        .collect();
    // Missing photos stay in the list with a badge instead of vanishing.
    for m in state
        .store
        .missing_photos(folder.folder_id)
        .unwrap_or_default()
    {
        let rel_dir = std::path::Path::new(&m.dir)
            .strip_prefix(&root)
            .unwrap_or(std::path::Path::new(&m.dir))
            .to_string_lossy()
            .into_owned();
        photos.push(PhotoOut {
            id: m.id,
            stem: m.stem,
            rel_dir,
            has_jpeg: m.has_jpeg,
            has_raf: m.has_raf,
            rating: m.rating,
            tags: m.tags,
            missing: true,
        });
    }
    let trashed_count = state
        .store
        .trashed_list(folder.folder_id)
        .map(|v| v.len())
        .unwrap_or(0);
    let missing_count = state.store.missing_count(folder.folder_id).unwrap_or(0);

    state.preview.set_entries(entries.clone());

    // Background: capture times for all + rating adoption. Adoption considers
    // every photo the user hasn't touched (new, or rating 0 with no journal
    // actions) so stars set in other tools (ApolloOne xattr, C1 XMP) appear
    // even in folders scanned before this feature existed. The SQL guard in
    // adopt_rating makes this safe against clobbering user verdicts.
    let bg_store = state.store.clone();
    let candidates: std::collections::HashSet<String> = entries
        .iter()
        .filter(|e| match existing.get(&e.id) {
            None => true,
            Some(r) => !r.trashed && r.rating == 0,
        })
        .map(|e| e.id.clone())
        .collect();
    std::thread::spawn(move || {
        let meta = fastexif::photo_meta(&entries);
        let times: HashMap<&String, i64> = meta.iter().map(|(id, m)| (id, m.ts)).collect();
        let dims: HashMap<&String, (u32, u32)> = meta
            .iter()
            .filter_map(|(id, m)| Some((id, (m.width?, m.height?))))
            .collect();
        let mut adopted: HashMap<String, u8> = HashMap::new();
        for e in entries.iter().filter(|e| candidates.contains(&e.id)) {
            if let Some(r) = xmp::read_rating(e.jpeg.as_deref(), e.raf.as_deref()) {
                if r > 0 && bg_store.adopt_rating(&e.id, r).unwrap_or(false) {
                    adopted.insert(e.id.clone(), r);
                }
            }
        }
        let _ = app.emit(
            "photo-meta",
            serde_json::json!({ "times": times, "adopted": adopted, "dims": dims }),
        );
    });

    Ok(ScanResult {
        photos,
        state: folder,
        trashed_count,
        missing_count,
    })
}

/// Cached full-metadata dump for a photo, fetching inline when the
/// background sweep hasn't reached it yet.
fn ensure_metadata(state: &AppState, photo_id: &str) -> Result<Option<String>, String> {
    if let Some(j) = state
        .store
        .metadata_json(photo_id)
        .map_err(|e| e.to_string())?
    {
        return Ok(Some(j));
    }
    let Some(entry) = state.preview.entry(photo_id) else {
        return Ok(None);
    };
    let Some(file) = entry.display_file() else {
        return Ok(None);
    };
    let out = state.exiftool.run(&[
        "-j",
        "-G1",
        "-Orientation#",
        "-FileSize#",
        "-All",
        &file.to_string_lossy(),
    ])?;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let json = parsed
        .into_iter()
        .next()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".into());
    let _ = state.store.set_metadata_json(photo_id, &json);
    Ok(Some(json))
}

/// AF point in display-normalized coordinates, from cached MakerNotes.
#[tauri::command]
fn get_focus(
    state: tauri::State<'_, AppState>,
    photo_id: String,
) -> Result<Option<(f64, f64)>, String> {
    match ensure_metadata(&state, &photo_id)? {
        Some(json) => Ok(metadata::focus_point(&json)),
        None => Ok(None),
    }
}

/// Raw grouped metadata JSON for the panel (frontend picks and formats).
#[tauri::command]
fn get_metadata(
    state: tauri::State<'_, AppState>,
    photo_id: String,
) -> Result<Option<String>, String> {
    ensure_metadata(&state, &photo_id)
}

/// Recipe match for one photo (HUD chip + panel).
#[tauri::command]
fn get_recipe(
    state: tauri::State<'_, AppState>,
    photo_id: String,
) -> Result<recipes::RecipeResult, String> {
    let Some(json) = ensure_metadata(&state, &photo_id)? else {
        return Ok(recipes::RecipeResult {
            name: None,
            matches: vec![],
            has_meta: false,
        });
    };
    let meta: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
    let has_meta = meta.as_object().map(|o| o.len() > 1).unwrap_or(false);
    let (name, matches) = state.recipes.match_meta(&meta);
    Ok(recipes::RecipeResult {
        name,
        matches,
        has_meta,
    })
}

/// Recipe label for every photo in the folder that has metadata — powers the
/// recipe filter. Photos the sweep hasn't reached are simply absent.
#[tauri::command]
fn recipe_map(
    state: tauri::State<'_, AppState>,
    folder_id: i64,
) -> Result<HashMap<String, Option<String>>, String> {
    let rows = state
        .store
        .metadata_for_folder(folder_id)
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(id, json)| {
            let meta: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
            (id, state.recipes.match_meta(&meta).0)
        })
        .collect())
}

#[tauri::command]
fn list_recipes(state: tauri::State<'_, AppState>) -> Vec<String> {
    state.recipes.names()
}

/// "Save these settings as a new recipe", pre-filled from the photo's EXIF.
#[tauri::command]
fn save_recipe(
    state: tauri::State<'_, AppState>,
    photo_id: String,
    name: String,
) -> Result<(), String> {
    let Some(json) = ensure_metadata(&state, &photo_id)? else {
        return Err("no metadata for this photo yet".into());
    };
    let meta: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    state.recipes.save_from_meta(&name, &meta)
}

#[tauri::command]
fn get_keymap(app: tauri::AppHandle) -> Result<keymap::KeymapResult, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(keymap::load(&dir))
}

#[tauri::command]
fn set_rating(
    state: tauri::State<'_, AppState>,
    folder_id: i64,
    photo_id: String,
    rating: u8,
) -> Result<(), String> {
    if rating > 5 {
        return Err("rating out of range".into());
    }
    state
        .store
        .set_rating(folder_id, &photo_id, rating)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn trash_photo(
    state: tauri::State<'_, AppState>,
    folder_id: i64,
    photo_id: String,
) -> Result<(), String> {
    let (fid, jpeg, raf) = state
        .store
        .photo_paths(&photo_id)
        .map_err(|e| format!("unknown photo: {e}"))?;
    if fid != folder_id {
        return Err("photo/folder mismatch".into());
    }
    let trashed = trash::trash_pair(jpeg.as_deref(), raf.as_deref())?;
    let payload = TrashPayload {
        jpeg: jpeg.map(|p| p.to_string_lossy().into_owned()),
        raf: raf.map(|p| p.to_string_lossy().into_owned()),
        tjpeg: trashed.jpeg.map(|p| p.to_string_lossy().into_owned()),
        traf: trashed.raf.map(|p| p.to_string_lossy().into_owned()),
    };
    state
        .store
        .record_trash(folder_id, &photo_id, &payload)
        .map_err(|e| format!("CRITICAL: files trashed but journal write failed: {e}"))
}

#[derive(Deserialize)]
struct RatePayload {
    from: u8,
    to: u8,
}

#[derive(Deserialize)]
struct TagsPayload {
    from: Vec<String>,
    to: Vec<String>,
}

/// Mood/role tags (standard XMP dc:subject). Data model only in v1 — the
/// hotkey palette UI is post-v1; journaled and undoable like every verdict.
#[tauri::command]
fn set_tags(
    state: tauri::State<'_, AppState>,
    folder_id: i64,
    photo_id: String,
    tags: Vec<String>,
) -> Result<(), String> {
    state
        .store
        .set_tags(folder_id, &photo_id, &tags)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Restore both halves from the Trash. Tolerates halves the user already put
/// back via Finder: a half whose Trash copy is gone but whose original exists
/// counts as restored. Pair rollback only unwinds moves made by THIS call.
#[tauri::command]
fn get_tag_vocab(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(settings::load_tag_vocab(&dir))
}

fn restore_from_payload(p: &TrashPayload) -> Result<(), String> {
    // (trash copy, original) → Ok(true) if this call moved the file.
    fn restore_half(t: &str, orig: &str) -> Result<bool, String> {
        let (t, orig) = (std::path::Path::new(t), std::path::Path::new(orig));
        if t.exists() {
            trash::restore_file(t, orig)?;
            return Ok(true);
        }
        if orig.exists() {
            return Ok(false); // put back externally — nothing to do
        }
        Err(format!(
            "{} is gone from both the Trash and its folder",
            orig.display()
        ))
    }

    let jpeg_moved_here = match (&p.tjpeg, &p.jpeg) {
        (Some(t), Some(orig)) => restore_half(t, orig)?,
        _ => false,
    };
    if let (Some(t), Some(orig)) = (&p.traf, &p.raf) {
        if let Err(e) = restore_half(t, orig) {
            // Roll back only a JPEG we moved, to keep the pair consistent.
            if jpeg_moved_here {
                if let (Some(t), Some(orig)) = (&p.tjpeg, &p.jpeg) {
                    let _ = trash::move_file(std::path::Path::new(orig), std::path::Path::new(t));
                }
            }
            return Err(e);
        }
    }
    Ok(())
}

/// A photo marked trashed whose files are all back on disk (Finder "Put
/// Back") would otherwise be invisible forever: the scan filter drops
/// trashed rows and the Trash panel's restore has nothing left to move.
/// Journal a restore for exactly that state. Bookkeeping only — a photo
/// with any half still in the Trash stays in the panel, where the
/// half-aware restore completes it explicitly.
fn reconcile_external_restores(
    store: &Store,
    folder_id: i64,
    entries: &[std::sync::Arc<scanner::PhotoEntry>],
) {
    let trashed_ids: std::collections::HashSet<String> = store
        .trashed_list(folder_id)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.id)
        .collect();
    if trashed_ids.is_empty() {
        return;
    }
    let gone = |t: &Option<String>| {
        t.as_deref()
            .is_none_or(|p| !std::path::Path::new(p).exists())
    };
    let here = |o: &Option<String>| {
        o.as_deref()
            .is_none_or(|p| std::path::Path::new(p).exists())
    };
    for e in entries.iter().filter(|e| trashed_ids.contains(&e.id)) {
        let Ok(p) = store.trash_info(&e.id) else {
            continue;
        };
        if gone(&p.tjpeg) && gone(&p.traf) && here(&p.jpeg) && here(&p.raf) {
            let _ = store.record_restore(folder_id, &e.id, &p);
        }
    }
}

fn retrash_from_payload(p: &TrashPayload) -> Result<TrashPayload, String> {
    let trashed = trash::trash_pair(
        p.jpeg.as_deref().map(std::path::Path::new),
        p.raf.as_deref().map(std::path::Path::new),
    )?;
    Ok(TrashPayload {
        jpeg: p.jpeg.clone(),
        raf: p.raf.clone(),
        tjpeg: trashed.jpeg.map(|q| q.to_string_lossy().into_owned()),
        traf: trashed.raf.map(|q| q.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
fn undo(state: tauri::State<'_, AppState>, folder_id: i64) -> Result<Option<Delta>, String> {
    let Some(act) = state
        .store
        .peek_undo(folder_id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let delta = match act.kind.as_str() {
        "rate" => {
            let p: RatePayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            state
                .store
                .finish_flip(act.seq, true, &act.photo_id, Some(p.from), None, None, None)
                .map_err(|e| e.to_string())?;
            Delta {
                photo_id: act.photo_id,
                kind: "rate".into(),
                rating: Some(p.from),
                tags: None,
                trashed: None,
                error: None,
            }
        }
        "tags" => {
            let p: TagsPayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            state
                .store
                .finish_flip(
                    act.seq,
                    true,
                    &act.photo_id,
                    None,
                    Some(&p.from),
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?;
            Delta {
                photo_id: act.photo_id,
                kind: "tags".into(),
                rating: None,
                tags: Some(p.from),
                trashed: None,
                error: None,
            }
        }
        "trash" => {
            let p: TrashPayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            let err = restore_from_payload(&p).err();
            state
                .store
                .finish_flip(
                    act.seq,
                    true,
                    &act.photo_id,
                    None,
                    None,
                    Some((&p, false)),
                    None,
                )
                .map_err(|e| e.to_string())?;
            Delta {
                photo_id: act.photo_id,
                kind: "trash".into(),
                rating: None,
                tags: None,
                trashed: Some(false),
                error: err,
            }
        }
        "restore" => {
            let p: TrashPayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            match retrash_from_payload(&p) {
                Ok(new_p) => {
                    let json = serde_json::to_string(&new_p).unwrap();
                    state
                        .store
                        .finish_flip(
                            act.seq,
                            true,
                            &act.photo_id,
                            None,
                            None,
                            Some((&new_p, true)),
                            Some(&json),
                        )
                        .map_err(|e| e.to_string())?;
                    Delta {
                        photo_id: act.photo_id,
                        kind: "restore".into(),
                        rating: None,
                        tags: None,
                        trashed: Some(true),
                        error: None,
                    }
                }
                Err(e) => Delta {
                    photo_id: act.photo_id,
                    kind: "restore".into(),
                    rating: None,
                    tags: None,
                    trashed: None,
                    error: Some(e),
                },
            }
        }
        other => return Err(format!("unknown action kind: {other}")),
    };
    Ok(Some(delta))
}

#[tauri::command]
fn redo(state: tauri::State<'_, AppState>, folder_id: i64) -> Result<Option<Delta>, String> {
    let Some(act) = state
        .store
        .peek_redo(folder_id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let delta = match act.kind.as_str() {
        "rate" => {
            let p: RatePayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            state
                .store
                .finish_flip(act.seq, false, &act.photo_id, Some(p.to), None, None, None)
                .map_err(|e| e.to_string())?;
            Delta {
                photo_id: act.photo_id,
                kind: "rate".into(),
                rating: Some(p.to),
                tags: None,
                trashed: None,
                error: None,
            }
        }
        "tags" => {
            let p: TagsPayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            state
                .store
                .finish_flip(act.seq, false, &act.photo_id, None, Some(&p.to), None, None)
                .map_err(|e| e.to_string())?;
            Delta {
                photo_id: act.photo_id,
                kind: "tags".into(),
                rating: None,
                tags: Some(p.to),
                trashed: None,
                error: None,
            }
        }
        "trash" => {
            let p: TrashPayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            match retrash_from_payload(&p) {
                Ok(new_p) => {
                    let json = serde_json::to_string(&new_p).unwrap();
                    state
                        .store
                        .finish_flip(
                            act.seq,
                            false,
                            &act.photo_id,
                            None,
                            None,
                            Some((&new_p, true)),
                            Some(&json),
                        )
                        .map_err(|e| e.to_string())?;
                    Delta {
                        photo_id: act.photo_id,
                        kind: "trash".into(),
                        rating: None,
                        tags: None,
                        trashed: Some(true),
                        error: None,
                    }
                }
                Err(e) => Delta {
                    photo_id: act.photo_id,
                    kind: "trash".into(),
                    rating: None,
                    tags: None,
                    trashed: None,
                    error: Some(e),
                },
            }
        }
        "restore" => {
            let p: TrashPayload = serde_json::from_str(&act.payload).map_err(|e| e.to_string())?;
            let err = restore_from_payload(&p).err();
            state
                .store
                .finish_flip(
                    act.seq,
                    false,
                    &act.photo_id,
                    None,
                    None,
                    Some((&p, false)),
                    None,
                )
                .map_err(|e| e.to_string())?;
            Delta {
                photo_id: act.photo_id,
                kind: "restore".into(),
                rating: None,
                tags: None,
                trashed: Some(false),
                error: err,
            }
        }
        other => return Err(format!("unknown action kind: {other}")),
    };
    Ok(Some(delta))
}

#[tauri::command]
fn restore_photo(
    state: tauri::State<'_, AppState>,
    folder_id: i64,
    photo_id: String,
) -> Result<(), String> {
    let payload = state
        .store
        .trash_info(&photo_id)
        .map_err(|e| format!("not in trash: {e}"))?;
    restore_from_payload(&payload)?;
    state
        .store
        .record_restore(folder_id, &photo_id, &payload)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn trashed_list(
    state: tauri::State<'_, AppState>,
    folder_id: i64,
) -> Result<Vec<TrashedPhoto>, String> {
    state
        .store
        .trashed_list(folder_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_view(
    state: tauri::State<'_, AppState>,
    folder_id: i64,
    sort: String,
    reverse: bool,
    filter: String,
    cursor: Option<String>,
) -> Result<(), String> {
    state
        .store
        .save_view(folder_id, &sort, reverse, &filter, cursor.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn xmp_pending(state: tauri::State<'_, AppState>) -> Result<store::XmpStatus, String> {
    state.store.xmp_status().map_err(|e| e.to_string())
}

/// Re-arm permanently-failed XMP writes; the queue worker picks them up
/// within its poll interval. Returns the refreshed status immediately so the
/// HUD doesn't wait on the worker.
#[tauri::command]
fn retry_xmp_errors(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<store::XmpStatus, String> {
    state.store.xmp_retry_errors().map_err(|e| e.to_string())?;
    let status = state.store.xmp_status().map_err(|e| e.to_string())?;
    let _ = app.emit("xmp-pending", &status);
    Ok(status)
}

fn harness_hidden() -> bool {
    std::env::var("EMBER_HIDDEN")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Bring the window frontmost — rAF-based glass-time measurement is only
/// meaningful when the window isn't occluded (WebKit throttles hidden windows).
/// No-op for hidden harness runs: they must never steal focus or keystrokes
/// from a live user session.
#[tauri::command]
fn focus_window(app: tauri::AppHandle) {
    if harness_hidden() {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn set_cursor(state: tauri::State<'_, AppState>, id: String) {
    state.preview.set_anchor(&id);
}

#[tauri::command]
fn set_order(state: tauri::State<'_, AppState>, ids: Vec<String>) {
    state.preview.set_order(&ids);
}

#[tauri::command]
fn save_perf_report(app: tauri::AppHandle, report: serde_json::Value) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("perf-reports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("report-{ts}.json"));
    let pretty = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&path, pretty).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Dev/test hooks: EMBER_OPEN=<folder> auto-opens it on launch;
/// EMBER_STORM=1 runs the flip-storm harness and exits;
/// EMBER_CHAOS=1 rates/flips continuously (kill -9 target);
/// EMBER_VERIFY=1 dumps persisted verdicts after reopen.
#[tauri::command]
fn dev_flags() -> serde_json::Value {
    let flag = |k: &str| std::env::var(k).map(|v| v == "1").unwrap_or(false);
    serde_json::json!({
        "open": std::env::var("EMBER_OPEN").ok(),
        "storm": flag("EMBER_STORM"),
        "chaos": flag("EMBER_CHAOS"),
        "verify": flag("EMBER_VERIFY"),
        "resumeTest": flag("EMBER_RESUME_TEST"),
        "zoomTest": flag("EMBER_ZOOMTEST"),
    })
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Webview console/error forwarding so headless runs are debuggable from stdout.
#[tauri::command]
fn frontend_log(level: String, msg: String) {
    eprintln!("[webview:{level}] {msg}");
}

/// One-time codename migration (ApolloTwo → Ember, 2026-08): adopt the old
/// app-data dir so no verdict is left behind. The old identifier is named
/// deliberately — remove this whole function after a release or two.
fn migrate_codename_data(new_dir: &std::path::Path) {
    let old_dir = new_dir.with_file_name("com.cleung.apollotwo");
    if !old_dir.exists() || new_dir.join("ember.sqlite3").exists() {
        return;
    }
    let _ = std::fs::create_dir_all(new_dir);
    // The journal gets the new stem. WAL/SHM sidecars exist after an unclean
    // shutdown and MUST travel with the DB or recent commits are lost.
    for (from, to) in [
        ("apollotwo.sqlite3", "ember.sqlite3"),
        ("apollotwo.sqlite3-wal", "ember.sqlite3-wal"),
        ("apollotwo.sqlite3-shm", "ember.sqlite3-shm"),
    ] {
        let src = old_dir.join(from);
        if src.exists() {
            let _ = std::fs::rename(&src, new_dir.join(to));
        }
    }
    // User config keeps its names; never clobber files already in the new dir.
    for name in [
        "keymap.toml",
        "settings.toml",
        "recipes.toml",
        "perf-reports",
    ] {
        let (src, dst) = (old_dir.join(name), new_dir.join(name));
        if src.exists() && !dst.exists() {
            let _ = std::fs::rename(&src, dst);
        }
    }
    // Succeeds only if empty — anything unexpected stays for manual review.
    let _ = std::fs::remove_dir(&old_dir);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    // e2e builds (cargo feature `e2e`) embed the WebdriverIO plugins; the
    // server only listens when the harness spawns us with TAURI_WEBDRIVER_PORT.
    #[cfg(feature = "e2e")]
    let builder = builder
        .plugin(tauri_plugin_wdio::init())
        .plugin(tauri_plugin_wdio_webdriver::init());
    builder
        .setup(|app| {
            // Harness runs (EMBER_HIDDEN=1) keep their window invisible so
            // automated storms can never hijack a window the user is culling in.
            if harness_hidden() {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            let data_dir = app.path().app_data_dir()?;
            // Production identifier only: the e2e harness runs under
            // com.cleung.ember.e2e, whose sandbox must never adopt (move!)
            // journals that belong to the real app.
            if app.config().identifier == "com.cleung.ember" {
                migrate_codename_data(&data_dir);
            }
            std::fs::create_dir_all(&data_dir)?;
            let store = Arc::new(
                Store::new(&data_dir.join("ember.sqlite3"))
                    .map_err(|e| std::io::Error::other(e.to_string()))?,
            );
            let cache_dir = app.path().app_cache_dir()?.join("previews");
            std::fs::create_dir_all(&cache_dir)?;
            let blinkies = settings::load(&data_dir);
            let preview = Arc::new(preview::PreviewState::new(cache_dir, blinkies));
            preview::spawn_workers(preview.clone(), store.clone(), 6);
            let (tx, rx) = protocol::channel();
            protocol::spawn_workers(preview.clone(), rx, 6);
            let recipe_store = recipes::RecipeStore::new(&data_dir);
            let exiftool = Arc::new(xmp::Exiftool::new());
            xmp::spawn_queue_worker(app.handle().clone(), store.clone(), exiftool.clone());
            metadata::spawn_worker(store.clone(), preview.clone(), exiftool.clone());
            app.manage(AppState {
                store,
                preview,
                exiftool,
                recipes: recipe_store,
                proto_tx: tx,
            });
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("photo", |ctx, request, responder| {
            let state = ctx.app_handle().state::<AppState>();
            let job = protocol::Job {
                path: request.uri().path().to_string(),
                responder,
            };
            // Send never blocks (unbounded); workers bound actual concurrency.
            let _ = state.proto_tx.send(job);
        })
        .invoke_handler(tauri::generate_handler![
            scan_folder,
            set_rating,
            set_tags,
            trash_photo,
            undo,
            redo,
            restore_photo,
            trashed_list,
            save_view,
            xmp_pending,
            retry_xmp_errors,
            get_keymap,
            get_tag_vocab,
            get_focus,
            get_metadata,
            get_recipe,
            recipe_map,
            list_recipes,
            save_recipe,
            focus_window,
            set_cursor,
            set_order,
            save_perf_report,
            dev_flags,
            quit_app,
            frontend_log
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| {
            // Both quit paths (quit_app and last-window-close) funnel through
            // ExitRequested: block until the XMP queue drains so acknowledged
            // verdicts reach their files before the process dies (spec §7).
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(s) = handle.try_state::<AppState>() {
                    let _ = xmp::drain_blocking(&s.store, std::time::Duration::from_secs(5));
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pstr(p: &std::path::Path) -> Option<String> {
        Some(p.to_string_lossy().into_owned())
    }

    #[test]
    fn restore_tolerates_externally_put_back_halves() {
        let base = std::env::temp_dir().join(format!("ember-rst-{}", std::process::id()));
        let (dir, tr) = (base.join("photos"), base.join("trash"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&tr).unwrap();
        let (jpeg, raf) = (dir.join("A.JPG"), dir.join("A.RAF"));
        let (tjpeg, traf) = (tr.join("A.JPG"), tr.join("A.RAF"));
        let payload = TrashPayload {
            jpeg: pstr(&jpeg),
            raf: pstr(&raf),
            tjpeg: pstr(&tjpeg),
            traf: pstr(&traf),
        };

        // JPEG put back via Finder (orig exists, trash copy gone); RAF in trash.
        std::fs::write(&jpeg, b"j").unwrap();
        std::fs::write(&traf, b"r").unwrap();
        restore_from_payload(&payload).expect("must complete the RAF half");
        assert!(jpeg.exists() && raf.exists(), "pair fully on disk");
        assert!(!traf.exists(), "RAF left the trash");

        // Second restore of the same payload is a no-op, not an error.
        restore_from_payload(&payload).expect("idempotent");

        // A half missing from BOTH places is a loud error.
        std::fs::remove_file(&raf).unwrap();
        assert!(restore_from_payload(&payload).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn scan_reconciles_fully_put_back_photos() {
        let base = std::env::temp_dir().join(format!("ember-rec-{}", std::process::id()));
        let dir = base.join("photos");
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&base.join("t.sqlite3")).unwrap();
        let f = store.open_folder(dir.to_str().unwrap()).unwrap();

        let jpeg = dir.join("DSCF0001.JPG");
        std::fs::write(&jpeg, b"j").unwrap();
        let entries = scanner::scan(&dir);
        assert_eq!(entries.len(), 1);
        store.sync_photos(f.folder_id, &entries).unwrap();

        // Trash it (payload says the copy lives at tjpeg), then simulate
        // Finder Put Back: original exists, trash copy gone.
        let tjpeg = base.join("trash-DSCF0001.JPG");
        let payload = TrashPayload {
            jpeg: pstr(&jpeg),
            raf: None,
            tjpeg: pstr(&tjpeg),
            traf: None,
        };
        store
            .record_trash(f.folder_id, &entries[0].id, &payload)
            .unwrap();
        assert_eq!(store.trashed_list(f.folder_id).unwrap().len(), 1);

        reconcile_external_restores(&store, f.folder_id, &entries);
        assert!(
            store.trashed_list(f.folder_id).unwrap().is_empty(),
            "fully put-back photo must leave the trash bookkeeping"
        );

        // A photo whose trash copy still exists is NOT reconciled.
        std::fs::write(&tjpeg, b"copy").unwrap();
        store
            .record_trash(f.folder_id, &entries[0].id, &payload)
            .unwrap();
        reconcile_external_restores(&store, f.folder_id, &entries);
        assert_eq!(
            store.trashed_list(f.folder_id).unwrap().len(),
            1,
            "half-in-trash stays in the panel for explicit restore"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn codename_migration_moves_journal_and_never_clobbers() {
        let base = std::env::temp_dir().join(format!("ember-mig-{}", std::process::id()));
        let old_dir = base.join("com.cleung.apollotwo");
        let new_dir = base.join("com.cleung.ember");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("apollotwo.sqlite3"), b"journal").unwrap();
        std::fs::write(old_dir.join("apollotwo.sqlite3-wal"), b"wal").unwrap();
        std::fs::write(old_dir.join("recipes.toml"), b"old recipes").unwrap();
        // The new dir already has a recipes.toml (e.g. an early Ember launch):
        // migration must not clobber it.
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(new_dir.join("recipes.toml"), b"new recipes").unwrap();

        migrate_codename_data(&new_dir);

        assert_eq!(
            std::fs::read(new_dir.join("ember.sqlite3")).unwrap(),
            b"journal"
        );
        assert_eq!(
            std::fs::read(new_dir.join("ember.sqlite3-wal")).unwrap(),
            b"wal",
            "WAL must travel with the DB"
        );
        assert_eq!(
            std::fs::read(new_dir.join("recipes.toml")).unwrap(),
            b"new recipes",
            "existing files never clobbered"
        );
        assert!(
            old_dir.join("recipes.toml").exists(),
            "unclaimed leftovers stay for manual review"
        );

        // Second run is a no-op (guarded on ember.sqlite3 existing).
        std::fs::write(old_dir.join("apollotwo.sqlite3"), b"stale").unwrap();
        migrate_codename_data(&new_dir);
        assert_eq!(
            std::fs::read(new_dir.join("ember.sqlite3")).unwrap(),
            b"journal"
        );

        std::fs::remove_dir_all(&base).unwrap();
    }
}
