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
