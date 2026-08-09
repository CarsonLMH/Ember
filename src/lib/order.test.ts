import { describe, expect, it } from 'vitest';
import { byCaptureOrder, comparator, passesFilter, passesTagFilter } from './order';
import type { Photo } from './types';

function photo(p: Partial<Photo>): Photo {
  return {
    id: p.stem ?? 'id',
    stem: 'DSCF0001',
    relDir: '',
    hasJpeg: true,
    hasRaf: true,
    rating: 0,
    ...p,
  };
}

describe('byCaptureOrder', () => {
  it('sorts by capture time, tie-broken by stem', () => {
    const a = photo({ stem: 'DSCF0002', captureTs: 100 });
    const b = photo({ stem: 'DSCF0001', captureTs: 100 });
    const c = photo({ stem: 'DSCF0003', captureTs: 50 });
    expect([a, b, c].sort(byCaptureOrder).map((p) => p.stem)).toEqual([
      'DSCF0003',
      'DSCF0001',
      'DSCF0002',
    ]);
  });

  it('sorts photos without a capture time last', () => {
    const dated = photo({ stem: 'DSCF0001', captureTs: 100 });
    const undated = photo({ stem: 'DSCF0000' });
    expect([undated, dated].sort(byCaptureOrder)[0]).toBe(dated);
  });
});

describe('comparator', () => {
  const early = photo({ stem: 'DSCF0001', captureTs: 100, rating: 1 });
  const late = photo({ stem: 'DSCF0002', captureTs: 200, rating: 5 });

  it('name sort falls back to relDir on equal stems', () => {
    const rootCopy = photo({ stem: 'DSCF0001', relDir: '' });
    const subCopy = photo({ stem: 'DSCF0001', relDir: 'sub' });
    expect([subCopy, rootCopy].sort(comparator('name', false))[0]).toBe(rootCopy);
  });

  it('rating sort is descending, capture-tiebroken', () => {
    const alsoFive = photo({ stem: 'DSCF0000', captureTs: 50, rating: 5 });
    const sorted = [early, late, alsoFive].sort(comparator('rating', false));
    expect(sorted.map((p) => p.stem)).toEqual(['DSCF0000', 'DSCF0002', 'DSCF0001']);
  });

  it('reverse negates the order', () => {
    expect([early, late].sort(comparator('capture', true))[0]).toBe(late);
  });
});

describe('passesFilter', () => {
  it('all admits everything', () => {
    expect(passesFilter(photo({ rating: 0 }), 'all')).toBe(true);
    expect(passesFilter(photo({ rating: 5 }), 'all')).toBe(true);
  });

  it('unstarred and starred split on zero', () => {
    expect(passesFilter(photo({ rating: 0 }), 'unstarred')).toBe(true);
    expect(passesFilter(photo({ rating: 3 }), 'unstarred')).toBe(false);
    expect(passesFilter(photo({ rating: 0 }), 'starred')).toBe(false);
    expect(passesFilter(photo({ rating: 3 }), 'starred')).toBe(true);
  });

  it('starN matches the exact rating only', () => {
    expect(passesFilter(photo({ rating: 3 }), 'star3')).toBe(true);
    expect(passesFilter(photo({ rating: 4 }), 'star3')).toBe(false);
  });
});

describe('passesTagFilter', () => {
  it('null filter admits everything, including untagged', () => {
    expect(passesTagFilter(photo({}), null)).toBe(true);
    expect(passesTagFilter(photo({ tags: ['print'] }), null)).toBe(true);
  });

  it('matches exact tag membership', () => {
    expect(passesTagFilter(photo({ tags: ['print', 'album'] }), 'album')).toBe(true);
    expect(passesTagFilter(photo({ tags: ['print'] }), 'album')).toBe(false);
    expect(passesTagFilter(photo({}), 'album')).toBe(false);
  });
});
