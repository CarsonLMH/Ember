import { origUrl, previewUrl } from './ipc';
import type { Photo, ServedFrom } from './types';

/**
 * The app's decoded-pixel store. WebKit prunes its own decoded frames on a
 * timer and flushes them under memory pressure, so we never rely on it:
 * every displayed bitmap lives here as an ImageBitmap we own, and is
 * explicitly close()d on eviction (~25–32MB each; GC is too lazy for that).
 */
const MAX_ENTRIES = 20;

export interface CacheEntry {
  bitmap: ImageBitmap;
  servedFrom: ServedFrom;
  lastUsed: number;
}

interface Pending {
  promise: Promise<CacheEntry | null>;
  ctrl: AbortController;
}

const cache = new Map<string, CacheEntry>();
const pending = new Map<string, Pending>();
let tick = 0;

export function get(id: string): CacheEntry | undefined {
  const entry = cache.get(id);
  if (entry) entry.lastUsed = ++tick;
  return entry;
}

export function size(): number {
  return cache.size;
}

async function fetchBitmap(photo: Photo, ctrl: AbortController): Promise<CacheEntry | null> {
  let servedFrom: ServedFrom = 'preview-decode';
  let resp = await fetch(previewUrl(photo.id), { signal: ctrl.signal });
  if (!resp.ok) {
    // Preview not generated yet — decode the original instead of waiting.
    servedFrom = 'original-fallback';
    resp = await fetch(origUrl(photo.id), { signal: ctrl.signal });
    if (!resp.ok) return null;
  }
  const blob = await resp.blob();
  const bitmap = await createImageBitmap(blob);
  return { bitmap, servedFrom, lastUsed: ++tick };
}

/**
 * Ensure a photo's bitmap is cached; concurrent calls for the same id share
 * one fetch. Returns null for undecodable photos.
 */
export function ensure(photo: Photo, protectedIds: Set<string>): Promise<CacheEntry | null> {
  const hit = cache.get(photo.id);
  if (hit) {
    hit.lastUsed = ++tick;
    return Promise.resolve(hit);
  }
  const inFlight = pending.get(photo.id);
  if (inFlight) return inFlight.promise;

  const ctrl = new AbortController();
  const promise = fetchBitmap(photo, ctrl)
    .then((entry) => {
      pending.delete(photo.id);
      if (!entry) return null;
      cache.set(photo.id, entry);
      evict(protectedIds);
      return entry;
    })
    .catch(() => {
      pending.delete(photo.id);
      return null;
    });
  pending.set(photo.id, { promise, ctrl });
  return promise;
}

/** Abort queued fetches that fell outside the preload window. */
export function abortOutside(windowIds: Set<string>): void {
  for (const [id, p] of pending) {
    if (!windowIds.has(id)) {
      p.ctrl.abort();
      pending.delete(id);
    }
  }
}

function evict(protectedIds: Set<string>): void {
  while (cache.size > MAX_ENTRIES) {
    let victim: string | null = null;
    let oldest = Infinity;
    for (const [id, entry] of cache) {
      if (protectedIds.has(id)) continue;
      if (entry.lastUsed < oldest) {
        oldest = entry.lastUsed;
        victim = id;
      }
    }
    if (!victim) return;
    cache.get(victim)?.bitmap.close();
    cache.delete(victim);
  }
}

export function clear(): void {
  for (const p of pending.values()) p.ctrl.abort();
  pending.clear();
  for (const entry of cache.values()) entry.bitmap.close();
  cache.clear();
}
