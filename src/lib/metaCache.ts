import { getMetadata } from './ipc';

/** Shared metadata cache: the panel reads it, the session prefetches into it
 * for neighboring photos so flips render panel content in one pass (no
 * loading flash). */
export type Meta = Record<string, unknown>;

const cache = new Map<string, Meta>();
const pending = new Map<string, Promise<Meta | null>>();

export function getCachedMeta(id: string): Meta | undefined {
  return cache.get(id);
}

export function ensureMeta(id: string): Promise<Meta | null> {
  const hit = cache.get(id);
  if (hit) return Promise.resolve(hit);
  const inflight = pending.get(id);
  if (inflight) return inflight;
  const p = getMetadata(id)
    .then((json) => {
      pending.delete(id);
      const parsed = json ? (JSON.parse(json) as Meta) : null;
      // "{}" rows mean the sweep hasn't produced real data yet — don't cache.
      if (parsed && Object.keys(parsed).length > 1) {
        if (cache.size > 150) cache.clear();
        cache.set(id, parsed);
        return parsed;
      }
      return null;
    })
    .catch(() => {
      pending.delete(id);
      return null;
    });
  pending.set(id, p);
  return p;
}

export function clearMetaCache(): void {
  cache.clear();
}
