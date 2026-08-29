import { useEffect, useRef, useState } from 'react';
import * as session from '../lib/session';
import { thumbUrl } from '../lib/ipc';
import type { Photo } from '../lib/types';

const ROW = 78;
const OVERSCAN = 8;
const RETRY_MS = 1200;
const MAX_RETRIES = 40; // covers a full preview sweep of a large folder

/**
 * A thumbnail 404s until the background sweep generates that photo's preview
 * — and a plain <img> never retries, which left permanent broken-image "?"
 * icons. This retries quietly behind a placeholder until the thumb exists.
 */
function StripThumb({ id }: { id: string }) {
  const [attempt, setAttempt] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  if (attempt > MAX_RETRIES) {
    return <div className="strip-thumb strip-raf">·</div>;
  }
  return (
    <img
      className="strip-thumb"
      style={loaded ? undefined : { opacity: 0 }}
      src={`${thumbUrl(id)}?r=${attempt}`}
      loading="lazy"
      alt=""
      onLoad={() => setLoaded(true)}
      onError={() => {
        if (timer.current) clearTimeout(timer.current);
        timer.current = setTimeout(() => setAttempt((a) => a + 1), RETRY_MS);
      }}
    />
  );
}

/**
 * Virtualized vertical strip. Only the visible rows (+overscan) exist in the
 * DOM; thumbnails are 240px files, so decoded cost per row is trivial.
 */
export default function Filmstrip({
  photos,
  cursor,
}: {
  photos: Photo[];
  cursor: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(0);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // Keep the current photo centered as the cursor moves.
  useEffect(() => {
    const el = ref.current;
    if (!el || !height) return;
    const target = cursor * ROW - height / 2 + ROW / 2;
    el.scrollTo({ top: Math.max(0, target) });
  }, [cursor, height, photos.length]);

  const start = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
  const end = Math.min(photos.length, Math.ceil((scrollTop + height) / ROW) + OVERSCAN);
  // Focus-check dots appear only past the user's own settings.toml threshold.
  const { focusScores, focusSoftThreshold } = session.getState();
  const rows = [];
  for (let i = start; i < end; i++) {
    const p = photos[i];
    const soft =
      focusSoftThreshold !== null &&
      focusScores[p.id] !== undefined &&
      focusScores[p.id] < focusSoftThreshold;
    rows.push(
      <div
        key={p.id}
        className={`strip-row${i === cursor ? ' strip-current' : ''}${p.missing ? ' strip-missing' : ''}`}
        style={{ top: i * ROW }}
        onClick={() => session.jumpTo(i)}
      >
        <StripThumb id={p.id} />
        {p.rating > 0 && <span className="strip-stars">{'★'.repeat(p.rating)}</span>}
        {p.missing && <span className="strip-missing-mark">⚠</span>}
        {soft && (
          <span className="strip-soft" title="soft at the AF point">
            ●
          </span>
        )}
        {p.tags && p.tags.length > 0 && (
          <span className="strip-tags">#{p.tags.length}</span>
        )}
      </div>,
    );
  }

  return (
    <div
      ref={ref}
      className="filmstrip"
      role="region"
      aria-label="Photo filmstrip"
      tabIndex={0}
      onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
    >
      <div className="strip-inner" style={{ height: photos.length * ROW }}>
        {rows}
      </div>
    </div>
  );
}
