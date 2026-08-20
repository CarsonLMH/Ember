import type { Photo } from './types';

/** Pure sort/filter logic for the photo list — no session state, unit-testable. */

export type FilterMode =
  | 'all'
  | 'unstarred'
  | 'starred'
  | 'star1'
  | 'star2'
  | 'star3'
  | 'star4'
  | 'star5';
export type SortMode = 'capture' | 'name' | 'rating';

export const FILTERS: FilterMode[] = ['all', 'unstarred', 'starred', 'star1', 'star2', 'star3', 'star4', 'star5'];
export const SORTS: SortMode[] = ['capture', 'name', 'rating'];

export function byCaptureOrder(a: Photo, b: Photo): number {
  return (
    (a.captureTs ?? Infinity) - (b.captureTs ?? Infinity) || a.stem.localeCompare(b.stem)
  );
}

export function comparator(sort: SortMode, reverse: boolean): (a: Photo, b: Photo) => number {
  const base =
    sort === 'name'
      ? (a: Photo, b: Photo) => a.stem.localeCompare(b.stem) || a.relDir.localeCompare(b.relDir)
      : sort === 'rating'
        ? (a: Photo, b: Photo) => b.rating - a.rating || byCaptureOrder(a, b)
        : byCaptureOrder;
  return reverse ? (a: Photo, b: Photo) => -base(a, b) : base;
}

export function passesTagFilter(p: Photo, tag: string | null): boolean {
  if (!tag) return true;
  return (p.tags ?? []).includes(tag);
}

/**
 * Person filter (SPEC §14). The map is photo id → person ids with a visible
 * assigned face; a photo the face index hasn't reached yet is simply absent,
 * so it fails the filter until it's scanned — matches stream in as the worker
 * progresses rather than the list lying about a complete answer.
 */
export function passesPersonFilter(
  p: Photo,
  personId: number | null,
  map: Record<string, number[]>,
): boolean {
  if (personId === null) return true;
  return (map[p.id] ?? []).includes(personId);
}

/**
 * Focus filter: show only photos scoring BELOW the user's soft threshold
 * (settings.toml [focus]). Like the person filter, an unscanned photo is
 * simply absent from the map and fails — matches stream in as the sweep
 * progresses rather than the list lying about a complete answer. With no
 * threshold configured the filter is inert (Ember never judges on its own).
 */
export function passesFocusFilter(
  p: Photo,
  on: boolean,
  threshold: number | null,
  scores: Record<string, number>,
): boolean {
  if (!on || threshold === null) return true;
  const s = scores[p.id];
  return s !== undefined && s < threshold;
}

/**
 * Preview-priority hint for the backend: everything the user can currently
 * reach, nearest-first order, then every other photo in the folder.
 *
 * The old version appended only photos failing the STAR filter, so anything
 * hidden by the recipe/tag/person axes vanished from the hint entirely and
 * its preview never got prioritized — clearing that filter then hit cold
 * cache. Visible-first ordering is what preserves flip-window preloading.
 */
export function orderHint(visible: Photo[], sortedAll: Photo[]): string[] {
  const seen = new Set(visible.map((p) => p.id));
  const rest = sortedAll.filter((p) => !seen.has(p.id)).map((p) => p.id);
  return [...seen, ...rest];
}

export function passesFilter(p: Photo, filter: FilterMode): boolean {
  switch (filter) {
    case 'all':
      return true;
    case 'unstarred':
      return p.rating === 0;
    case 'starred':
      return p.rating > 0;
    default:
      return p.rating === Number(filter.slice(4));
  }
}
