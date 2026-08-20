import * as imageCache from './imageCache';
import * as perf from './perf';
import * as viewer from './viewer';
import {
  facesEventApplies,
  facesForPhoto,
  focusMap,
  getFocus,
  getRecipe,
  listPersons,
  listRecipes,
  maskUrl,
  onFacesProgress,
  onFocusProgress,
  personMap,
  recipeMap,
  onPhotoMeta,
  onXmpPending,
  origUrl,
  redoAction,
  restorePhoto,
  retryXmpErrors,
  saveView,
  scanFolder,
  setCursor,
  setOrder,
  setRating,
  setTags,
  trashPhoto,
  undoAction,
  type FaceOut,
  type PersonOut,
} from './ipc';
import { clearMetaCache, ensureMeta } from './metaCache';
import {
  FILTERS,
  SORTS,
  comparator,
  orderHint,
  passesFilter,
  passesFocusFilter,
  passesPersonFilter,
  passesTagFilter,
  type FilterMode,
  type SortMode,
} from './order';
import type { Delta, Photo } from './types';

/** Preload window, forward-biased; mirrored when culling backwards. */
const AHEAD = 8;
const BEHIND = 3;

export type { FilterMode, SortMode } from './order';

export type HistMode = 'off' | 'lum' | 'rgb';

export interface SessionState {
  folder: string | null;
  folderId: number | null;
  /** Every non-trashed photo, in sort order. */
  all: Photo[];
  /** The filtered view the cursor moves through. */
  photos: Photo[];
  cursor: number;
  filter: FilterMode;
  sort: SortMode;
  reverse: boolean;
  autoAdvance: boolean;
  histMode: HistMode;
  blinkies: boolean;
  currentRecipe: { name: string | null; hasMeta: boolean } | null;
  recipeFilter: string | null; // recipe name, or UNKNOWN_RECIPE sentinel
  recipeNames: string[];
  tagFilter: string | null;
  /** Person filter (SPEC §14) — person id, session-only like the others. */
  personFilter: number | null;
  /** Focus check: photo id → AF-patch sharpness score (HUD chip, strip dots). */
  focusScores: Record<string, number>;
  /** settings.toml [focus] soft_threshold; null = display-only, no judgment. */
  focusSoftThreshold: number | null;
  /** Show only photos scoring below the threshold (Shift+A). */
  focusFilter: boolean;
  /** Folder-scoped people, for the switcher and the HUD chip label. */
  persons: PersonOut[];
  /** Faces of the current photo: null until scanned (badges stay silent). */
  currentFaces: FaceOut[] | null;
  /** Bumped on every people edit from anywhere — the panel watches it so a
   * correction made on the photo refreshes its counts and clusters. */
  peopleVersion: number;
  loading: boolean;
  error: string | null;
  notice: string | null;
  sortedByCapture: boolean;
  trashedCount: number;
  missingCount: number;
  xmpPending: number;
  /** Writes parked after repeated failures — visible until retried. */
  xmpFailed: number;
  xmpLastError: string | null;
}

let state: SessionState = {
  folder: null,
  folderId: null,
  all: [],
  photos: [],
  cursor: 0,
  filter: 'all',
  sort: 'capture',
  reverse: false,
  autoAdvance: localStorage.getItem('autoAdvance') !== '0',
  histMode: (['off', 'lum', 'rgb'].includes(localStorage.getItem('histMode') ?? '')
    ? localStorage.getItem('histMode')
    : 'off') as HistMode,
  blinkies: localStorage.getItem('blinkies') === '1',
  currentRecipe: null,
  recipeFilter: null,
  recipeNames: [],
  tagFilter: null,
  personFilter: null,
  focusScores: {},
  focusSoftThreshold: null,
  focusFilter: false,
  persons: [],
  currentFaces: null,
  peopleVersion: 0,
  loading: false,
  error: null,
  notice: null,
  sortedByCapture: false,
  trashedCount: 0,
  missingCount: 0,
  xmpPending: 0,
  xmpFailed: 0,
  xmpLastError: null,
};

let direction: 1 | -1 = 1;
let listeners: Array<() => void> = [];
let started = false;
/** Photos removed by trash this session, for instant undo re-insertion. */
const removed = new Map<string, Photo>();
let noticeTimer: ReturnType<typeof setTimeout> | null = null;

function setState(patch: Partial<SessionState>): void {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

export function getState(): SessionState {
  return state;
}

export function subscribe(fn: () => void): () => void {
  listeners.push(fn);
  return () => {
    listeners = listeners.filter((l) => l !== fn);
  };
}

/** Idempotent one-time wiring of backend events. */
export function start(): void {
  if (started) return;
  started = true;
  void onPhotoMeta(applyPhotoMeta);
  void onFacesProgress((p) => {
    // Stale event from a folder we've since left — drop it. A folderless
    // event (terminal engine failure) always applies.
    if (!facesEventApplies(p, state.folderId)) return;
    for (const id of p.photoIds) faceCache.delete(id);
    const current = currentPhoto();
    if (current && p.photoIds.includes(current.id)) void refreshFaces(current.id);
    schedulePersonRefresh();
  });
  void onFocusProgress((p) => {
    if (p.folderId !== null && p.folderId !== state.folderId) return;
    scheduleFocusRefresh();
  });
  void onXmpPending((s) => {
    // A write just got parked as permanently failed: say so once, loudly.
    if (s.failed > state.xmpFailed) {
      showNotice(
        `A metadata write failed${s.lastError ? ` (${s.lastError})` : ''} — press ⚠ retry in the corner`,
      );
    }
    setState({ xmpPending: s.pending, xmpFailed: s.failed, xmpLastError: s.lastError });
  });
}

/** People data (membership map, person list) failing must never be mistaken
 * for "this person has no photos": we keep whatever we had and say so — once,
 * so a persistent failure during a scan can't spam the HUD. */
let personDataOk = true;
function personDataFailed(what: string, e: unknown): void {
  if (personDataOk) showNotice(`${what} unavailable — ${String(e)}`);
  personDataOk = false;
}

/** Live person-filter membership while a scan runs: matches arrive in bounded
 * batches (one refetch per quiet second) instead of a rebuild per photo. */
let personRefreshTimer: ReturnType<typeof setTimeout> | null = null;
function schedulePersonRefresh(): void {
  if (state.personFilter === null || state.folderId === null) return;
  if (personRefreshTimer) return;
  personRefreshTimer = setTimeout(() => {
    personRefreshTimer = null;
    const { folderId } = state;
    if (state.personFilter === null || folderId === null) return;
    void personMap(folderId)
      .then((map) => {
        if (state.folderId !== folderId) return; // folder changed mid-flight
        personDataOk = true;
        personMapCache = map;
        rebuild(state.all, {}, currentPhoto()?.id ?? null);
      })
      .catch((e: unknown) => personDataFailed('Live person matches', e));
  }, 1000);
}

// ---------- focus check ----------

/** Fetch folder scores; guard against a folder change mid-flight. When the
 * focus filter is live, matches stream in like the person filter's. */
async function loadFocusScores(): Promise<void> {
  const { folderId } = state;
  if (folderId === null) return;
  try {
    const map = await focusMap(folderId);
    if (state.folderId !== folderId) return;
    setState({ focusScores: map.scores, focusSoftThreshold: map.softThreshold });
    if (state.focusFilter) rebuild(state.all, {}, currentPhoto()?.id ?? null);
  } catch {
    // Derived data — keep what we had; the next progress event retries.
  }
}

/** Live scores while the sweep runs: one refetch per quiet second, not one
 * state update per photo. */
let focusRefreshTimer: ReturnType<typeof setTimeout> | null = null;
function scheduleFocusRefresh(): void {
  if (state.folderId === null || focusRefreshTimer) return;
  focusRefreshTimer = setTimeout(() => {
    focusRefreshTimer = null;
    void loadFocusScores();
  }, 1000);
}

/** Shift+A: show only photos soft at the AF point — needs the user's own
 * threshold, because the raw score is texture-dependent and Ember does not
 * judge on its own. */
export function toggleFocusFilter(): void {
  if (!state.focusFilter && state.focusSoftThreshold === null) {
    showNotice('Set soft_threshold under [focus] in settings.toml to filter by focus');
    return;
  }
  rebuild(state.all, { focusFilter: !state.focusFilter }, currentPhoto()?.id ?? null);
}

function showNotice(msg: string): void {
  setState({ notice: msg });
  if (noticeTimer) clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => setState({ notice: null }), 4000);
}

/** For chrome outside this module (keymap warnings etc.). */
export function notify(msg: string): void {
  showNotice(msg);
}

/** Re-arm permanently-failed metadata writes (the ⚠ chip). */
export async function retryXmpWrites(): Promise<void> {
  try {
    const s = await retryXmpErrors();
    setState({ xmpPending: s.pending, xmpFailed: s.failed, xmpLastError: s.lastError });
    showNotice('Retrying metadata writes…');
  } catch (e) {
    showNotice(`Retry failed: ${String(e)}`);
  }
}

function currentPhoto(): Photo | undefined {
  return state.photos[state.cursor];
}

// ---------- sorting & filtering (pure logic lives in order.ts) ----------

export const UNKNOWN_RECIPE = '__unknown__';

const recipeCache = new Map<string, { name: string | null; hasMeta: boolean }>();
let recipeMapCache: Record<string, string | null> = {};
/** photo id → person ids (SPEC §14); refreshed as the face worker progresses. */
let personMapCache: Record<string, number[]> = {};
/** Per-photo face rows for the HUD badges, invalidated by photo id. */
const faceCache = new Map<string, FaceOut[] | null>();

function passesRecipeFilter(p: Photo, recipeFilter: string | null): boolean {
  if (!recipeFilter) return true;
  const entry = recipeMapCache[p.id];
  if (recipeFilter === UNKNOWN_RECIPE) return entry === null || entry === undefined;
  return entry === recipeFilter;
}

/**
 * Re-derive sorted `all` + filtered `photos`, repositioning the cursor.
 * `focusId`: photo to place the cursor on (advance: one past it).
 * If it isn't visible (filtered out / trashed), the cursor holds its slot so
 * the next photo slides into view — the multi-pass culling rhythm.
 */
function rebuild(
  all: Photo[],
  patch: Partial<SessionState>,
  focusId: string | null,
  advance = false,
): void {
  const sort = (patch.sort ?? state.sort) as SortMode;
  const reverse = patch.reverse ?? state.reverse;
  const filter = (patch.filter ?? state.filter) as FilterMode;
  const recipeFilter =
    patch.recipeFilter !== undefined ? patch.recipeFilter : state.recipeFilter;
  const tagFilter = patch.tagFilter !== undefined ? patch.tagFilter : state.tagFilter;
  const personFilter =
    patch.personFilter !== undefined ? patch.personFilter : state.personFilter;
  const focusFilter = patch.focusFilter !== undefined ? patch.focusFilter : state.focusFilter;
  const sortedAll = [...all].sort(comparator(sort, reverse));
  const photos = sortedAll.filter(
    (p) =>
      passesFilter(p, filter) &&
      passesRecipeFilter(p, recipeFilter) &&
      passesTagFilter(p, tagFilter) &&
      passesPersonFilter(p, personFilter, personMapCache) &&
      passesFocusFilter(p, focusFilter, state.focusSoftThreshold, state.focusScores),
  );
  let cursor: number;
  const focusIdx = focusId ? photos.findIndex((p) => p.id === focusId) : -1;
  if (focusIdx >= 0) {
    cursor = advance ? Math.min(focusIdx + 1, photos.length - 1) : focusIdx;
  } else {
    cursor = Math.min(state.cursor, Math.max(0, photos.length - 1));
  }
  setState({ ...patch, all: sortedAll, photos, cursor });
  showCurrent(null);
  schedulePreload();
  setOrder(orderHint(photos, sortedAll));
  persistView();
}

// ---------- preloading ----------

function protectedIds(): Set<string> {
  const ids = new Set<string>();
  for (const d of [-1, 0, 1]) {
    const p = state.photos[state.cursor + d];
    if (p) ids.add(p.id);
  }
  return ids;
}

function schedulePreload(): void {
  const { photos, cursor } = state;
  if (!photos.length) return;
  const ahead = direction === 1 ? AHEAD : BEHIND;
  const behind = direction === 1 ? BEHIND : AHEAD;
  const offsets: number[] = [];
  for (let i = 1; i <= Math.max(ahead, behind); i++) {
    if (i <= ahead) offsets.push(direction * i);
    if (i <= behind) offsets.push(-direction * i);
  }
  const windowIds = new Set<string>();
  const current = photos[cursor];
  if (current) windowIds.add(current.id);
  const targets: Photo[] = [];
  for (const off of offsets) {
    const p = photos[cursor + off];
    if (p) {
      windowIds.add(p.id);
      targets.push(p);
    }
  }
  imageCache.abortOutside(windowIds);
  const protect = protectedIds();
  // Launch in priority order; the backend's worker pool preserves rough FIFO.
  for (const p of targets) {
    if (!imageCache.get(p.id)) void imageCache.ensure(p, protect);
  }
}

/** Persist the view on every change — fire-and-forget, a single tiny UPDATE
 * off the hot path. Debouncing here cost exactness on quit. */
function persistView(): void {
  const { folderId } = state;
  if (folderId === null) return;
  saveView(folderId, state.sort, state.reverse, state.filter, currentPhoto()?.id ?? null);
}

function dimsOf(photo: Photo): { w: number; h: number } | null {
  return photo.pixW && photo.pixH ? { w: photo.pixW, h: photo.pixH } : null;
}

function showCurrent(eventTs: number | null): void {
  const photo = currentPhoto();
  if (!photo) {
    viewer.setPhoto(null, null);
    return;
  }
  const hit = imageCache.get(photo.id);
  if (hit) {
    viewer.setPhoto(hit.bitmap, dimsOf(photo));
    if (eventTs !== null) perf.recordFlip(eventTs, 'bitmap-cache', photo.id);
  } else {
    viewer.setPhoto(null, dimsOf(photo));
    void imageCache.ensure(photo, protectedIds()).then((entry) => {
      if (currentPhoto()?.id !== photo.id) return; // user already moved on
      viewer.setPhoto(entry?.bitmap ?? null, dimsOf(photo));
      if (entry && eventTs !== null) perf.recordFlip(eventTs, entry.servedFrom, photo.id);
      afterShow(photo);
    });
    return;
  }
  afterShow(photo);
}

/** Post-display concerns: full-res for zoom, AF overlay data, face badges. */
function afterShow(photo: Photo): void {
  // Show cached faces synchronously (no flash between flips), fetch otherwise.
  const cachedFaces = faceCache.get(photo.id);
  if (cachedFaces !== undefined) {
    if (state.currentFaces !== cachedFaces) setState({ currentFaces: cachedFaces });
  } else {
    setState({ currentFaces: null });
    void refreshFaces(photo.id);
  }
  if (viewer.isZoomed()) {
    kickFullres(photo);
    for (const d of [1, -1]) {
      const n = state.photos[state.cursor + d];
      if (n) void ensureFullres(n);
    }
  }
  if (afOverlay) {
    void focusFor(photo.id).then((p) => {
      if (currentPhoto()?.id !== photo.id) return;
      viewer.setAf(p ? { x: p[0], y: p[1] } : null, true);
    });
  } else {
    viewer.setAf(null, false);
  }
  if (state.blinkies) {
    void ensureMask(photo).then((bitmap) => {
      if (currentPhoto()?.id !== photo.id) return;
      viewer.setBlinkies(bitmap);
    });
  }
  // Warm the panel + recipe caches for neighbors so the next flip renders
  // its panel content in a single pass (no flash).
  for (const d of [1, -1, 2, -2]) {
    const n = state.photos[state.cursor + d];
    if (!n) continue;
    void ensureMeta(n.id);
    if (!recipeCache.has(n.id)) {
      void getRecipe(n.id)
        .then((r) => {
          if (recipeCache.size > 200) recipeCache.clear();
          recipeCache.set(n.id, { name: r.name, hasMeta: r.hasMeta });
        })
        .catch(() => {});
    }
  }
  const cachedRecipe = recipeCache.get(photo.id);
  if (cachedRecipe) {
    if (state.currentRecipe !== cachedRecipe) setState({ currentRecipe: cachedRecipe });
  } else {
    setState({ currentRecipe: null });
    void getRecipe(photo.id)
      .then((r) => {
        const entry = { name: r.name, hasMeta: r.hasMeta };
        if (recipeCache.size > 200) recipeCache.clear();
        recipeCache.set(photo.id, entry);
        if (currentPhoto()?.id === photo.id) setState({ currentRecipe: entry });
      })
      .catch(() => {});
  }
}

// ---------- recipes ----------

export async function setRecipeFilter(recipeFilter: string | null): Promise<void> {
  if (recipeFilter && state.folderId !== null) {
    recipeMapCache = await recipeMap(state.folderId).catch(() => ({}));
  }
  rebuild(state.all, { recipeFilter }, currentPhoto()?.id ?? null);
}

// ---------- tags ----------

export function setTagFilter(tagFilter: string | null): void {
  rebuild(state.all, { tagFilter }, currentPhoto()?.id ?? null);
}

// ---------- people (SPEC §14) ----------

export async function setPersonFilter(personFilter: number | null): Promise<void> {
  const folderId = state.folderId;
  if (personFilter !== null && folderId !== null) {
    try {
      const map = await personMap(folderId);
      // Every people answer is checked against the folder it was asked for:
      // person ids and photo ids belong to one folder, and applying a slow
      // reply to the folder the user has since opened would filter it with
      // another folder's membership.
      if (state.folderId !== folderId) return;
      personMapCache = map;
      personDataOk = true;
    } catch (e) {
      // Filtering against an empty map hides every photo and reads exactly
      // like "this person isn't in this folder". Leave the view alone.
      if (state.folderId === folderId) personDataFailed('Person filter', e);
      return;
    }
  }
  rebuild(state.all, { personFilter }, currentPhoto()?.id ?? null);
}

export function loadPersons(): void {
  const folderId = state.folderId;
  if (folderId === null) return;
  void listPersons(folderId)
    .then((persons) => {
      if (state.folderId !== folderId) return; // another folder's roster
      personDataOk = true;
      setState({ persons });
    })
    .catch((e: unknown) => {
      if (state.folderId === folderId) personDataFailed('People list', e);
    });
}

/** Panel edits (naming, corrections, dismissals) change names, counts, badges
 * and filter membership all at once — one refresh covers the lot. */
export async function peopleChanged(): Promise<void> {
  faceCache.clear();
  loadPersons();
  const folderId = state.folderId;
  if (folderId !== null) {
    try {
      const map = await personMap(folderId);
      if (state.folderId !== folderId) return; // the folder moved under us
      personMapCache = map;
      personDataOk = true;
    } catch (e) {
      // Keep the membership we had rather than emptying the filtered view.
      if (state.folderId !== folderId) return;
      personDataFailed('Person data', e);
    }
  }
  rebuild(state.all, { peopleVersion: state.peopleVersion + 1 }, currentPhoto()?.id ?? null);
  const photo = currentPhoto();
  if (photo) void refreshFaces(photo.id);
}

/** Badges for the current photo, post-display and cache-backed: never on the
 * flip critical path, and a cached photo re-renders its badges with no IPC. */
async function refreshFaces(photoId: string): Promise<void> {
  if (faceCache.has(photoId)) {
    const hit = faceCache.get(photoId) ?? null;
    if (currentPhoto()?.id === photoId) setState({ currentFaces: hit });
    return;
  }
  const faces = await facesForPhoto(photoId).catch(() => null);
  if (faceCache.size > 300) faceCache.clear();
  faceCache.set(photoId, faces);
  if (currentPhoto()?.id === photoId) setState({ currentFaces: faces });
}

/** Toggle a tag on the current photo. Journaled + undoable; no auto-advance —
 * tags are additive, unlike the one-verdict-per-photo rating rhythm. */
export async function toggleTag(tag: string): Promise<void> {
  const photo = currentPhoto();
  if (!photo || state.folderId === null) return;
  const cur = photo.tags ?? [];
  const next = cur.includes(tag) ? cur.filter((t) => t !== tag) : [...cur, tag];
  try {
    await setTags(state.folderId, photo.id, next); // journal precedes UI ack
  } catch (e) {
    showNotice(`Tag failed — ${String(e)}`);
    return;
  }
  const all = state.all.map((p) => (p.id === photo.id ? { ...p, tags: next } : p));
  rebuild(all, {}, photo.id);
}

export function loadRecipeNames(): void {
  void listRecipes()
    .then((recipeNames) => setState({ recipeNames }))
    .catch(() => {});
}

/** After saving a recipe: labels may change everywhere. */
export async function recipesChanged(): Promise<void> {
  recipeCache.clear();
  recipeMapCache = {};
  loadRecipeNames();
  if (state.recipeFilter && state.folderId !== null) {
    recipeMapCache = await recipeMap(state.folderId).catch(() => ({}));
    rebuild(state.all, {}, currentPhoto()?.id ?? null);
  }
  const photo = currentPhoto();
  if (photo) afterShow(photo);
}

// ---------- full-res (zoom mode only) ----------

const FULLRES_MAX = 3;
const fullresCache = new Map<string, ImageBitmap>();
const fullresPending = new Set<string>();

async function ensureFullres(photo: Photo): Promise<ImageBitmap | null> {
  const hit = fullresCache.get(photo.id);
  if (hit && hit.width > 0) {
    // LRU touch: re-insert at the end so eviction takes the coldest.
    fullresCache.delete(photo.id);
    fullresCache.set(photo.id, hit);
    return hit;
  }
  if (fullresPending.has(photo.id)) return null;
  fullresPending.add(photo.id);
  try {
    const resp = await fetch(origUrl(photo.id));
    if (!resp.ok) return null;
    const bitmap = await createImageBitmap(await resp.blob());
    fullresCache.set(photo.id, bitmap);
    const currentId = currentPhoto()?.id;
    for (const victim of fullresCache.keys()) {
      if (fullresCache.size <= FULLRES_MAX) break;
      // Never close the bitmap the viewer may be drawing right now.
      if (victim === currentId) continue;
      fullresCache.get(victim)?.close();
      fullresCache.delete(victim);
    }
    return bitmap;
  } catch {
    return null;
  } finally {
    fullresPending.delete(photo.id);
  }
}

function kickFullres(photo: Photo): void {
  const hit = fullresCache.get(photo.id);
  if (hit) {
    viewer.setFullres(hit);
    return;
  }
  void ensureFullres(photo).then((bitmap) => {
    if (bitmap && currentPhoto()?.id === photo.id && viewer.isZoomed()) {
      viewer.setFullres(bitmap); // progressive sharpen lands
    }
  });
}

function dropFullres(): void {
  for (const b of fullresCache.values()) b.close();
  fullresCache.clear();
}

// ---------- zoom + AF actions ----------

let afOverlay = false;
const focusCache = new Map<string, [number, number] | null>();

async function focusFor(id: string): Promise<[number, number] | null> {
  if (focusCache.has(id)) return focusCache.get(id) ?? null;
  const p = await getFocus(id).catch(() => null);
  focusCache.set(id, p);
  return p;
}

export function toggleZoom(): void {
  const photo = currentPhoto();
  if (!photo) return;
  if (viewer.isZoomed()) {
    viewer.exitZoom();
    dropFullres();
  } else {
    viewer.toggle100();
    afterShow(photo);
  }
}

/** F toggles: zoom 100% onto the AF point; from any zoom, F returns to fit
 * (same as Z). Locked zoom carries the view across flips, so the exit branch
 * must not care which photo started the zoom. */
export async function focusZoom(): Promise<void> {
  const photo = currentPhoto();
  if (!photo) return;
  if (viewer.isZoomed()) {
    viewer.exitZoom();
    dropFullres();
    return;
  }
  const p = await focusFor(photo.id);
  if (currentPhoto()?.id !== photo.id) return;
  if (!p) {
    showNotice('No AF point recorded for this photo');
    return;
  }
  viewer.zoomToPoint(p[0], p[1]);
  afterShow(photo);
}

export function toggleAfOverlay(): void {
  afOverlay = !afOverlay;
  const photo = currentPhoto();
  if (photo) afterShow(photo);
}

export function onManualZoomChange(): void {
  const photo = currentPhoto();
  if (!photo) return;
  if (viewer.isZoomed()) afterShow(photo);
  else dropFullres();
}

// ---------- navigation ----------

/** Jump to an absolute position in the visible list (Home/End, strip clicks). */
/** Put the cursor on a specific photo (People panel chip → context). */
export function jumpToPhotoId(id: string): void {
  const idx = state.photos.findIndex((p) => p.id === id);
  if (idx >= 0) {
    jumpTo(idx);
  } else if (state.all.some((p) => p.id === id)) {
    showNotice('That photo is hidden by the current filter');
  }
}

export function jumpTo(index: number): void {
  const { photos } = state;
  if (!photos.length) return;
  const next = Math.min(photos.length - 1, Math.max(0, index));
  if (next === state.cursor) return;
  direction = 1;
  setState({ cursor: next });
  showCurrent(null);
  const photo = photos[next];
  if (photo) setCursor(photo.id);
  schedulePreload();
  persistView();
}

/** The flip hot path: called straight from the keydown handler. */
export function flip(delta: number, eventTs: number): void {
  const { photos, cursor } = state;
  if (!photos.length) return;
  const next = Math.min(photos.length - 1, Math.max(0, cursor + delta));
  if (next === cursor) return;
  direction = delta > 0 ? 1 : -1;
  setState({ cursor: next });
  showCurrent(eventTs);
  const photo = photos[next];
  if (photo) setCursor(photo.id);
  schedulePreload();
  persistView();
}

// ---------- view controls ----------

export function cycleFilter(): void {
  const order: FilterMode[] = ['all', 'unstarred', 'starred'];
  const idx = order.indexOf(state.filter as (typeof order)[number]);
  setFilter(order[(idx + 1) % order.length] ?? 'all');
}

export function setFilter(filter: FilterMode): void {
  if (!FILTERS.includes(filter)) return;
  rebuild(state.all, { filter }, currentPhoto()?.id ?? null);
}

export function cycleSort(): void {
  const idx = SORTS.indexOf(state.sort);
  rebuild(state.all, { sort: SORTS[(idx + 1) % SORTS.length] }, currentPhoto()?.id ?? null);
}

export function toggleReverse(): void {
  rebuild(state.all, { reverse: !state.reverse }, currentPhoto()?.id ?? null);
}

export function toggleAutoAdvance(): void {
  const autoAdvance = !state.autoAdvance;
  localStorage.setItem('autoAdvance', autoAdvance ? '1' : '0');
  setState({ autoAdvance });
}

export function cycleHistogram(): void {
  // Color first: it's the mode used most while culling.
  const order: HistMode[] = ['off', 'rgb', 'lum'];
  const histMode = order[(order.indexOf(state.histMode) + 1) % order.length];
  localStorage.setItem('histMode', histMode);
  setState({ histMode });
}

export function toggleBlinkies(): void {
  const blinkies = !state.blinkies;
  localStorage.setItem('blinkies', blinkies ? '1' : '0');
  setState({ blinkies });
  const photo = currentPhoto();
  if (photo) afterShow(photo);
  if (!blinkies) viewer.setBlinkies(null);
}

// Small owned-bitmap cache for clipping masks (a few KB each decoded small).
const MASK_MAX = 8;
const maskCache = new Map<string, ImageBitmap | null>();

async function ensureMask(photo: Photo): Promise<ImageBitmap | null> {
  if (maskCache.has(photo.id)) {
    const hit = maskCache.get(photo.id) ?? null;
    if (hit === null || hit.width > 0) return hit;
  }
  try {
    const resp = await fetch(maskUrl(photo.id));
    if (!resp.ok) {
      maskCache.set(photo.id, null);
      return null;
    }
    const bitmap = await createImageBitmap(await resp.blob());
    maskCache.set(photo.id, bitmap);
    while (maskCache.size > MASK_MAX) {
      const victim = maskCache.keys().next().value;
      if (victim === undefined) break;
      maskCache.get(victim)?.close();
      maskCache.delete(victim);
    }
    return bitmap;
  } catch {
    return null;
  }
}

// ---------- verdicts ----------

/** Rate the current photo. Journal write is awaited BEFORE auto-advance:
 * an acknowledged star can never be lost, even to kill -9 — and a REFUSED
 * star must be equally loud: no advance, no local state, a visible notice. */
export async function rate(rating: number, _eventTs: number): Promise<void> {
  const photo = currentPhoto();
  const { folderId } = state;
  if (!photo || folderId === null) return;
  try {
    await setRating(folderId, photo.id, rating);
  } catch (e) {
    showNotice(`Rating NOT saved — ${String(e)}`);
    return;
  }
  const all = state.all.map((p) => (p.id === photo.id ? { ...p, rating } : p));
  const advance = state.autoAdvance && rating > 0;
  rebuild(all, {}, photo.id, advance);
}

/** Trash the current photo pair (both files or nothing → system Trash). */
export async function trashCurrent(): Promise<void> {
  const photo = currentPhoto();
  const { folderId } = state;
  if (!photo || folderId === null) return;
  try {
    await trashPhoto(folderId, photo.id);
  } catch (e) {
    showNotice(String(e));
    return;
  }
  removed.set(photo.id, photo);
  rebuild(
    state.all.filter((p) => p.id !== photo.id),
    { trashedCount: state.trashedCount + 1 },
    null,
  );
}

function applyDelta(delta: Delta | null): void {
  if (!delta) return;
  if (delta.error) {
    showNotice(`${delta.kind}: ${delta.error}`);
  }
  if (delta.rating !== null) {
    const all = state.all.map((p) =>
      p.id === delta.photoId ? { ...p, rating: delta.rating as number } : p,
    );
    rebuild(all, {}, delta.photoId);
  }
  if (delta.tags !== null) {
    const all = state.all.map((p) =>
      p.id === delta.photoId ? { ...p, tags: delta.tags as string[] } : p,
    );
    rebuild(all, {}, delta.photoId);
  }
  if (delta.trashed === false) {
    // Photo came back from the Trash — put the cursor on it.
    const photo = removed.get(delta.photoId);
    if (photo) {
      removed.delete(delta.photoId);
      rebuild(
        [...state.all, photo],
        { trashedCount: Math.max(0, state.trashedCount - 1) },
        delta.photoId,
      );
    } else {
      // Restored a photo trashed in an earlier session — full refresh.
      void refresh();
      setState({ trashedCount: Math.max(0, state.trashedCount - 1) });
    }
  }
  if (delta.trashed === true) {
    const photo = state.all.find((p) => p.id === delta.photoId);
    if (photo) removed.set(photo.id, photo);
    rebuild(
      state.all.filter((p) => p.id !== delta.photoId),
      { trashedCount: state.trashedCount + 1 },
      null,
    );
  }
}

export async function undo(): Promise<void> {
  if (state.folderId === null) return;
  try {
    applyDelta(await undoAction(state.folderId));
  } catch (e) {
    showNotice(`Undo failed — ${String(e)}`);
  }
}

export async function redo(): Promise<void> {
  if (state.folderId === null) return;
  try {
    applyDelta(await redoAction(state.folderId));
  } catch (e) {
    showNotice(`Redo failed — ${String(e)}`);
  }
}

/** Restore from the trash panel (may predate this session). */
export async function restoreFromTrash(photoId: string): Promise<void> {
  if (state.folderId === null) return;
  try {
    await restorePhoto(state.folderId, photoId);
  } catch (e) {
    showNotice(`Restore failed — ${String(e)}`);
    return;
  }
  applyDelta({ photoId, kind: 'restore', rating: null, tags: null, trashed: false, error: null });
}

// ---------- folder lifecycle ----------

function validSort(s: string): SortMode {
  return (SORTS as string[]).includes(s) ? (s as SortMode) : 'capture';
}

function validFilter(f: string): FilterMode {
  return (FILTERS as string[]).includes(f) ? (f as FilterMode) : 'all';
}

export async function openFolder(dir: string): Promise<void> {
  const t0 = performance.now();
  setState({ loading: true, error: null });
  try {
    const result = await scanFolder(dir);
    imageCache.clear();
    perf.reset();
    removed.clear();
    direction = 1;
    const sort = validSort(result.state.sort);
    const reverse = result.state.reverse;
    const filter = validFilter(result.state.filter);
    const sortedAll = [...result.photos].sort(comparator(sort, reverse));
    const photos = sortedAll.filter((p) => passesFilter(p, filter));
    const cursorId = result.state.cursorPhoto;
    const cursor = cursorId
      ? Math.max(
          0,
          photos.findIndex((p) => p.id === cursorId),
        )
      : 0;
    setState({
      folder: dir,
      folderId: result.state.folderId,
      all: sortedAll,
      photos,
      cursor,
      sort,
      reverse,
      filter,
      loading: false,
      sortedByCapture: false,
      trashedCount: result.trashedCount,
      missingCount: result.missingCount,
      currentRecipe: null,
      recipeFilter: null,
      personFilter: null,
      focusScores: {},
      focusFilter: false,
      persons: [],
      currentFaces: null,
    });
    recipeCache.clear();
    recipeMapCache = {};
    personMapCache = {};
    faceCache.clear();
    clearMetaCache();
    loadRecipeNames();
    loadPersons();
    void loadFocusScores();
    const current = state.photos[cursor];
    if (!current) {
      viewer.render(null);
      return;
    }
    setCursor(current.id);
    const entry = await imageCache.ensure(current, new Set([current.id]));
    if (currentPhoto()?.id === current.id) {
      viewer.render(entry?.bitmap ?? null);
      perf.markColdOpen(performance.now() - t0);
    }
    schedulePreload();
  } catch (e) {
    setState({ loading: false, error: String(e) });
  }
}

/** Re-scan the current folder in place (R key, external changes, restores). */
export async function refresh(): Promise<void> {
  const { folder } = state;
  const keepId = currentPhoto()?.id ?? null;
  if (!folder) return;
  let result;
  try {
    result = await scanFolder(folder);
  } catch (e) {
    showNotice(`Rescan failed — ${String(e)}`);
    return;
  }
  // Preserve capture times we already know; scan results don't carry them.
  const known = new Map(state.all.map((p) => [p.id, p.captureTs]));
  const merged = result.photos.map((p) =>
    known.has(p.id) ? { ...p, captureTs: known.get(p.id) } : p,
  );
  removed.clear();
  recipeCache.clear();
  recipeMapCache = {};
  faceCache.clear();
  clearMetaCache();
  // A rescan can re-detect faces (pixel changes mark scans stale), so the
  // person map and people list are refetched rather than assumed.
  void peopleChanged();
  void loadFocusScores();
  rebuild(merged, { trashedCount: result.trashedCount, missingCount: result.missingCount }, keepId);
  showNotice('Folder rescanned');
}

/** Background EXIF pass landed: capture times, adopted ratings, dims; re-sort. */
function applyPhotoMeta(meta: {
  times: Record<string, number>;
  adopted: Record<string, number>;
  dims: Record<string, [number, number]>;
}): void {
  if (!state.all.length) return;
  const currentId = currentPhoto()?.id ?? null;
  const all = state.all.map((p) => {
    const next = { ...p };
    if (meta.times[p.id] !== undefined) next.captureTs = meta.times[p.id];
    if (meta.adopted[p.id] !== undefined && next.rating === 0) {
      next.rating = meta.adopted[p.id];
    }
    const d = meta.dims[p.id];
    if (d) {
      next.pixW = d[0];
      next.pixH = d[1];
    }
    return next;
  });
  rebuild(all, { sortedByCapture: true }, currentId);
}
