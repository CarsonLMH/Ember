import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { FaceOut, FacesProgress } from './ipc';

/**
 * The face-event half of the session (SPEC §14): events from a folder we have
 * left are dropped, folderless events (a terminal engine failure) are not, the
 * badge cache is invalidated PER PHOTO, and no people answer is ever applied
 * to a folder it wasn't asked for.
 *
 * session.ts reads localStorage at module scope and drives the canvas, the
 * bitmap cache and the backend; all four are stubbed so the event logic can be
 * exercised in node.
 */
const storage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => storage.get(k) ?? null,
  setItem: (k: string, v: string) => void storage.set(k, String(v)),
  removeItem: (k: string) => void storage.delete(k),
});

/** The handler session.start() registers for `faces-progress`. */
let onFaces: ((p: FacesProgress) => void) | null = null;
const facesForPhoto = vi.fn(async (_photoId: string): Promise<FaceOut[] | null> => []);
const personMap = vi.fn(async (): Promise<Record<string, number[]>> => ({}));

/** Two folders, so a folder switch mid-request is expressible. */
const photosOf = (ids: string[]) =>
  ids.map((id) => ({
    id,
    stem: id.toUpperCase(),
    relDir: '',
    hasJpeg: true,
    hasRaf: false,
    rating: 0,
    tags: [] as string[],
    missing: false,
  }));

vi.mock('./ipc', async (importOriginal) => {
  const real = await importOriginal<typeof import('./ipc')>();
  return {
    // The pure event/status predicates are the real ones: the point of the
    // tests below is that the session uses them.
    facesEventApplies: real.facesEventApplies,
    isScanning: real.isScanning,
    scanFolder: vi.fn(async (dir: string) => ({
      photos: dir === '/photos' ? photosOf(['a', 'b']) : photosOf(['c']),
      state: {
        folderId: dir === '/photos' ? 1 : 2,
        sort: 'capture',
        reverse: false,
        filter: 'all',
        cursorPhoto: null,
      },
      trashedCount: 0,
      missingCount: 0,
      foreign: [],
      dir,
    })),
    onFacesProgress: vi.fn(async (cb: (p: FacesProgress) => void) => {
      onFaces = cb;
      return () => {};
    }),
    onPhotoMeta: vi.fn(async () => () => {}),
    onXmpPending: vi.fn(async () => () => {}),
    onFocusProgress: vi.fn(async () => () => {}),
    focusMap: vi.fn(async () => ({ softThreshold: null, scores: {} })),
    facesForPhoto: (id: string) => facesForPhoto(id),
    personMap: () => personMap(),
    listPersons: vi.fn(async () => []),
    listRecipes: vi.fn(async () => []),
    recipeMap: vi.fn(async () => ({})),
    getRecipe: vi.fn(async () => ({ name: null, matches: [], hasMeta: false })),
    getFocus: vi.fn(async () => null),
    saveView: vi.fn(),
    setCursor: vi.fn(),
    setOrder: vi.fn(),
    setRating: vi.fn(),
    setTags: vi.fn(),
    trashPhoto: vi.fn(),
    undoAction: vi.fn(),
    redoAction: vi.fn(),
    restorePhoto: vi.fn(),
    retryXmpErrors: vi.fn(),
    maskUrl: (id: string) => `photo://mask/${id}`,
    origUrl: (id: string) => `photo://orig/${id}`,
  };
});

vi.mock('./imageCache', () => ({
  clear: vi.fn(),
  get: vi.fn(() => null),
  ensure: vi.fn(async () => null),
  abortOutside: vi.fn(),
  size: vi.fn(() => 0),
}));
vi.mock('./viewer', () => ({
  setPhoto: vi.fn(),
  setAf: vi.fn(),
  setBlinkies: vi.fn(),
  setFullres: vi.fn(),
  render: vi.fn(),
  isZoomed: vi.fn(() => false),
  exitZoom: vi.fn(),
  zoomToPoint: vi.fn(),
}));
vi.mock('./metaCache', () => ({ clearMetaCache: vi.fn(), ensureMeta: vi.fn(async () => null) }));
vi.mock('./perf', () => ({
  reset: vi.fn(),
  recordFlip: vi.fn(),
  markColdOpen: vi.fn(),
  subscribe: vi.fn(() => () => {}),
  stats: vi.fn(),
}));

const session = await import('./session');
const viewer = await import('./viewer');
const ipc = await import('./ipc');

async function settle(): Promise<void> {
  for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0));
}

/** Move the cursor and let the post-display face fetch run. */
async function goTo(id: string): Promise<void> {
  session.jumpToPhotoId(id);
  await settle();
}

describe('auto-advance preference', () => {
  it('starts with auto-advance disabled when no preference exists', () => {
    expect(storage.has('autoAdvance')).toBe(false);
    expect(session.getState().autoAdvance).toBe(false);
  });
});

describe('rating durability and movement', () => {
  beforeEach(async () => {
    storage.clear();
    if (session.getState().autoAdvance) session.toggleAutoAdvance();
    vi.mocked(ipc.setRating).mockReset();
    vi.mocked(ipc.setRating).mockResolvedValue(undefined);
    await session.openFolder('/photos');
    await settle();
  });

  it('changes neither rating nor cursor before the durable acknowledgement', async () => {
    let acknowledge!: () => void;
    vi.mocked(ipc.setRating).mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          acknowledge = resolve;
        }),
    );

    const pending = session.rate(3, 0);
    await settle();
    expect(session.getState().photos[0].rating).toBe(0);
    expect(session.getState().cursor).toBe(0);

    acknowledge();
    await pending;
    expect(session.getState().photos[0].rating).toBe(3);
    expect(session.getState().cursor).toBe(0);
  });

  it('keeps the verdict and cursor unchanged when the write is refused', async () => {
    vi.mocked(ipc.setRating).mockRejectedValueOnce(new Error('disk full'));

    await session.rate(4, 0);

    expect(session.getState().photos[0].rating).toBe(0);
    expect(session.getState().cursor).toBe(0);
    expect(session.getState().notice).toContain('Rating NOT saved');
  });

  it('persists an explicit opt-in and advances only after acknowledgement', async () => {
    session.toggleAutoAdvance();
    expect(storage.get('autoAdvance')).toBe('1');

    await session.rate(5, 0);

    expect(session.getState().all.find((p) => p.id === 'a')?.rating).toBe(5);
    expect(session.getState().cursor).toBe(1);
    expect(session.getState().photos[1].id).toBe('b');
  });

  it('never advances when clearing a rating', async () => {
    session.toggleAutoAdvance();

    await session.rate(0, 0);

    expect(session.getState().cursor).toBe(0);
  });

  it('allows an active filter to remove the rated photo without calling it auto-advance', async () => {
    session.setFilter('unstarred');

    await session.rate(3, 0);

    expect(session.getState().autoAdvance).toBe(false);
    expect(session.getState().photos.map((p) => p.id)).toEqual(['b']);
    expect(session.getState().cursor).toBe(0);
  });
});

describe('faces-progress handling', () => {
  beforeEach(async () => {
    facesForPhoto.mockClear();
    personMap.mockClear();
    personMap.mockImplementation(async () => ({}));
    session.start();
    await session.openFolder('/photos');
    await settle();
    facesForPhoto.mockClear();
  });

  it('drops events from a folder we have left', () => {
    expect(onFaces).not.toBeNull();
    onFaces?.({ folderId: 99, scanned: 1, total: 2, photoIds: ['a'] });
    expect(facesForPhoto).not.toHaveBeenCalled();
  });

  it('refetches the current photo when the worker touched it', async () => {
    onFaces?.({ folderId: 1, scanned: 1, total: 2, photoIds: ['a'] });
    await settle();
    expect(facesForPhoto).toHaveBeenCalledWith('a');
  });

  it('invalidates per photo: only the touched photo refetches on revisit', async () => {
    // Cache BOTH photos first — the cache is what the invalidation acts on.
    await goTo('b');
    expect(facesForPhoto.mock.calls.map(([id]) => id)).toEqual(['b']);
    await goTo('a');
    facesForPhoto.mockClear();
    await goTo('b');
    expect(facesForPhoto).not.toHaveBeenCalled(); // both are cached now

    // The worker touches only b, while a is on screen.
    await goTo('a');
    facesForPhoto.mockClear();
    onFaces?.({ folderId: 1, scanned: 1, total: 2, photoIds: ['b'] });
    await settle();
    expect(facesForPhoto).not.toHaveBeenCalled(); // a is untouched: no IPC

    // Revisiting b refetches (its entry was dropped)…
    await goTo('b');
    expect(facesForPhoto.mock.calls.map(([id]) => id)).toEqual(['b']);

    // …and revisiting a still does not: per-photo, not "clear everything".
    facesForPhoto.mockClear();
    await goTo('a');
    expect(facesForPhoto).not.toHaveBeenCalled();
  });

  it('accepts the folderless event a terminal engine failure emits', async () => {
    // Same shape as a real event, with the current photo named: if the
    // folderless event were dropped as stale, no refetch would happen. This
    // asserts the acceptance, not merely the absence of an effect.
    onFaces?.({ folderId: null, scanned: 0, total: 0, photoIds: ['a'] });
    await settle();
    expect(facesForPhoto).toHaveBeenCalledWith('a');
  });

  it('never applies one folder’s person membership to another', async () => {
    // The membership request for folder 1 is still in flight when the user
    // opens folder 2. Its answer must be discarded, not filtered with.
    let release!: (v: Record<string, number[]>) => void;
    const held = new Promise<Record<string, number[]>>((resolve) => {
      release = resolve;
    });
    personMap.mockImplementationOnce(() => held);
    const pending = session.setPersonFilter(7);
    await settle();
    await session.openFolder('/other');
    await settle();
    expect(session.getState().folderId).toBe(2);

    release({ a: [7] }); // folder 1's answer arrives late
    await pending;
    expect(session.getState().personFilter).toBeNull();
    expect(session.getState().photos.map((p) => p.id)).toEqual(['c']);
  });

  it('refreshes membership when a faces event races filter activation', async () => {
    vi.useFakeTimers();
    try {
      let release!: (v: Record<string, number[]>) => void;
      const initialMap = new Promise<Record<string, number[]>>((resolve) => {
        release = resolve;
      });
      personMap
        .mockImplementationOnce(() => initialMap)
        .mockResolvedValue({ a: [7], b: [7] });

      const pending = session.setPersonFilter(7);
      // The worker commits another match while the initial membership request
      // is in flight. Its event arrives before personFilter is visible in
      // session state, so filter activation must arrange a follow-up refresh.
      onFaces?.({ folderId: 1, scanned: 2, total: 2, photoIds: ['b'] });
      release({ a: [7] });
      await pending;
      expect(session.getState().photos.map((p) => p.id)).toEqual(['a']);

      await vi.advanceTimersByTimeAsync(1000);
      expect(session.getState().photos.map((p) => p.id)).toEqual(['a', 'b']);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('focusZoom (F key)', () => {
  beforeEach(async () => {
    session.start();
    await session.openFolder('/photos');
    await settle();
    vi.mocked(viewer.isZoomed).mockReturnValue(false);
    vi.mocked(viewer.exitZoom).mockClear();
    vi.mocked(viewer.zoomToPoint).mockClear();
  });

  it('F while zoomed exits to fit, even after flipping to another photo', async () => {
    // Regression: locked zoom survives the flip, so the F pressed on photo b
    // arrives with photo a's AF zoom still on screen. It must exit, not
    // re-zoom onto b's AF point.
    vi.mocked(ipc.getFocus).mockResolvedValue([0.3, 0.4]);
    await session.focusZoom();
    expect(viewer.zoomToPoint).toHaveBeenCalledWith(0.3, 0.4);

    vi.mocked(viewer.isZoomed).mockReturnValue(true);
    await goTo('b');
    vi.mocked(viewer.zoomToPoint).mockClear();
    await session.focusZoom();
    expect(viewer.exitZoom).toHaveBeenCalled();
    expect(viewer.zoomToPoint).not.toHaveBeenCalled();
  });
});
