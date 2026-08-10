import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import * as session from './lib/session';
import * as viewer from './lib/viewer';
import * as perf from './lib/perf';
import * as imageCache from './lib/imageCache';
import { histUrl, pickFolder, previewUrl, trashedList, type HistogramData } from './lib/ipc';
import { actionFor, loadKeymap } from './lib/keys';
import { installLogForwarding, runIfRequested } from './lib/devharness';
import Filmstrip from './components/Filmstrip';
import CheatSheet from './components/CheatSheet';
import RecipeSwitcher from './components/RecipeSwitcher';
import TagPalette from './components/TagPalette';
import TagSwitcher from './components/TagSwitcher';
import ExifPanel from './components/ExifPanel';
import PeoplePanel from './components/PeoplePanel';
import PersonSwitcher from './components/PersonSwitcher';
import FaceBadges from './components/FaceBadges';
import type { TrashedPhoto } from './lib/types';
import './App.css';

function pairBadge(hasJpeg: boolean, hasRaf: boolean): string {
  if (hasJpeg && hasRaf) return 'J+R';
  return hasJpeg ? 'J' : 'R';
}

const FILTER_LABEL: Record<string, string> = {
  unstarred: 'unstarred',
  starred: 'starred',
  star1: '★ only',
  star2: '★★ only',
  star3: '★★★ only',
  star4: '★★★★ only',
  star5: '★★★★★ only',
};

function PerfHud() {
  const statsJson = useSyncExternalStore(perf.subscribe, () => JSON.stringify(perf.stats()));
  const s: perf.PerfStats = JSON.parse(statsJson);
  const [running, setRunning] = useState(false);

  const runStorm = async () => {
    setRunning(true);
    try {
      await perf.flipStorm();
    } finally {
      setRunning(false);
    }
  };

  const fmt = (n: number) => n.toFixed(1);
  const over = s.p99 > 50;
  return (
    <div className="perf-hud">
      <div className="perf-title">perf</div>
      <table>
        <tbody>
          <tr><td>flips</td><td>{s.flips}</td></tr>
          <tr><td>p50</td><td>{fmt(s.p50)} ms</td></tr>
          <tr><td>p95</td><td>{fmt(s.p95)} ms</td></tr>
          <tr className={over ? 'perf-bad' : 'perf-good'}><td>p99</td><td>{fmt(s.p99)} ms</td></tr>
          <tr><td>max</td><td>{fmt(s.max)} ms</td></tr>
          <tr className={s.missServes > 0 ? 'perf-bad' : 'perf-good'}>
            <td>misses</td><td>{s.missServes}</td>
          </tr>
          {s.occluded > 0 && <tr><td>occluded</td><td>{s.occluded}</td></tr>}
          <tr><td>cache</td><td>{imageCache.size()}</td></tr>
          <tr><td>cold open</td><td>{s.coldOpenMs === null ? '—' : `${fmt(s.coldOpenMs)} ms`}</td></tr>
        </tbody>
      </table>
      <button onClick={runStorm} disabled={running}>
        {running ? 'storming…' : 'flip storm (300)'}
      </button>
    </div>
  );
}

/** 3-bin box smoothing: turns the raw comb into the readable curves that
 * camera/ApolloOne histograms show. */
function smoothBins(bins: number[]): number[] {
  const out = new Array<number>(256);
  for (let i = 0; i < 256; i++) {
    let sum = 0;
    let n = 0;
    for (let k = -2; k <= 2; k++) {
      const j = i + k;
      if (j >= 0 && j < 256) {
        sum += bins[j];
        n++;
      }
    }
    out[i] = sum / n;
  }
  return out;
}

/** ApolloOne-style: per-channel translucent fill + a distinct bright outline,
 * linear scale, normalized on interior bins (endpoint spikes clamp). */
function drawHistogram(
  canvas: HTMLCanvasElement | null,
  data: HistogramData | null,
  mode: 'lum' | 'rgb',
): void {
  if (!canvas) return;
  const g = canvas.getContext('2d');
  if (!g) return;
  const { width: w, height: h } = canvas;
  g.clearRect(0, 0, w, h);
  if (!data) return;
  const series: Array<{ bins: number[]; fill: string; line: string }> =
    mode === 'lum'
      ? [{ bins: data.l, fill: 'rgba(200, 200, 200, 0.45)', line: 'rgba(235, 235, 235, 0.95)' }]
      : [
          { bins: data.b, fill: 'rgba(100, 130, 185, 0.50)', line: 'rgba(135, 170, 235, 0.95)' },
          { bins: data.g, fill: 'rgba(100, 150, 85, 0.50)', line: 'rgba(130, 200, 110, 0.95)' },
          { bins: data.r, fill: 'rgba(180, 75, 70, 0.50)', line: 'rgba(230, 110, 100, 0.95)' },
        ];
  const smoothed = series.map((s) => ({ ...s, bins: smoothBins(s.bins) }));
  const max = Math.max(1, ...smoothed.map((s) => Math.max(...s.bins.slice(2, 254))));
  const yOf = (v: number) => h - Math.min(1, v / max) * (h - 3);
  for (const s of smoothed) {
    g.fillStyle = s.fill;
    g.beginPath();
    g.moveTo(0, h);
    for (let i = 0; i < 256; i++) g.lineTo((i / 255) * w, yOf(s.bins[i]));
    g.lineTo(w, h);
    g.closePath();
    g.fill();
  }
  // Outlines on top of every fill so each channel's curve stays readable.
  for (const s of smoothed) {
    g.strokeStyle = s.line;
    g.lineWidth = 2;
    g.beginPath();
    for (let i = 0; i < 256; i++) {
      const x = (i / 255) * w;
      const y = yOf(s.bins[i]);
      if (i === 0) g.moveTo(x, y);
      else g.lineTo(x, y);
    }
    g.stroke();
  }
}

function HistogramPanel({ photoId, mode }: { photoId: string; mode: 'lum' | 'rgb' }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const load = (attempt: number) => {
      fetch(`${histUrl(photoId)}?r=${attempt}`)
        .then((r) => (r.ok ? (r.json() as Promise<HistogramData>) : null))
        .then((data) => {
          if (!alive) return;
          if (data) drawHistogram(canvasRef.current, data, mode);
          else if (attempt < 15) timer = setTimeout(() => load(attempt + 1), 1000);
        })
        .catch(() => {});
    };
    drawHistogram(canvasRef.current, null, mode);
    load(0);
    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
    };
  }, [photoId, mode]);

  return <canvas ref={canvasRef} className="hist-panel" width={420} height={150} />;
}

function TrashPanel({ folderId, onClose }: { folderId: number; onClose: () => void }) {
  const [items, setItems] = useState<TrashedPhoto[] | null>(null);

  const load = useCallback(() => {
    void trashedList(folderId).then(setItems);
  }, [folderId]);

  useEffect(load, [load]);

  const restore = async (id: string) => {
    try {
      await session.restoreFromTrash(id);
    } catch (e) {
      console.warn(String(e));
    }
    load();
  };

  return (
    <div className="trash-panel">
      <div className="trash-head">
        <span>Trashed this folder</span>
        <button onClick={onClose}>close</button>
      </div>
      {items === null && <div className="trash-empty">loading…</div>}
      {items?.length === 0 && <div className="trash-empty">Nothing in the Trash from this folder.</div>}
      <ul>
        {items?.map((t) => (
          <li key={t.id}>
            <img className="trash-thumb" src={previewUrl(t.id)} loading="lazy" alt={t.stem} />
            <div className="trash-meta">
              <span className="trash-stem">{t.stem}</span>
              {t.rating > 0 && <span className="trash-stars">{'★'.repeat(t.rating)}</span>}
            </div>
            <button onClick={() => void restore(t.id)}>restore</button>
          </li>
        ))}
      </ul>
    </div>
  );
}

export default function App() {
  const state = useSyncExternalStore(session.subscribe, session.getState);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [showPerf, setShowPerf] = useState(false);
  const [showTrash, setShowTrash] = useState(false);
  const [showPeople, setShowPeople] = useState(false);
  const [showCheat, setShowCheat] = useState(false);
  const [showRecipes, setShowRecipes] = useState(false);
  const [showTagPalette, setShowTagPalette] = useState(false);
  const [showTagFilter, setShowTagFilter] = useState(false);
  const [showPersonFilter, setShowPersonFilter] = useState(false);
  // Ref mirror so the (deps-stable) global key handler sees the live values.
  const overlayOpenRef = useRef(false);
  overlayOpenRef.current =
    showRecipes || showTagPalette || showTagFilter || showPersonFilter;
  const [showStrip, setShowStrip] = useState(localStorage.getItem('filmstrip') !== '0');
  const [showFaces, setShowFaces] = useState(localStorage.getItem('faceBadges') !== '0');
  const [showExif, setShowExif] = useState(localStorage.getItem('exifPanel') === '1');
  const [keysReady, setKeysReady] = useState(false);
  const [keysError, setKeysError] = useState<string | null>(null);

  useEffect(() => {
    session.start();
    if (canvasRef.current) viewer.init(canvasRef.current);
    installLogForwarding();
    // The keyboard IS the app: a failed keymap load may never be silent.
    // Retry briefly (startup races), then show a persistent, actionable error.
    const tryLoadKeymap = (attempt: number): void => {
      loadKeymap()
        .then((r) => {
          setKeysReady(true);
          setKeysError(null);
          if (r.warning) session.notify(r.warning);
        })
        .catch((e: unknown) => {
          if (attempt < 3) setTimeout(() => tryLoadKeymap(attempt + 1), 400);
          else setKeysError(String(e));
        });
    };
    tryLoadKeymap(0);
    void runIfRequested();
  }, []);

  const openViaDialog = useCallback(async () => {
    const dir = await pickFolder();
    if (dir) void session.openFolder(dir);
  }, []);

  const toggleStrip = useCallback(() => {
    setShowStrip((v) => {
      localStorage.setItem('filmstrip', v ? '0' : '1');
      return !v;
    });
  }, []);

  useEffect(() => {
    if (!keysReady) return;
    const onKey = (e: KeyboardEvent) => {
      // Form fields (recipe name input, filter dropdown) own their keys.
      const target = e.target as HTMLElement | null;
      if (target && ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)) return;
      // Open pickers/palettes own every key (incl. Escape) while mounted.
      if (overlayOpenRef.current) return;
      if (e.key === 'Escape') {
        setShowCheat(false);
        setShowTrash(false);
        setShowPeople(false);
        return;
      }
      const action = actionFor(e);
      if (!action) return;
      e.preventDefault();
      // Key-repeat is for scanning, not for verdicts or toggles.
      if (e.repeat && action !== 'next' && action !== 'prev') return;
      switch (action) {
        case 'next':
          session.flip(1, e.timeStamp);
          break;
        case 'prev':
          session.flip(-1, e.timeStamp);
          break;
        case 'home':
          session.jumpTo(0);
          break;
        case 'end':
          session.jumpTo(Number.MAX_SAFE_INTEGER);
          break;
        case 'trash':
          void session.trashCurrent();
          break;
        case 'undo':
          void session.undo();
          break;
        case 'redo':
          void session.redo();
          break;
        case 'filter_cycle':
          session.cycleFilter();
          break;
        case 'filter_all':
          session.setFilter('all');
          break;
        case 'sort_cycle':
          session.cycleSort();
          break;
        case 'sort_reverse':
          session.toggleReverse();
          break;
        case 'zoom_100':
          session.toggleZoom();
          break;
        case 'focus_zoom':
          void session.focusZoom();
          break;
        case 'af_overlay':
          session.toggleAfOverlay();
          break;
        case 'histogram':
          session.cycleHistogram();
          break;
        case 'blinkies':
          session.toggleBlinkies();
          break;
        case 'auto_advance':
          session.toggleAutoAdvance();
          break;
        case 'filmstrip':
          toggleStrip();
          break;
        case 'exif_panel':
          setShowExif((v) => {
            localStorage.setItem('exifPanel', v ? '0' : '1');
            return !v;
          });
          break;
        case 'refresh':
          void session.refresh();
          break;
        case 'cheat_sheet':
          setShowCheat((v) => !v);
          break;
        case 'recipe_filter':
          setShowRecipes((v) => !v);
          break;
        case 'tag_palette':
          setShowTagPalette((v) => !v);
          break;
        case 'tag_filter':
          setShowTagFilter((v) => !v);
          break;
        case 'people_panel':
          setShowPeople((v) => !v);
          setShowTrash(false); // same dock — one panel at a time
          break;
        case 'person_filter':
          setShowPersonFilter((v) => !v);
          break;
        case 'face_badges':
          setShowFaces((v) => {
            localStorage.setItem('faceBadges', v ? '0' : '1');
            return !v;
          });
          break;
        case 'perf_hud':
          setShowPerf((v) => !v);
          break;
        case 'open_folder':
          void openViaDialog();
          break;
        default:
          if (action.startsWith('rate')) {
            void session.rate(Number(action.slice(4)), e.timeStamp);
          } else if (action.startsWith('filter_star')) {
            session.setFilter(`star${action.slice(11)}` as session.FilterMode);
          }
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [keysReady, openViaDialog, toggleStrip]);

  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === 'drop' && event.payload.paths.length > 0) {
        void session.openFolder(event.payload.paths[0]);
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Zoom gestures. Trackpad pinch arrives as wheel events with ctrlKey set
  // (the cross-browser de-facto standard — WebKit ALSO fires proprietary
  // GestureEvents for the same pinch; handling both double-zooms, so we
  // deliberately swallow those). The viewer coalesces all of this into
  // rAF-paced draws; the expensive settle work (full-res fetch) is debounced.
  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const dpr = () => window.devicePixelRatio || 1;
    const anchorOf = (e: { clientX: number; clientY: number }) => {
      const r = el.getBoundingClientRect();
      return { x: (e.clientX - r.left) * dpr(), y: (e.clientY - r.top) * dpr() };
    };
    let settleTimer: ReturnType<typeof setTimeout> | null = null;
    const settleSoon = () => {
      if (settleTimer) clearTimeout(settleTimer);
      settleTimer = setTimeout(() => session.onManualZoomChange(), 180);
    };
    // WebKit delivers trackpad pinch as proprietary GestureEvents (it does
    // NOT synthesize ctrl+wheel like Chromium/Firefox — swallowing these
    // killed pinch entirely). GestureEvents are primary; ctrl+wheel stays for
    // mice, guarded so one physical pinch can never be handled twice.
    let gestureActive = false;
    let lastGestureScale = 1;
    interface GestureEventLike {
      scale?: number;
      clientX: number;
      clientY: number;
      preventDefault(): void;
    }
    const onGestureStart = (e: Event) => {
      e.preventDefault();
      gestureActive = true;
      lastGestureScale = 1;
    };
    const onGestureChange = (e: Event) => {
      const ge = e as unknown as GestureEventLike;
      ge.preventDefault();
      if (!ge.scale) return;
      viewer.zoomBy(ge.scale / lastGestureScale, anchorOf(ge));
      lastGestureScale = ge.scale;
      settleSoon();
    };
    const onGestureEnd = (e: Event) => {
      e.preventDefault();
      gestureActive = false;
      session.onManualZoomChange();
    };
    const onWheel = (e: WheelEvent) => {
      if (e.ctrlKey && !gestureActive) {
        e.preventDefault();
        // Clamp: fine pinch-style deltas pass through; mouse-wheel notches
        // (±100+) are capped so one notch ≈ one step.
        const delta = Math.max(-10, Math.min(10, e.deltaY));
        viewer.zoomBy(Math.exp(-delta * 0.03), anchorOf(e));
        settleSoon();
      } else if (!e.ctrlKey && viewer.isZoomed()) {
        e.preventDefault();
        viewer.panBy(e.deltaX * dpr(), e.deltaY * dpr());
      }
    };
    const onDblClick = (e: MouseEvent) => {
      if (viewer.isZoomed()) viewer.exitZoom();
      else viewer.zoomBy(Number.MAX_SAFE_INTEGER, anchorOf(e));
      session.onManualZoomChange();
    };
    let dragging = false;
    let onMinimap = false;
    let lastX = 0;
    let lastY = 0;
    const onPointerDown = (e: PointerEvent) => {
      if (!viewer.isZoomed()) return;
      const a = anchorOf(e);
      onMinimap = viewer.minimapNavigate(a.x, a.y);
      dragging = true;
      lastX = e.clientX;
      lastY = e.clientY;
      el.setPointerCapture(e.pointerId);
      if (onMinimap) settleSoon();
    };
    const onPointerMove = (e: PointerEvent) => {
      if (!dragging) return;
      if (onMinimap) {
        const a = anchorOf(e);
        viewer.minimapNavigate(a.x, a.y);
      } else {
        viewer.panBy((lastX - e.clientX) * dpr(), (lastY - e.clientY) * dpr());
      }
      lastX = e.clientX;
      lastY = e.clientY;
    };
    const onPointerUp = () => {
      dragging = false;
      onMinimap = false;
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    el.addEventListener('dblclick', onDblClick);
    el.addEventListener('pointerdown', onPointerDown);
    el.addEventListener('pointermove', onPointerMove);
    el.addEventListener('pointerup', onPointerUp);
    el.addEventListener('gesturestart', onGestureStart);
    el.addEventListener('gesturechange', onGestureChange);
    el.addEventListener('gestureend', onGestureEnd);
    return () => {
      el.removeEventListener('wheel', onWheel);
      el.removeEventListener('dblclick', onDblClick);
      el.removeEventListener('pointerdown', onPointerDown);
      el.removeEventListener('pointermove', onPointerMove);
      el.removeEventListener('pointerup', onPointerUp);
      el.removeEventListener('gesturestart', onGestureStart);
      el.removeEventListener('gesturechange', onGestureChange);
      el.removeEventListener('gestureend', onGestureEnd);
      if (settleTimer) clearTimeout(settleTimer);
    };
  }, []);

  const zoomPercent = useSyncExternalStore(viewer.subscribe, () => {
    const p = viewer.zoomPercent();
    return p === null ? null : Math.round(p * 100);
  });

  const photo = state.photos[state.cursor];
  const starred = state.all.filter((p) => p.rating > 0).length;
  const unstarred = state.all.length - starred;

  return (
    <div className="app">
      {showStrip && state.photos.length > 0 && (
        <Filmstrip photos={state.photos} cursor={state.cursor} />
      )}
      <div className="viewer-wrap">
        <canvas ref={canvasRef} className="viewer-canvas" />

        {showFaces && photo && state.currentFaces && state.currentFaces.length > 0 && (
          <FaceBadges faces={state.currentFaces} />
        )}

        {photo && (
          <div className="hud">
            <span className="hud-name">{photo.stem}</span>
            <span className="hud-badge">{pairBadge(photo.hasJpeg, photo.hasRaf)}</span>
            {photo.missing && <span className="hud-chip hud-warn">missing on disk</span>}
            <span className="hud-stars">{photo.rating > 0 ? '★'.repeat(photo.rating) : '·'}</span>
            <span className="hud-pos">
              {state.cursor + 1} / {state.photos.length}
            </span>
            {state.filter !== 'all' && (
              <span className="hud-chip">{FILTER_LABEL[state.filter]}</span>
            )}
            {(state.sort !== 'capture' || state.reverse) && (
              <span className="hud-chip">
                {state.sort}
                {state.reverse ? ' ↓' : ''}
              </span>
            )}
            {state.currentRecipe?.hasMeta && (
              <span className={`hud-chip${state.currentRecipe.name ? '' : ' hud-dim'}`}>
                {state.currentRecipe.name ?? 'unknown recipe'}
              </span>
            )}
            {state.tagFilter && <span className="hud-chip">#{state.tagFilter}</span>}
            {state.personFilter !== null && (
              <span className="hud-chip">
                @{state.persons.find((p) => p.id === state.personFilter)?.name ?? 'person'}
              </span>
            )}
            {photo.tags && photo.tags.length > 0 && (
              <span className="hud-chip hud-dim">
                {photo.tags.slice(0, 2).map((t) => `#${t}`).join(' ')}
                {photo.tags.length > 2 ? ` +${photo.tags.length - 2}` : ''}
              </span>
            )}
            {!state.autoAdvance && <span className="hud-chip">manual</span>}
            {zoomPercent !== null && <span className="hud-chip">{zoomPercent}%</span>}
            {state.blinkies && <span className="hud-chip">blinkies</span>}
            {!state.sortedByCapture && state.photos.length > 0 && (
              <span className="hud-note">sorting…</span>
            )}
          </div>
        )}

        {state.folder && (
          <div className="hud-right">
            {(state.recipeNames.length > 0 || state.recipeFilter) && (
              <select
                className="recipe-select"
                value={state.recipeFilter ?? ''}
                onChange={(e) =>
                  void session.setRecipeFilter(e.target.value === '' ? null : e.target.value)
                }
              >
                <option value="">all recipes</option>
                {state.recipeNames.map((n) => (
                  <option key={n} value={n}>
                    {n}
                  </option>
                ))}
                <option value={session.UNKNOWN_RECIPE}>unknown recipe</option>
              </select>
            )}
            <span className="hud-note">
              ★ {starred} · unstarred {unstarred}
            </span>
            {state.missingCount > 0 && (
              <span className="hud-chip hud-warn">missing {state.missingCount}</span>
            )}
            {state.xmpPending > 0 && <span className="hud-note">✎ {state.xmpPending} pending</span>}
            {state.xmpFailed > 0 && (
              <button
                className="hud-trash-btn hud-warn"
                title={state.xmpLastError ?? 'metadata write failed'}
                onClick={() => void session.retryXmpWrites()}
              >
                ⚠ {state.xmpFailed} failed · retry
              </button>
            )}
            <button
              className="hud-trash-btn"
              onClick={() => {
                setShowTrash((v) => !v);
                setShowPeople(false); // same dock — one panel at a time
              }}
              disabled={state.trashedCount === 0 && !showTrash}
            >
              trashed {state.trashedCount}
            </button>
          </div>
        )}

        {!state.folder && !state.loading && (
          <div className="overlay-msg">
            <div className="overlay-title">Ember</div>
            <div>Drop a folder here, or</div>
            <button onClick={openViaDialog}>Open folder ⌘O</button>
            <div className="overlay-hint">? shows all shortcuts</div>
          </div>
        )}

        {state.folder && state.photos.length === 0 && !state.loading && (
          <div className="overlay-msg">
            <div>
              {state.all.length > 0
                ? `No photos match the ${FILTER_LABEL[state.filter] ?? state.filter} filter.`
                : `No photos to show${state.trashedCount > 0 ? ' (everything is trashed)' : ''}.`}
            </div>
          </div>
        )}

        {state.loading && <div className="overlay-msg">Scanning…</div>}
        {state.error && <div className="overlay-msg error">{state.error}</div>}
        {keysError && (
          <div className="overlay-msg error">
            <div className="overlay-title">Keyboard disabled</div>
            <div>Keybindings failed to load: {keysError}</div>
            <button onClick={() => window.location.reload()}>Reload</button>
          </div>
        )}
        {state.notice && <div className="notice">{state.notice}</div>}

        {photo && state.histMode !== 'off' && (
          <HistogramPanel photoId={photo.id} mode={state.histMode} />
        )}
      </div>

      {showExif && photo && (
        <ExifPanel
          photoId={photo.id}
          recipe={state.currentRecipe}
          onRecipeSaved={() => void session.recipesChanged()}
        />
      )}

      {showTrash && state.folderId !== null && (
        <TrashPanel folderId={state.folderId} onClose={() => setShowTrash(false)} />
      )}
      {showPeople && state.folderId !== null && (
        <PeoplePanel
          folderId={state.folderId}
          trashedCount={state.trashedCount}
          peopleVersion={state.peopleVersion}
          onClose={() => setShowPeople(false)}
          notify={(m) => session.notify(m)}
          onJump={(id) => session.jumpToPhotoId(id)}
        />
      )}
      {showCheat && <CheatSheet onClose={() => setShowCheat(false)} />}
      {showRecipes && <RecipeSwitcher onClose={() => setShowRecipes(false)} />}
      {showTagPalette && <TagPalette onClose={() => setShowTagPalette(false)} />}
      {showTagFilter && <TagSwitcher onClose={() => setShowTagFilter(false)} />}
      {showPersonFilter && <PersonSwitcher onClose={() => setShowPersonFilter(false)} />}
      {showPerf && <PerfHud />}
    </div>
  );
}
