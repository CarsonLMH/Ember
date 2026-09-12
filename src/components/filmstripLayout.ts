export const FILMSTRIP_ROW = 78;
const OVERSCAN = 8;

export function scrollTopForCursor(
  cursor: number,
  viewportHeight: number,
  photoCount: number,
): number {
  if (photoCount <= 0) return 0;
  const clampedCursor = Math.min(photoCount - 1, Math.max(0, cursor));
  const height = Math.max(FILMSTRIP_ROW, viewportHeight);
  const target =
    clampedCursor * FILMSTRIP_ROW - height / 2 + FILMSTRIP_ROW / 2;
  const max = Math.max(0, photoCount * FILMSTRIP_ROW - height);
  return Math.min(max, Math.max(0, target));
}

export function filmstripWindow(
  photoCount: number,
  scrollTop: number,
  viewportHeight: number,
): { start: number; end: number } {
  if (photoCount <= 0) return { start: 0, end: 0 };
  const height = Math.max(FILMSTRIP_ROW, viewportHeight);
  return {
    start: Math.max(0, Math.floor(scrollTop / FILMSTRIP_ROW) - OVERSCAN),
    end: Math.min(
      photoCount,
      Math.ceil((scrollTop + height) / FILMSTRIP_ROW) + OVERSCAN,
    ),
  };
}
