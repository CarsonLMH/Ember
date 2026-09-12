import { describe, expect, it } from 'vitest';
import {
  filmstripWindow,
  scrollTopForCursor,
} from './filmstripLayout';

describe('filmstrip virtualization', () => {
  it.each([
    { count: 1, cursor: 0 },
    { count: 197, cursor: 0 },
    { count: 197, cursor: 98 },
    { count: 197, cursor: 156 },
    { count: 197, cursor: 196 },
  ])('mounts the saved cursor on the first render ($cursor of $count)', ({ count, cursor }) => {
    const initialTop = scrollTopForCursor(cursor, 0, count);
    const { start, end } = filmstripWindow(count, initialTop, 0);

    expect(start).toBeLessThanOrEqual(cursor);
    expect(end).toBeGreaterThan(cursor);
  });

  it('centers the cursor when possible and clamps at both ends', () => {
    expect(scrollTopForCursor(0, 780, 197)).toBe(0);
    expect(scrollTopForCursor(98, 780, 197)).toBe(98 * 78 - 780 / 2 + 78 / 2);
    expect(scrollTopForCursor(196, 780, 197)).toBe(197 * 78 - 780);
  });

  it('returns no rows for an empty strip', () => {
    expect(filmstripWindow(0, 0, 0)).toEqual({ start: 0, end: 0 });
  });
});
