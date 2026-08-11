//! Pure face-detection math — no ONNX/ort types anywhere, so every piece is
//! unit-testable without model files. Coordinate spaces: YuNet consumes a
//! 640×640 letterboxed BGR image (the 2023mar export has a FIXED input shape,
//! verified against the pinned ONNX); detections come back in that 640-space
//! and are mapped to preview pixels by dividing out the letterbox scale.

use image::RgbImage;

/// YuNet 2023mar fixed input edge (both dimensions). The export's input
/// tensor is [1, 3, 640, 640]; output grids are (640/stride)² per stride.
pub const YUNET_INPUT: usize = 640;
pub const YUNET_STRIDES: [usize; 3] = [8, 16, 32];
/// IoU threshold OpenCV's FaceDetectorYN ships for YuNet post-processing.
pub const YUNET_NMS_IOU: f32 = 0.3;

/// SFace consumes a 112×112 landmark-aligned crop; embeddings are 128-d.
pub const SFACE_INPUT: usize = 112;
pub const EMBED_DIM: usize = 128;

/// ArcFace/SFace canonical 5-point template in 112×112 space, same order as
/// YuNet landmarks: left-in-image eye, right eye, nose tip, left mouth
/// corner, right mouth corner (OpenCV FaceRecognizerSF::alignCrop).
const TEMPLATE_112: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

#[derive(Debug, Clone)]
pub struct Detection {
    /// x, y, w, h in 640-letterbox pixels.
    pub rect: [f32; 4],
    pub score: f32,
    /// 5 landmarks, (x, y) in 640-letterbox pixels.
    pub landmarks: [[f32; 2]; 5],
}

/// Letterbox geometry: uniform scale into the top-left of a 640×640 canvas,
/// black padding right/bottom. `scale` maps preview px → 640-space px.
#[derive(Debug, Clone, Copy)]
pub struct Letterbox {
    pub scale: f32,
    pub scaled_w: u32,
    pub scaled_h: u32,
}

pub fn letterbox(w: u32, h: u32) -> Letterbox {
    let scale = (YUNET_INPUT as f32 / w as f32).min(YUNET_INPUT as f32 / h as f32);
    Letterbox {
        scale,
        scaled_w: ((w as f32 * scale).round() as u32).clamp(1, YUNET_INPUT as u32),
        scaled_h: ((h as f32 * scale).round() as u32).clamp(1, YUNET_INPUT as u32),
    }
}

/// Decode one YuNet output stride: cls/obj are sigmoid scores per grid cell,
/// bbox is (dx, dy, log w, log h) relative to the cell, kps are 5 offset
/// pairs. Mirrors OpenCV FaceDetectorYN's post-processing exactly.
pub fn decode_stride(
    cls: &[f32],
    obj: &[f32],
    bbox: &[f32],
    kps: &[f32],
    stride: usize,
    min_score: f32,
) -> Vec<Detection> {
    let cols = YUNET_INPUT / stride;
    let cells = cols * cols;
    debug_assert_eq!(cls.len(), cells);
    debug_assert_eq!(bbox.len(), cells * 4);
    debug_assert_eq!(kps.len(), cells * 10);
    let mut out = Vec::new();
    for idx in 0..cells {
        let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
        if score < min_score {
            continue;
        }
        let (r, c) = (idx / cols, idx % cols);
        let s = stride as f32;
        let cx = (c as f32 + bbox[idx * 4]) * s;
        let cy = (r as f32 + bbox[idx * 4 + 1]) * s;
        let w = bbox[idx * 4 + 2].exp() * s;
        let h = bbox[idx * 4 + 3].exp() * s;
        let mut landmarks = [[0f32; 2]; 5];
        for (p, lm) in landmarks.iter_mut().enumerate() {
            lm[0] = (c as f32 + kps[idx * 10 + p * 2]) * s;
            lm[1] = (r as f32 + kps[idx * 10 + p * 2 + 1]) * s;
        }
        out.push(Detection {
            rect: [cx - w / 2.0, cy - h / 2.0, w, h],
            score,
            landmarks,
        });
    }
    out
}

pub fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = (a[0] + a[2]).min(b[0] + b[2]);
    let y2 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Greedy NMS, highest score first.
pub fn nms(mut dets: Vec<Detection>, iou_thresh: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for d in dets {
        if kept.iter().all(|k| iou(&k.rect, &d.rect) < iou_thresh) {
            kept.push(d);
        }
    }
    kept
}

/// 640-space detection → normalized [0,1] display-space rect + preview-pixel
/// landmarks, clamped to the preview bounds.
pub fn to_preview_space(
    det: &Detection,
    lb: Letterbox,
    pw: u32,
    ph: u32,
) -> ([f32; 4], [[f32; 2]; 5]) {
    let (pw_f, ph_f) = (pw as f32, ph as f32);
    let inv = 1.0 / lb.scale;
    let x = (det.rect[0] * inv).clamp(0.0, pw_f);
    let y = (det.rect[1] * inv).clamp(0.0, ph_f);
    let w = (det.rect[2] * inv).min(pw_f - x);
    let h = (det.rect[3] * inv).min(ph_f - y);
    let rect = [x / pw_f, y / ph_f, w / pw_f, h / ph_f];
    let mut lms = [[0f32; 2]; 5];
    for (i, lm) in det.landmarks.iter().enumerate() {
        lms[i] = [
            (lm[0] * inv).clamp(0.0, pw_f),
            (lm[1] * inv).clamp(0.0, ph_f),
        ];
    }
    (rect, lms)
}

/// Least-squares similarity transform (rotation + uniform scale +
/// translation) mapping the 5 detected landmarks onto the 112×112 SFace
/// template — the no-reflection case of Umeyama's method, which is the only
/// case reachable from real (non-mirrored) face landmarks.
/// Returns [a, b, tx, c, d, ty] for dst = [[a, b], [c, d]]·src + [tx, ty].
pub fn umeyama_112(landmarks: &[[f32; 2]; 5]) -> [f32; 6] {
    let n = landmarks.len() as f32;
    let (mut mx, mut my, mut mu, mut mv) = (0f32, 0f32, 0f32, 0f32);
    for (src, dst) in landmarks.iter().zip(TEMPLATE_112.iter()) {
        mx += src[0];
        my += src[1];
        mu += dst[0];
        mv += dst[1];
    }
    let (mx, my, mu, mv) = (mx / n, my / n, mu / n, mv / n);
    let (mut num_cos, mut num_sin, mut var) = (0f32, 0f32, 0f32);
    for (src, dst) in landmarks.iter().zip(TEMPLATE_112.iter()) {
        let (x, y) = (src[0] - mx, src[1] - my);
        let (u, v) = (dst[0] - mu, dst[1] - mv);
        num_cos += x * u + y * v;
        num_sin += x * v - y * u;
        var += x * x + y * y;
    }
    if var <= f32::EPSILON {
        // Degenerate (all landmarks coincide): identity centered on template.
        return [1.0, 0.0, mu - mx, 0.0, 1.0, mv - my];
    }
    let p = num_cos / var; // s·cosθ
    let q = num_sin / var; // s·sinθ
    [p, -q, mu - (p * mx - q * my), q, p, mv - (q * mx + p * my)]
}

/// Invert an affine [a, b, tx, c, d, ty].
fn invert_affine(m: &[f32; 6]) -> [f32; 6] {
    let det = m[0] * m[4] - m[1] * m[3];
    let det = if det.abs() < f32::EPSILON { 1.0 } else { det };
    let (ia, ib, ic, id) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
    [
        ia,
        ib,
        -(ia * m[2] + ib * m[5]),
        ic,
        id,
        -(ic * m[2] + id * m[5]),
    ]
}

/// Inverse-warp a 112×112 aligned crop out of the preview via bilinear
/// sampling, emitted as BGR CHW f32 (0–255) — exactly what SFace consumes
/// (OpenCV blobFromImage defaults: no scaling, BGR order).
pub fn warp_112(img: &RgbImage, to_template: &[f32; 6]) -> Vec<f32> {
    let inv = invert_affine(to_template);
    let (w, h) = img.dimensions();
    let (wf, hf) = (w as f32, h as f32);
    let mut out = vec![0f32; 3 * SFACE_INPUT * SFACE_INPUT];
    let plane = SFACE_INPUT * SFACE_INPUT;
    for dy in 0..SFACE_INPUT {
        for dx in 0..SFACE_INPUT {
            let (dxf, dyf) = (dx as f32, dy as f32);
            let sx = inv[0] * dxf + inv[1] * dyf + inv[2];
            let sy = inv[3] * dxf + inv[4] * dyf + inv[5];
            if sx < 0.0 || sy < 0.0 || sx > wf - 1.0 || sy > hf - 1.0 {
                continue; // out of bounds → black
            }
            let (x0, y0) = (sx.floor() as u32, sy.floor() as u32);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let p00 = img.get_pixel(x0, y0).0;
            let p10 = img.get_pixel(x1, y0).0;
            let p01 = img.get_pixel(x0, y1).0;
            let p11 = img.get_pixel(x1, y1).0;
            let o = dy * SFACE_INPUT + dx;
            for ch in 0..3 {
                let top = p00[ch] as f32 * (1.0 - fx) + p10[ch] as f32 * fx;
                let bot = p01[ch] as f32 * (1.0 - fx) + p11[ch] as f32 * fx;
                // RGB source channel ch → BGR plane (2 - ch).
                out[(2 - ch) * plane + o] = top * (1.0 - fy) + bot * fy;
            }
        }
    }
    out
}

/// Letterboxed BGR CHW f32 (0–255) input tensor for YuNet from an
/// already-resized image placed at the top-left of the 640×640 canvas.
pub fn yunet_tensor(scaled: &RgbImage) -> Vec<f32> {
    let mut out = vec![0f32; 3 * YUNET_INPUT * YUNET_INPUT];
    let plane = YUNET_INPUT * YUNET_INPUT;
    let (w, h) = scaled.dimensions();
    for y in 0..h.min(YUNET_INPUT as u32) {
        for x in 0..w.min(YUNET_INPUT as u32) {
            let p = scaled.get_pixel(x, y).0;
            let o = y as usize * YUNET_INPUT + x as usize;
            out[o] = p[2] as f32; // B
            out[plane + o] = p[1] as f32; // G
            out[2 * plane + o] = p[0] as f32; // R
        }
    }
    out
}

pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        0.0
    } else {
        dot / (na * nb)
    }
}

// ---------- embedding storage ----------

/// 128 × f32 little-endian — the `faces.embedding` BLOB format.
pub fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn blob_to_embedding(b: &[u8]) -> Option<Vec<f32>> {
    if b.len() != EMBED_DIM * 4 {
        return None;
    }
    Some(
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

// ---------- clustering (People panel "Unnamed") ----------

/// Deterministic greedy clustering: faces in input order, each joining the
/// first existing cluster whose *first member* (its anchor) is within
/// `threshold` cosine similarity, else founding a new cluster. Anchor
/// comparison (not centroid) keeps results order-stable and cheap; input is
/// sorted by the caller (det_score desc) so anchors are strong faces.
pub fn cluster_greedy(embeddings: &[Vec<f32>], threshold: f32) -> Vec<Vec<usize>> {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for (i, emb) in embeddings.iter().enumerate() {
        let found = clusters
            .iter_mut()
            .find(|c| cosine(&embeddings[c[0]], emb) >= threshold);
        match found {
            Some(c) => c.push(i),
            None => clusters.push(vec![i]),
        }
    }
    clusters
}

// ---------- auto-recognition (Slice B) ----------

/// Pick up to `max` diverse exemplars, greedy max-min: seed with the first
/// embedding (caller orders by det_score, so the seed is a strong face),
/// then repeatedly add the embedding farthest (min-cosine) from the picks.
/// Diversity beats redundancy: five near-identical frontal shots recognize
/// worse than frontal + profile + glasses + shade + distance.
pub fn select_exemplars(embeddings: &[Vec<f32>], max: usize) -> Vec<usize> {
    if embeddings.is_empty() || max == 0 {
        return Vec::new();
    }
    let mut picked = vec![0usize];
    while picked.len() < max.min(embeddings.len()) {
        let next = (0..embeddings.len())
            .filter(|i| !picked.contains(i))
            .max_by(|&a, &b| {
                let worst = |i: usize| {
                    picked
                        .iter()
                        .map(|&p| cosine(&embeddings[p], &embeddings[i]))
                        .fold(f32::NEG_INFINITY, f32::max)
                };
                // Farthest = smallest max-similarity to the current picks.
                worst(b).total_cmp(&worst(a))
            });
        match next {
            Some(i) => picked.push(i),
            None => break,
        }
    }
    picked
}

/// A confirmed face that resembles NONE of the person's other confirmed
/// faces is a mislabel or a junk detection (back of a head, blur) — and
/// max-min exemplar selection would otherwise LOVE it (outliers are maximally
/// "diverse"). Below the OpenCV same-identity floor (0.363) it cannot be the
/// same person as the rest, so it never becomes a reference.
pub const EXEMPLAR_OUTLIER_FLOOR: f32 = 0.35;

/// Indices safe to use as exemplar candidates. Needs enough evidence to
/// judge (n ≥ 4); bails out to "keep all" if it would reject nearly
/// everything (a person whose faces are genuinely all different is beyond
/// automatic help and the thresholds still protect matching).
pub fn filter_exemplar_outliers(embeddings: &[Vec<f32>]) -> Vec<usize> {
    let n = embeddings.len();
    if n < 4 {
        return (0..n).collect();
    }
    let kept: Vec<usize> = (0..n)
        .filter(|&i| {
            embeddings
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, e)| cosine(&embeddings[i], e))
                .fold(f32::NEG_INFINITY, f32::max)
                >= EXEMPLAR_OUTLIER_FLOOR
        })
        .collect();
    if kept.len() < 2 {
        (0..n).collect()
    } else {
        kept
    }
}

pub struct PersonProtos {
    pub person_id: i64,
    pub exemplars: Vec<Vec<f32>>,
}

/// Auto-assignment that cannot cascade: score = max cosine vs a person's
/// (user-confirmed-only) exemplars; assign only when the best clears
/// `threshold` AND leads the runner-up by `margin`. Rejected persons are
/// ABSOLUTE — excluded from best and runner-up alike ("not X" means X's
/// score is meaningless for this face, not merely losing).
pub fn match_face(
    embedding: &[f32],
    protos: &[PersonProtos],
    rejected: &std::collections::HashSet<i64>,
    threshold: f32,
    margin: f32,
) -> Option<(i64, f32)> {
    let mut best: Option<(i64, f32)> = None;
    let mut second: f32 = f32::NEG_INFINITY;
    for p in protos {
        if rejected.contains(&p.person_id) || p.exemplars.is_empty() {
            continue;
        }
        let score = p
            .exemplars
            .iter()
            .map(|e| cosine(embedding, e))
            .fold(f32::NEG_INFINITY, f32::max);
        match best {
            Some((_, b)) if score > b => {
                second = b;
                best = Some((p.person_id, score));
            }
            Some((_, b)) => second = second.max(score.min(b)),
            None => best = Some((p.person_id, score)),
        }
    }
    let (person, score) = best?;
    if score < threshold {
        return None;
    }
    if second > f32::NEG_INFINITY && score - second < margin {
        return None; // too close to call — leave it to the human
    }
    Some((person, score))
}

/// One person cannot be two faces in the same photo. Given candidate
/// (face, person, score) triples for a single photo, keep only the
/// best-scoring face per person — and drop any candidate for a person the
/// photo already has (a carried or user assignment always wins).
/// Input order is preserved for the survivors.
pub fn one_face_per_person(
    mut candidates: Vec<(usize, i64, f32)>,
    already_present: &std::collections::HashSet<i64>,
) -> Vec<(usize, i64, f32)> {
    candidates.retain(|(_, person, _)| !already_present.contains(person));
    // Strongest first, then take the first occurrence of each person.
    candidates.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<(usize, i64, f32)> = candidates
        .into_iter()
        .filter(|(_, person, _)| seen.insert(*person))
        .collect();
    kept.sort_by_key(|(idx, _, _)| *idx);
    kept
}

// ---------- carry-over correspondence (rescan / model change) ----------

/// Confident-match thresholds for transferring user state old→new rows.
/// Same-photo re-detections overlap heavily and embed nearly identically, so
/// both bars are conservative without being fragile.
pub const CARRY_IOU: f32 = 0.5;
pub const CARRY_COSINE: f32 = 0.5;

pub struct CarrySide<'a> {
    /// Normalized display-space rect.
    pub rect: [f32; 4],
    /// None when the embedding must not be compared (cross-generation).
    pub embedding: Option<&'a [f32]>,
}

/// Match old detections to new ones for the carry-over transaction.
/// Same-generation: rect-IoU AND embedding similarity must both clear their
/// bars. Cross-generation (either side's embedding withheld by the caller):
/// geometric correspondence only — embeddings from different model
/// generations are never comparable. Ambiguity (an old face whose best new
/// match is already claimed by a stronger old face) drops the match: that
/// face's state lapses to Unnamed by design.
pub fn match_carry_over(old: &[CarrySide], new: &[CarrySide]) -> Vec<(usize, usize)> {
    // Best new candidate per old face, then greedy by IoU strength so a
    // contested new face goes to the strongest overlap.
    let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
    for (oi, o) in old.iter().enumerate() {
        for (ni, n) in new.iter().enumerate() {
            let iou_v = iou(&o.rect, &n.rect);
            if iou_v < CARRY_IOU {
                continue;
            }
            if let (Some(oe), Some(ne)) = (o.embedding, n.embedding) {
                if cosine(oe, ne) < CARRY_COSINE {
                    continue;
                }
            }
            candidates.push((iou_v, oi, ni));
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut used_old = vec![false; old.len()];
    let mut used_new = vec![false; new.len()];
    let mut pairs = Vec::new();
    for (_, oi, ni) in candidates {
        if !used_old[oi] && !used_new[ni] {
            used_old[oi] = true;
            used_new[ni] = true;
            pairs.push((oi, ni));
        }
    }
    pairs
}

/// Square chip crop with +25% margin around a normalized rect, clamped to the
/// image. Returns pixel (x, y, side) in preview space.
pub fn chip_crop(rect: [f32; 4], pw: u32, ph: u32) -> (u32, u32, u32) {
    let (pw_f, ph_f) = (pw as f32, ph as f32);
    let (x, y, w, h) = (
        rect[0] * pw_f,
        rect[1] * ph_f,
        rect[2] * pw_f,
        rect[3] * ph_f,
    );
    let side = (w.max(h) * 1.5).max(1.0); // face + 25% margin each side
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let side = side.min(pw_f).min(ph_f);
    let x0 = (cx - side / 2.0).clamp(0.0, pw_f - side);
    let y0 = (cy - side / 2.0).clamp(0.0, ph_f - side);
    (x0 as u32, y0 as u32, side as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &[f32; 6], p: [f32; 2]) -> [f32; 2] {
        [
            m[0] * p[0] + m[1] * p[1] + m[2],
            m[3] * p[0] + m[4] * p[1] + m[5],
        ]
    }

    #[test]
    fn decode_recovers_planted_cell() {
        let cols = YUNET_INPUT / 32; // 20
        let cells = cols * cols;
        let (mut cls, mut obj) = (vec![0f32; cells], vec![0f32; cells]);
        let mut bbox = vec![0f32; cells * 4];
        let mut kps = vec![0f32; cells * 10];
        let idx = 5 * cols + 7; // row 5, col 7
        cls[idx] = 0.81;
        obj[idx] = 1.0;
        bbox[idx * 4] = 0.5; // cx = (7 + 0.5) * 32 = 240
        bbox[idx * 4 + 1] = 0.25; // cy = (5 + 0.25) * 32 = 168
        bbox[idx * 4 + 2] = 1.0f32.ln(); // w = 32
        bbox[idx * 4 + 3] = 2.0f32.ln(); // h = 64
        kps[idx * 10] = 0.5;
        kps[idx * 10 + 1] = 0.25;
        let dets = decode_stride(&cls, &obj, &bbox, &kps, 32, 0.8);
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert!((d.score - 0.9).abs() < 1e-5); // sqrt(0.81)
        assert!((d.rect[0] - (240.0 - 16.0)).abs() < 1e-3);
        assert!((d.rect[1] - (168.0 - 32.0)).abs() < 1e-3);
        assert!((d.rect[2] - 32.0).abs() < 1e-3 && (d.rect[3] - 64.0).abs() < 1e-3);
        assert!((d.landmarks[0][0] - 240.0).abs() < 1e-3);
        assert!((d.landmarks[0][1] - 168.0).abs() < 1e-3);
        // Below-threshold cells stay silent.
        assert!(decode_stride(&cls, &obj, &bbox, &kps, 32, 0.95).is_empty());
    }

    #[test]
    fn nms_suppresses_overlaps_keeps_disjoint() {
        let d = |x: f32, score: f32| Detection {
            rect: [x, 0.0, 100.0, 100.0],
            score,
            landmarks: [[0.0; 2]; 5],
        };
        let kept = nms(vec![d(0.0, 0.9), d(10.0, 0.8), d(300.0, 0.7)], 0.3);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.9).abs() < 1e-6);
        assert!((kept[1].score - 0.7).abs() < 1e-6);
    }

    #[test]
    fn letterbox_maps_back_to_preview_space() {
        // 2600×1733 preview → scale = 640/2600.
        let lb = letterbox(2600, 1733);
        assert!((lb.scale - 640.0 / 2600.0).abs() < 1e-6);
        assert_eq!(lb.scaled_w, 640);
        let det = Detection {
            rect: [64.0, 32.0, 128.0, 128.0],
            score: 0.9,
            landmarks: [[64.0, 32.0]; 5],
        };
        let (rect, lms) = to_preview_space(&det, lb, 2600, 1733);
        let inv = 2600.0 / 640.0;
        assert!((rect[0] - 64.0 * inv / 2600.0).abs() < 1e-5);
        assert!((rect[2] - 128.0 * inv / 2600.0).abs() < 1e-5);
        assert!((lms[0][0] - 64.0 * inv).abs() < 1e-2);
    }

    #[test]
    fn umeyama_identity_when_landmarks_match_template() {
        let m = umeyama_112(&TEMPLATE_112.clone());
        assert!((m[0] - 1.0).abs() < 1e-3, "a≈1, got {}", m[0]);
        assert!(m[1].abs() < 1e-3 && m[3].abs() < 1e-3);
        assert!((m[4] - 1.0).abs() < 1e-3);
        assert!(m[2].abs() < 1e-2 && m[5].abs() < 1e-2);
    }

    #[test]
    fn umeyama_recovers_known_rotation_scale() {
        // Transform the template by a known similarity T (scale 2, rotate 30°,
        // translate) — the recovered mapping must be T⁻¹ to within 1e-3.
        let (s, th) = (2.0f32, 30f32.to_radians());
        let t = [
            s * th.cos(),
            -s * th.sin(),
            17.0,
            s * th.sin(),
            s * th.cos(),
            -4.0,
        ];
        let mut moved = TEMPLATE_112;
        for p in moved.iter_mut() {
            *p = apply(&t, *p);
        }
        let m = umeyama_112(&moved);
        for p in moved.iter().zip(TEMPLATE_112.iter()) {
            let back = apply(&m, *p.0);
            assert!(
                (back[0] - p.1[0]).abs() < 1e-3 && (back[1] - p.1[1]).abs() < 1e-3,
                "{back:?} vs {:?}",
                p.1
            );
        }
    }

    #[test]
    fn warp_solid_color_and_bgr_order() {
        let img = RgbImage::from_pixel(200, 200, image::Rgb([10, 20, 30]));
        // Identity-ish transform covering the image interior.
        let m = [1.0, 0.0, -10.0, 0.0, 1.0, -10.0]; // src (10..122) → dst (0..112)
        let out = warp_112(&img, &m);
        let plane = SFACE_INPUT * SFACE_INPUT;
        let mid = 56 * SFACE_INPUT + 56;
        assert!((out[mid] - 30.0).abs() < 1e-3, "B plane first");
        assert!((out[plane + mid] - 20.0).abs() < 1e-3);
        assert!((out[2 * plane + mid] - 10.0).abs() < 1e-3, "R plane last");
    }

    #[test]
    fn yunet_tensor_letterbox_padding_is_black() {
        let img = RgbImage::from_pixel(320, 160, image::Rgb([100, 150, 200]));
        let t = yunet_tensor(&img);
        let plane = YUNET_INPUT * YUNET_INPUT;
        assert!((t[0] - 200.0).abs() < 1e-3, "B at (0,0)");
        assert!((t[plane] - 150.0).abs() < 1e-3, "G at (0,0)");
        assert!((t[2 * plane] - 100.0).abs() < 1e-3, "R at (0,0)");
        // Padding beyond the placed image is zero.
        assert_eq!(t[200 * YUNET_INPUT + 400], 0.0);
        assert_eq!(t[0 * YUNET_INPUT + 321], 0.0);
    }

    #[test]
    fn cosine_and_normalize() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn embedding_blob_round_trip() {
        let v: Vec<f32> = (0..EMBED_DIM).map(|i| (i as f32) * 0.25 - 3.5).collect();
        let blob = embedding_to_blob(&v);
        assert_eq!(blob.len(), EMBED_DIM * 4);
        assert_eq!(blob_to_embedding(&blob).unwrap(), v);
        assert!(blob_to_embedding(&blob[1..]).is_none(), "wrong size → None");
    }

    #[test]
    fn cluster_greedy_is_deterministic_and_threshold_bound() {
        // Two tight groups + one outlier, in mixed order.
        let e = |x: f32, y: f32| {
            let mut v = vec![x, y];
            l2_normalize(&mut v);
            v
        };
        let embs = vec![
            e(1.0, 0.0),   // A anchor
            e(0.0, 1.0),   // B anchor
            e(0.98, 0.05), // near A
            e(0.05, 0.98), // near B
            e(-1.0, -1.0), // outlier
        ];
        let clusters = cluster_greedy(&embs, 0.9);
        assert_eq!(clusters.len(), 3);
        assert_eq!(clusters[0], vec![0, 2]);
        assert_eq!(clusters[1], vec![1, 3]);
        assert_eq!(clusters[2], vec![4]);
        // Determinism: same input, same output.
        assert_eq!(cluster_greedy(&embs, 0.9), clusters);
    }

    #[test]
    fn carry_over_matches_same_gen_and_ambiguous_lapses() {
        let r = |x: f32| [x, 0.1, 0.2, 0.2];
        let e1 = vec![1.0f32, 0.0];
        let e2 = vec![0.0f32, 1.0];
        // Same-gen: overlapping rect + same embedding → match.
        let old = [CarrySide {
            rect: r(0.10),
            embedding: Some(&e1),
        }];
        let new = [CarrySide {
            rect: r(0.11),
            embedding: Some(&e1),
        }];
        assert_eq!(match_carry_over(&old, &new), vec![(0, 0)]);
        // Same-gen: overlapping rect but a DIFFERENT face there now → no match.
        let new_diff = [CarrySide {
            rect: r(0.11),
            embedding: Some(&e2),
        }];
        assert!(match_carry_over(&old, &new_diff).is_empty());
        // Cross-gen (no embeddings): geometry alone decides.
        let old_x = [CarrySide {
            rect: r(0.10),
            embedding: None,
        }];
        let new_x = [CarrySide {
            rect: r(0.11),
            embedding: None,
        }];
        assert_eq!(match_carry_over(&old_x, &new_x), vec![(0, 0)]);
        // Ambiguity: two old faces best-match the same new face → only the
        // stronger overlap survives; the other's state lapses.
        let old_amb = [
            CarrySide {
                rect: [0.10, 0.1, 0.2, 0.2],
                embedding: None,
            },
            CarrySide {
                rect: [0.13, 0.1, 0.2, 0.2],
                embedding: None,
            },
        ];
        let new_amb = [CarrySide {
            rect: [0.11, 0.1, 0.2, 0.2],
            embedding: None,
        }];
        let pairs = match_carry_over(&old_amb, &new_amb);
        assert_eq!(pairs, vec![(0, 0)], "contested new face goes to best IoU");
    }

    #[test]
    fn exemplar_selection_prefers_diversity() {
        let e = |x: f32, y: f32| {
            let mut v = vec![x, y];
            l2_normalize(&mut v);
            v
        };
        // Three near-duplicates of the seed + two genuinely different poses.
        let embs = vec![
            e(1.0, 0.0),
            e(0.99, 0.01),
            e(0.98, 0.02),
            e(0.0, 1.0),
            e(-1.0, 0.5),
        ];
        let picked = select_exemplars(&embs, 3);
        assert_eq!(picked[0], 0, "seed is the strongest face");
        assert!(picked.contains(&3) && picked.contains(&4), "{picked:?}");
        // Cap respected; degenerate inputs safe.
        assert_eq!(select_exemplars(&embs, 10).len(), 5);
        assert!(select_exemplars(&[], 5).is_empty());
    }

    #[test]
    fn exemplar_outlier_filter_drops_junk_confirmed_faces() {
        let e = |x: f32, y: f32| {
            let mut v = vec![x, y];
            l2_normalize(&mut v);
            v
        };
        // Four coherent faces + one junk (orthogonal — a named back-of-head).
        let embs = vec![
            e(1.0, 0.0),
            e(0.95, 0.2),
            e(0.9, 0.3),
            e(0.97, 0.1),
            e(0.0, 1.0), // junk
        ];
        assert_eq!(filter_exemplar_outliers(&embs), vec![0, 1, 2, 3]);
        // Too few faces to judge → keep everything.
        assert_eq!(filter_exemplar_outliers(&embs[..3]), vec![0, 1, 2]);
        // A person whose faces are ALL mutually dissimilar: bail out, keep all
        // (the matching thresholds still protect us downstream).
        let scattered = vec![e(1.0, 0.0), e(0.0, 1.0), e(-1.0, 0.0), e(0.0, -1.0)];
        assert_eq!(filter_exemplar_outliers(&scattered).len(), 4);
    }

    #[test]
    fn match_face_threshold_margin_and_absolute_rejection() {
        let e = |x: f32, y: f32| {
            let mut v = vec![x, y];
            l2_normalize(&mut v);
            v
        };
        let protos = vec![
            PersonProtos {
                person_id: 1,
                exemplars: vec![e(1.0, 0.0)],
            },
            PersonProtos {
                person_id: 2,
                exemplars: vec![e(0.0, 1.0)],
            },
        ];
        let none = std::collections::HashSet::new();
        // Clear winner above threshold.
        let q = e(0.95, 0.1);
        let (p, s) = match_face(&q, &protos, &none, 0.40, 0.05).unwrap();
        assert_eq!(p, 1);
        assert!(s > 0.9);
        // Below threshold → no assignment.
        assert!(match_face(&e(0.3, 0.2), &protos, &none, 0.99, 0.05).is_none());
        // Equidistant between two persons → margin blocks it.
        let mid = e(1.0, 1.0);
        assert!(
            match_face(&mid, &protos, &none, 0.40, 0.05).is_none(),
            "ambiguous face must stay unassigned"
        );
        // …but with the runner-up REJECTED, its score is meaningless and the
        // face assigns cleanly (rejections are absolute, both directions).
        let rej2: std::collections::HashSet<i64> = [2].into();
        let (p, _) = match_face(&mid, &protos, &rej2, 0.40, 0.05).unwrap();
        assert_eq!(p, 1);
        // Rejecting the winner blocks it even at similarity 1.0.
        let rej1: std::collections::HashSet<i64> = [1].into();
        let got = match_face(&e(1.0, 0.0), &protos, &rej1, 0.40, 0.05);
        assert!(got.is_none() || got.unwrap().0 != 1, "\"not X\" is final");
    }

    #[test]
    fn one_face_per_person_keeps_the_best_and_respects_existing() {
        let none = std::collections::HashSet::new();
        // Person 1 proposed on faces 0 and 2 → only the stronger survives.
        let kept = one_face_per_person(vec![(0, 1, 0.6), (1, 2, 0.9), (2, 1, 0.8)], &none);
        assert_eq!(kept, vec![(1, 2, 0.9), (2, 1, 0.8)]);
        // A person already on another face in this photo is not proposed again.
        let present: std::collections::HashSet<i64> = [2].into();
        let kept = one_face_per_person(vec![(0, 1, 0.6), (1, 2, 0.9)], &present);
        assert_eq!(kept, vec![(0, 1, 0.6)]);
        assert!(one_face_per_person(vec![], &none).is_empty());
    }

    #[test]
    fn chip_crop_squares_margins_and_clamps() {
        // Face rect 200×100 at (100,50) in a 1000×800 preview.
        let (x, y, side) = chip_crop([0.1, 0.0625, 0.2, 0.125], 1000, 800);
        assert_eq!(side, 300, "1.5× the long edge");
        assert_eq!(x, 50, "centered on the face");
        assert_eq!(y, 0, "clamped to the top edge");
        // A face at the corner stays inside the image.
        let (x, y, side) = chip_crop([0.9, 0.9, 0.1, 0.1], 1000, 800);
        assert!(x + side <= 1000 && y + side <= 800);
    }
}
