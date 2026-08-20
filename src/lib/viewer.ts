/**
 * Imperative canvas renderer — the flip hot path. React renders chrome around
 * this canvas; it never touches the pixels.
 *
 * Zoom model: `view` is null in fit mode, else {cx, cy, factor} where cx/cy
 * are the viewport center in normalized image coords and factor is relative
 * to the fit scale. Normalized state survives photo flips unchanged — that IS
 * the locked-zoom-across-a-burst behavior. Rendering prefers the full-res
 * bitmap and falls back to the preview upscaled at the same view (progressive
 * sharpen) while full-res decodes.
 *
 * All mutations mark dirty and draw on the next animation frame — gesture
 * streams (pinch wheel events arrive at 60–120Hz) never trigger synchronous
 * redraws. While a gesture is active, resampling drops to low quality; a
 * settle timer repaints crisp.
 */

export interface AfPoint {
  x: number;
  y: number;
}

interface ViewState {
  cx: number;
  cy: number;
  factor: number; // 1 = fit
}

let canvas: HTMLCanvasElement | null = null;
let ctx: CanvasRenderingContext2D | null = null;
let observer: ResizeObserver | null = null;

let preview: ImageBitmap | null = null;
let fullres: ImageBitmap | null = null;
let blinkies: ImageBitmap | null = null;
/** Display-oriented native dimensions (from the fast EXIF pass, or the
 * full-res bitmap once decoded). */
let nativeW = 0;
let nativeH = 0;

let view: ViewState | null = null;
let afPoint: AfPoint | null = null;
let afVisible = false;

let changeListeners: Array<() => void> = [];
/** Listeners that only care about fit-mode DOM chrome (face badges) — see
 * `subscribeLayout`. Kept apart from `changeListeners` on purpose. */
let layoutListeners: Array<() => void> = [];
let lastLayoutKey = '';

const BG = '#141414';
const MAX_OVER_100 = 4;

// Navigator mini-map (shows while zooming, fades out when idle).
const MAP_CSS_WIDTH = 190;
const MAP_MARGIN = 14;
const MAP_VISIBLE_MS = 1800;
const MAP_FADE_MS = 400;
let lastInteractAt = 0;
let mapRect: { x: number; y: number; w: number; h: number } | null = null;

// rAF-coalesced drawing
let drawQueued = false;
let settleTimer: ReturnType<typeof setTimeout> | null = null;

/** A closed ImageBitmap reports 0×0 — never hand one to drawImage (it throws
 * mid-frame and leaves the canvas blank). */
function alive(b: ImageBitmap | null): b is ImageBitmap {
  return b !== null && b.width > 0 && b.height > 0;
}

/** Everything a fit-mode overlay's geometry depends on: the canvas backing
 * store, the displayed image, and whether we are in fit mode at all. Pan and
 * zoom inside a gesture leave this untouched — which is the point. */
function layoutKey(): string {
  const cw = canvas?.width ?? 0;
  const ch = canvas?.height ?? 0;
  const pw = alive(preview) ? preview.width : 0;
  const ph = alive(preview) ? preview.height : 0;
  return `${cw}x${ch}|${view ? 'zoom' : 'fit'}|${pw}x${ph}`;
}

function notifyChange(): void {
  for (const l of changeListeners) l();
  const key = layoutKey();
  if (key === lastLayoutKey) return;
  lastLayoutKey = key;
  for (const l of layoutListeners) l();
}

export function subscribe(fn: () => void): () => void {
  changeListeners.push(fn);
  return () => {
    changeListeners = changeListeners.filter((l) => l !== fn);
  };
}

/**
 * Subscribe to DISCRETE layout changes only (resize, photo swap, entering or
 * leaving zoom) — never to the 60–120Hz gesture stream `subscribe` carries.
 * Face badges are fit-mode-only DOM chrome: waking React for every pinch frame
 * would put chrome work on the interaction path, which is exactly what this
 * app keeps off it.
 */
export function subscribeLayout(fn: () => void): () => void {
  layoutListeners.push(fn);
  return () => {
    layoutListeners = layoutListeners.filter((l) => l !== fn);
  };
}

/** Synchronous draw for DISCRETE actions (flips, Z, F): the flip hot path
 * must never wait on an animation frame — WebKit throttles rAF to ~1Hz for
 * occluded windows, which would turn a 2ms blit into a visible stall. */
function drawNow(): void {
  draw();
  notifyChange();
}

/** rAF-coalesced draw for GESTURE STREAMS (pinch/pan events arrive at
 * 60–120Hz; drawing each one synchronously is what made pinch choppy). */
function requestDraw(): void {
  if (drawQueued) return;
  drawQueued = true;
  requestAnimationFrame(() => {
    drawQueued = false;
    draw();
    notifyChange();
    // Keep animating while the minimap is mid-fade.
    const sinceInteract = performance.now() - lastInteractAt;
    if (view && sinceInteract > MAP_VISIBLE_MS && sinceInteract < MAP_VISIBLE_MS + MAP_FADE_MS) {
      requestDraw();
    }
  });
}

/** Gesture tick: timestamps interaction (low-quality resampling window,
 * minimap visibility) and schedules a crisp settle repaint. */
function interact(): void {
  lastInteractAt = performance.now();
  if (settleTimer) clearTimeout(settleTimer);
  settleTimer = setTimeout(() => requestDraw(), 180);
  requestDraw();
}

/** Discrete-action variant: show the minimap (with fade) but draw right now. */
function interactNow(): void {
  lastInteractAt = performance.now();
  if (settleTimer) clearTimeout(settleTimer);
  settleTimer = setTimeout(() => requestDraw(), 200);
  drawNow();
}

function resizeBackingStore(): void {
  if (!canvas || !ctx) return;
  const dpr = window.devicePixelRatio || 1;
  const { clientWidth, clientHeight } = canvas;
  const w = Math.max(1, Math.round(clientWidth * dpr));
  const h = Math.max(1, Math.round(clientHeight * dpr));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  requestDraw();
}

export function init(el: HTMLCanvasElement): void {
  if (canvas === el) return;
  canvas = el;
  try {
    ctx = el.getContext('2d', { colorSpace: 'display-p3', alpha: false });
  } catch {
    ctx = null;
  }
  if (!ctx) ctx = el.getContext('2d', { alpha: false });
  observer?.disconnect();
  observer = new ResizeObserver(resizeBackingStore);
  observer.observe(el);
  resizeBackingStore();
}

// ---------- geometry ----------

function fitScale(): number {
  if (!canvas || !nativeW || !nativeH) return 1;
  return Math.min(canvas.width / nativeW, canvas.height / nativeH);
}

/** Zoom factor at which 1 native px = 1 device px. */
function factorFor100(): number {
  const s0 = fitScale();
  return s0 > 0 ? 1 / s0 : 1;
}

export function zoomPercent(): number | null {
  if (!view) return null;
  return fitScale() * view.factor;
}

export interface CssBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Map a normalized display-space rect (face rects, SPEC §14) to CSS pixels
 * over the canvas element, for DOM chrome drawn on top of it.
 *
 * Fit mode only, deliberately: zoom is for inspecting pixels, and chasing the
 * canvas transform through a 120Hz pinch stream with DOM nodes is exactly the
 * kind of chrome work this app keeps off the interaction path.
 */
export function normalizedRectToCss(r: [number, number, number, number]): CssBox | null {
  if (!canvas || view || !alive(preview)) return null;
  const dpr = window.devicePixelRatio || 1;
  const { width: cw, height: ch } = canvas;
  const scale = Math.min(cw / preview.width, ch / preview.height);
  const dw = preview.width * scale;
  const dh = preview.height * scale;
  const dx = (cw - dw) / 2;
  const dy = (ch - dh) / 2;
  return {
    left: (dx + r[0] * dw) / dpr,
    top: (dy + r[1] * dh) / dpr,
    width: (r[2] * dw) / dpr,
    height: (r[3] * dh) / dpr,
  };
}

export function isZoomed(): boolean {
  return view !== null;
}

function clampView(v: ViewState): ViewState {
  if (!canvas || !nativeW || !nativeH) return v;
  const scale = fitScale() * v.factor;
  const halfW = canvas.width / scale / 2 / nativeW;
  const halfH = canvas.height / scale / 2 / nativeH;
  const cx = halfW >= 0.5 ? 0.5 : Math.min(1 - halfW, Math.max(halfW, v.cx));
  const cy = halfH >= 0.5 ? 0.5 : Math.min(1 - halfH, Math.max(halfH, v.cy));
  return { cx, cy, factor: v.factor };
}

// ---------- photo lifecycle ----------

/** Show a photo. `view` deliberately survives — locked zoom across flips. */
export function setPhoto(
  previewBitmap: ImageBitmap | null,
  dims: { w: number; h: number } | null,
): void {
  preview = previewBitmap;
  fullres = null;
  blinkies = null; // per-photo; the session re-supplies it when enabled
  if (dims) {
    nativeW = dims.w;
    nativeH = dims.h;
  } else if (previewBitmap) {
    // Best effort until full-res arrives; preview aspect is exact.
    nativeW = previewBitmap.width;
    nativeH = previewBitmap.height;
  }
  if (view) view = clampView(view);
  drawNow();
}

export function setFullres(bitmap: ImageBitmap | null): void {
  fullres = alive(bitmap) ? bitmap : null;
  if (fullres) {
    nativeW = fullres.width;
    nativeH = fullres.height;
    if (view) view = clampView(view);
  }
  drawNow();
}

export function setAf(point: AfPoint | null, visible: boolean): void {
  afPoint = point;
  afVisible = visible;
  drawNow();
}

/** Clipping-warning overlay (RGBA mask, proportional to the full image).
 * It BLINKS — a ~1.1s cycle toggling the mask layer. Cost is one extra
 * repaint per phase (~2ms blit); zoom/pan frames just pick up the current
 * phase for free since they redraw anyway. */
let blinkOn = true;
let blinkTimer: ReturnType<typeof setInterval> | null = null;

export function setBlinkies(bitmap: ImageBitmap | null): void {
  blinkies = alive(bitmap) ? bitmap : null;
  if (blinkies && !blinkTimer) {
    blinkOn = true;
    blinkTimer = setInterval(() => {
      blinkOn = !blinkOn;
      drawNow();
    }, 550);
  } else if (!blinkies && blinkTimer) {
    clearInterval(blinkTimer);
    blinkTimer = null;
    blinkOn = true;
  }
  drawNow();
}

// ---------- zoom controls ----------

export function exitZoom(): void {
  view = null;
  // The session closes its full-res cache on zoom exit; drop our reference
  // BEFORE the next draw or drawImage would throw on a closed bitmap.
  fullres = null;
  drawNow();
}

/** Z: toggle fit ↔ 100% (centered where you are). */
export function toggle100(): void {
  if (view) {
    exitZoom();
    return;
  }
  view = clampView({ cx: 0.5, cy: 0.5, factor: factorFor100() });
  interactNow();
}

/** F: 100% centered on a normalized point (the AF point). */
export function zoomToPoint(x: number, y: number): void {
  view = clampView({ cx: x, cy: y, factor: factorFor100() });
  interactNow();
}

/** Pinch: multiply zoom, keeping the image point under `anchor` stationary.
 * Anchor is in canvas device pixels. */
export function zoomBy(multiplier: number, anchor?: { x: number; y: number }): void {
  if (!canvas) return;
  const current = view ?? { cx: 0.5, cy: 0.5, factor: 1 };
  const maxFactor = factorFor100() * MAX_OVER_100;
  const factor = Math.min(maxFactor, Math.max(1, current.factor * multiplier));
  if (factor <= 1.001) {
    exitZoom();
    return;
  }
  let { cx, cy } = current;
  if (anchor && nativeW && nativeH) {
    // Image point under the anchor before the zoom…
    const s1 = fitScale() * current.factor;
    const px = current.cx + (anchor.x - canvas.width / 2) / s1 / nativeW;
    const py = current.cy + (anchor.y - canvas.height / 2) / s1 / nativeH;
    // …stays under it after.
    const s2 = fitScale() * factor;
    cx = px - (anchor.x - canvas.width / 2) / s2 / nativeW;
    cy = py - (anchor.y - canvas.height / 2) / s2 / nativeH;
  }
  view = clampView({ cx, cy, factor });
  interact();
}

/** Pan by canvas device pixels. */
export function panBy(dx: number, dy: number): void {
  if (!view || !nativeW || !nativeH) return;
  const scale = fitScale() * view.factor;
  view = clampView({
    cx: view.cx + dx / scale / nativeW,
    cy: view.cy + dy / scale / nativeH,
    factor: view.factor,
  });
  interact();
}

/** Click/drag on the navigator mini-map jumps the view. Returns true when the
 * point (device px) landed on the map. */
export function minimapNavigate(x: number, y: number): boolean {
  if (!view || !mapRect) return false;
  const { x: mx, y: my, w, h } = mapRect;
  if (x < mx || y < my || x > mx + w || y > my + h) return false;
  view = clampView({
    cx: (x - mx) / w,
    cy: (y - my) / h,
    factor: view.factor,
  });
  interact();
  return true;
}

// ---------- drawing ----------

function draw(): void {
  if (!canvas || !ctx) return;
  const { width: cw, height: ch } = canvas;
  ctx.fillStyle = BG;
  ctx.fillRect(0, 0, cw, ch);
  mapRect = null;

  if (view && !alive(fullres)) fullres = null;
  if (!alive(preview)) preview = null;
  const interacting = performance.now() - lastInteractAt < 150;
  // During active pan/pinch, sample the 2600px preview instead of the 40MP
  // full-res bitmap — per-frame source-rect blits from a ~160MB bitmap are
  // what made dragging choppy. The settle repaint snaps back to crisp.
  const bitmap =
    view && fullres && (!interacting || !alive(preview)) ? fullres : preview;
  if (!bitmap || !nativeW || !nativeH) return;

  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = interacting ? 'low' : 'high';

  if (!view) {
    const scale = Math.min(cw / bitmap.width, ch / bitmap.height);
    const dw = Math.round(bitmap.width * scale);
    const dh = Math.round(bitmap.height * scale);
    const dx = Math.round((cw - dw) / 2);
    const dy = Math.round((ch - dh) / 2);
    ctx.drawImage(bitmap, dx, dy, dw, dh);
    if (alive(blinkies) && blinkOn) ctx.drawImage(blinkies, dx, dy, dw, dh);
    drawAf(dx, dy, dw / nativeW, dh / nativeH);
    return;
  }

  const scale = fitScale() * view.factor; // device px per native px
  const bScale = bitmap.width / nativeW; // bitmap px per native px
  const at100 = Math.abs(scale - 1) < 0.001 && bitmap === fullres;

  // Visible native-space rect, clamped to the image.
  const viewWn = Math.min(cw / scale, nativeW);
  const viewHn = Math.min(ch / scale, nativeH);
  const leftN = Math.min(Math.max(0, view.cx * nativeW - cw / scale / 2), nativeW - viewWn);
  const topN = Math.min(Math.max(0, view.cy * nativeH - ch / scale / 2), nativeH - viewHn);
  // Letterbox offset when the image is narrower than the viewport.
  const dx = Math.round(Math.max(0, (cw - nativeW * scale) / 2));
  const dy = Math.round(Math.max(0, (ch - nativeH * scale) / 2));

  let sx = leftN * bScale;
  let sy = topN * bScale;
  let sw = viewWn * bScale;
  let sh = viewHn * bScale;
  if (at100) {
    // Integer-snap at exactly 100% to avoid resampling shimmer.
    sx = Math.round(sx);
    sy = Math.round(sy);
    sw = Math.round(sw);
    sh = Math.round(sh);
  }
  const dw = Math.round(viewWn * scale);
  const dh = Math.round(viewHn * scale);
  try {
    ctx.drawImage(bitmap, sx, sy, sw, sh, dx, dy, dw, dh);
  } catch {
    return; // bitmap died between the alive() check and here; next draw recovers
  }
  if (alive(blinkies) && blinkOn) {
    const mScale = blinkies.width / nativeW;
    ctx.drawImage(
      blinkies,
      leftN * mScale,
      topN * (blinkies.height / nativeH),
      viewWn * mScale,
      viewHn * (blinkies.height / nativeH),
      dx,
      dy,
      dw,
      dh,
    );
  }
  drawAf(dx - leftN * scale, dy - topN * scale, scale, scale);
  drawMinimap(leftN, topN, viewWn, viewHn);
}

/** AF rectangle, given the native→screen transform. */
function drawAf(originX: number, originY: number, scaleX: number, scaleY: number): void {
  if (!ctx || !afVisible || !afPoint || !nativeW || !nativeH) return;
  const size = Math.min(nativeW, nativeH) * 0.055;
  const x = originX + (afPoint.x * nativeW - size / 2) * scaleX;
  const y = originY + (afPoint.y * nativeH - size / 2) * scaleY;
  ctx.strokeStyle = 'rgba(110, 220, 130, 0.95)';
  ctx.lineWidth = Math.max(2, (window.devicePixelRatio || 1) * 1.5);
  ctx.strokeRect(x, y, size * scaleX, size * scaleY);
}

/** Navigator: small copy of the photo top-right with a green rect marking the
 * visible region. Shown while interacting, fades out when idle. Clickable. */
function drawMinimap(leftN: number, topN: number, viewWn: number, viewHn: number): void {
  if (!ctx || !canvas || !alive(preview)) return;
  const since = performance.now() - lastInteractAt;
  let alpha = 1;
  if (since > MAP_VISIBLE_MS) {
    alpha = 1 - (since - MAP_VISIBLE_MS) / MAP_FADE_MS;
    if (alpha <= 0) return;
  }
  const dpr = window.devicePixelRatio || 1;
  const mw = MAP_CSS_WIDTH * dpr;
  const mh = (mw * nativeH) / nativeW;
  const mx = canvas.width - mw - MAP_MARGIN * dpr;
  const my = MAP_MARGIN * dpr;
  mapRect = { x: mx, y: my, w: mw, h: mh };

  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.fillStyle = 'rgba(0, 0, 0, 0.45)';
  ctx.fillRect(mx - 3, my - 3, mw + 6, mh + 6);
  ctx.imageSmoothingQuality = 'medium';
  ctx.drawImage(preview, mx, my, mw, mh);
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.35)';
  ctx.lineWidth = 1 * dpr;
  ctx.strokeRect(mx, my, mw, mh);
  // Visible-region rectangle (the ApolloOne green square).
  ctx.strokeStyle = 'rgba(110, 220, 130, 0.95)';
  ctx.lineWidth = 1.5 * dpr;
  ctx.strokeRect(
    mx + (leftN / nativeW) * mw,
    my + (topN / nativeH) * mh,
    (viewWn / nativeW) * mw,
    (viewHn / nativeH) * mh,
  );
  ctx.restore();
}

/** Legacy fit-mode entry point (kept for callers that only clear). */
export function render(bitmap: ImageBitmap | null): void {
  setPhoto(bitmap, null);
}
