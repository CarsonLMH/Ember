export interface Photo {
  id: string;
  /** Display name, e.g. "DSCF1234" */
  stem: string;
  /** Directory relative to the opened folder root ("" for root itself) */
  relDir: string;
  hasJpeg: boolean;
  hasRaf: boolean;
  rating: number;
  tags?: string[];
  /** On-disk file gone at last rescan; still listed, badged in the UI. */
  missing?: boolean;
  /** Epoch millis from EXIF DateTimeOriginal; filled in by the background pass */
  captureTs?: number;
  /** Display-oriented native pixel dims; filled in by the background pass */
  pixW?: number;
  pixH?: number;
}

export interface FolderState {
  folderId: number;
  sort: string;
  reverse: boolean;
  filter: string;
  cursorPhoto: string | null;
}

export interface ScanResult {
  photos: Photo[];
  state: FolderState;
  trashedCount: number;
  missingCount: number;
}

export interface Delta {
  photoId: string;
  kind: string;
  rating: number | null;
  tags: string[] | null;
  trashed: boolean | null;
  error: string | null;
}

export interface TrashedPhoto {
  id: string;
  stem: string;
  rating: number;
  trashedAt: number;
}

export type ServedFrom = 'bitmap-cache' | 'preview-decode' | 'original-fallback';

export interface FlipSample {
  at: number;
  latencyMs: number;
  servedFrom: ServedFrom;
  photoId: string;
}

export interface PerfReport {
  kind: 'flip-storm' | 'session';
  flips: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
  missServes: number;
  coldOpenMs: number | null;
  generatedAt: string;
}
