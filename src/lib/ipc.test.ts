import { describe, expect, it } from 'vitest';
import { facesEventApplies, isScanning, type FaceScanStatus } from './ipc';

/** The two face predicates the session and the People panel both route
 * through — pure, so they are tested here rather than inferred from UI. */

const status = (patch: Partial<FaceScanStatus> = {}): FaceScanStatus => ({
  enabled: true,
  total: 3,
  scanned: 3,
  errors: 0,
  retrying: 0,
  pending: 0,
  engineError: null,
  ...patch,
});

describe('isScanning', () => {
  it('is true only while photos are still queued', () => {
    expect(isScanning(status({ scanned: 1, pending: 2 }))).toBe(true);
    expect(isScanning(status())).toBe(false);
    expect(isScanning(null)).toBe(false);
  });

  it('a photo parked in a terminal error is finished, not scanning', () => {
    // The exact shape that used to hang the panel on "Scanning…": the only
    // photo in the folder exhausted every retry, so `scanned` can never reach
    // `total` and nothing further will ever happen.
    const terminal = status({ total: 1, scanned: 0, errors: 1, pending: 0 });
    expect(terminal.scanned < terminal.total).toBe(true); // the old predicate
    expect(isScanning(terminal)).toBe(false);
  });

  it('a failed photo the worker will still retry keeps the panel scanning', () => {
    // The backend counts retrying photos inside `pending`, so the run is
    // truthfully "still going" while another attempt is scheduled.
    const retrying = status({ total: 1, scanned: 0, retrying: 1, pending: 1 });
    expect(isScanning(retrying)).toBe(true);
  });

  it('is false while indexing is off, however much is unscanned', () => {
    expect(isScanning(status({ enabled: false, scanned: 0, pending: 3 }))).toBe(false);
  });
});

describe('facesEventApplies', () => {
  const event = (folderId: number | null) => ({
    folderId,
    scanned: 0,
    total: 0,
    photoIds: [] as string[],
  });

  it('keeps events for the open folder and drops the rest', () => {
    expect(facesEventApplies(event(1), 1)).toBe(true);
    expect(facesEventApplies(event(2), 1)).toBe(false);
  });

  it('always keeps the folderless event a terminal engine failure emits', () => {
    // It has no folder of its own; dropping it would leave an open panel on
    // a stale "Scanning…" with the engine error never arriving.
    expect(facesEventApplies(event(null), 1)).toBe(true);
    expect(facesEventApplies(event(null), null)).toBe(true);
  });
});
