import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import type { Delta, PerfReport, ScanResult, TrashedPhoto } from './types';

export async function pickFolder(): Promise<string | null> {
  const result = await open({ directory: true, multiple: false, title: 'Open photo folder' });
  return typeof result === 'string' ? result : null;
}

export function scanFolder(dir: string): Promise<ScanResult> {
  return invoke<ScanResult>('scan_folder', { dir });
}

// ---------- verdicts (awaited: journal write precedes UI acknowledgment) ----------

export function setRating(folderId: number, photoId: string, rating: number): Promise<void> {
  return invoke('set_rating', { folderId, photoId, rating });
}

export function setTags(folderId: number, photoId: string, tags: string[]): Promise<void> {
  return invoke('set_tags', { folderId, photoId, tags });
}

/** Tag vocabulary from tags.toml — re-read on every call, so edits are live. */
export function getTagVocab(): Promise<string[]> {
  return invoke<string[]>('get_tag_vocab');
}

export function trashPhoto(folderId: number, photoId: string): Promise<void> {
  return invoke('trash_photo', { folderId, photoId });
}

export function undoAction(folderId: number): Promise<Delta | null> {
  return invoke<Delta | null>('undo', { folderId });
}

export function redoAction(folderId: number): Promise<Delta | null> {
  return invoke<Delta | null>('redo', { folderId });
}

export function restorePhoto(folderId: number, photoId: string): Promise<void> {
  return invoke('restore_photo', { folderId, photoId });
}

export function trashedList(folderId: number): Promise<TrashedPhoto[]> {
  return invoke<TrashedPhoto[]>('trashed_list', { folderId });
}

export function saveView(
  folderId: number,
  sort: string,
  reverse: boolean,
  filter: string,
  cursor: string | null,
): void {
  void invoke('save_view', { folderId, sort, reverse, filter, cursor }).catch(() => {});
}

export interface XmpStatus {
  pending: number;
  failed: number;
  lastError: string | null;
}

export function xmpPending(): Promise<XmpStatus> {
  return invoke<XmpStatus>('xmp_pending');
}

export function retryXmpErrors(): Promise<XmpStatus> {
  return invoke<XmpStatus>('retry_xmp_errors');
}

export interface KeyBinding {
  action: string;
  keys: string[];
  label: string;
}

export interface KeymapResult {
  bindings: KeyBinding[];
  /** Set when keymap.toml exists but was unusable — defaults are active. */
  warning: string | null;
}

export function getKeymap(): Promise<KeymapResult> {
  return invoke<KeymapResult>('get_keymap');
}

// ---------- hints (fire-and-forget, never on the flip critical path) ----------

export function setCursor(id: string): void {
  void invoke('set_cursor', { id }).catch(() => {});
}

export function setOrder(ids: string[]): void {
  void invoke('set_order', { ids }).catch(() => {});
}

export function savePerfReport(report: PerfReport): Promise<string> {
  return invoke<string>('save_perf_report', { report });
}

// ---------- events ----------

export interface PhotoMeta {
  times: Record<string, number>;
  adopted: Record<string, number>;
  dims: Record<string, [number, number]>;
}

export function getFocus(photoId: string): Promise<[number, number] | null> {
  return invoke<[number, number] | null>('get_focus', { photoId });
}

export function getMetadata(photoId: string): Promise<string | null> {
  return invoke<string | null>('get_metadata', { photoId });
}

export interface RecipeResult {
  name: string | null;
  matches: string[];
  hasMeta: boolean;
}

export function getRecipe(photoId: string): Promise<RecipeResult> {
  return invoke<RecipeResult>('get_recipe', { photoId });
}

export function recipeMap(folderId: number): Promise<Record<string, string | null>> {
  return invoke<Record<string, string | null>>('recipe_map', { folderId });
}

export function listRecipes(): Promise<string[]> {
  return invoke<string[]>('list_recipes');
}

export function saveRecipe(photoId: string, name: string): Promise<void> {
  return invoke('save_recipe', { photoId, name });
}

export function onPhotoMeta(cb: (meta: PhotoMeta) => void): Promise<UnlistenFn> {
  return listen<PhotoMeta>('photo-meta', (e) => cb(e.payload));
}

export function onXmpPending(cb: (status: XmpStatus) => void): Promise<UnlistenFn> {
  return listen<XmpStatus>('xmp-pending', (e) => cb(e.payload));
}

// ---------- dev harness ----------

export interface DevFlags {
  open: string | null;
  storm: boolean;
  chaos: boolean;
  verify: boolean;
  resumeTest: boolean;
  zoomTest: boolean;
  facesForce: boolean;
}

export function devFlags(): Promise<DevFlags> {
  return invoke<DevFlags>('dev_flags');
}

/** Slice-0 faces spike counters — the storm harness reads these before and
 * after the measured window to prove inference was genuinely active. */
export interface FacesSpikeStats {
  photos: number;
  faces: number;
  passes: number;
  errors: number;
  avgDecodeMs: number;
  avgDetectMs: number;
  avgEmbedMs: number;
  avgTotalMs: number;
  initMs: number;
  rssMb: number;
}

export function facesSpikeStats(): Promise<FacesSpikeStats> {
  return invoke<FacesSpikeStats>('faces_spike_stats');
}

export function quitApp(): Promise<void> {
  return invoke('quit_app');
}

export function focusWindow(): Promise<void> {
  return invoke('focus_window');
}

export function frontendLog(level: string, msg: string): void {
  void invoke('frontend_log', { level, msg }).catch(() => {});
}

export function previewUrl(id: string): string {
  return `photo://localhost/preview/${id}`;
}

export function origUrl(id: string): string {
  return `photo://localhost/orig/${id}`;
}

export function thumbUrl(id: string): string {
  return `photo://localhost/thumb/${id}`;
}

export function histUrl(id: string): string {
  return `photo://localhost/hist/${id}`;
}

export function maskUrl(id: string): string {
  return `photo://localhost/mask/${id}`;
}

export interface HistogramData {
  r: number[];
  g: number[];
  b: number[];
  l: number[];
}
