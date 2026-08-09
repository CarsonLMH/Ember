import { useEffect, useState, useSyncExternalStore } from 'react';
import { getTagVocab } from '../lib/ipc';
import * as session from '../lib/session';

/** Tag toggles for the current photo. Stays open across toggles (tags are
 * additive); owns all keys while open — the global handler stands down. */
export default function TagPalette({ onClose }: { onClose: () => void }) {
  const [vocab, setVocab] = useState<string[] | null>(null);
  const state = useSyncExternalStore(session.subscribe, session.getState);
  const photo = state.photos[state.cursor];
  const [sel, setSel] = useState(0);

  useEffect(() => {
    getTagVocab()
      .then(setVocab)
      .catch(() => setVocab([]));
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const n = vocab?.length ?? 0;
      if (e.key === 'Escape') {
        onClose();
      } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        const d = e.key === 'ArrowDown' ? 1 : -1;
        setSel((s) => Math.min(Math.max(0, n - 1), Math.max(0, s + d)));
      } else if (e.key === 'Enter' && vocab && n > 0) {
        void session.toggleTag(vocab[sel]);
      } else if (/^[1-9]$/.test(e.key) && vocab && Number(e.key) <= n) {
        void session.toggleTag(vocab[Number(e.key) - 1]);
      } else {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [vocab, sel, onClose]);

  const current = new Set(photo?.tags ?? []);
  return (
    <div className="cheat-backdrop" onClick={onClose}>
      <div className="cheat-sheet recipe-switcher">
        <div className="cheat-title">
          Tags{photo ? ` — ${photo.stem}` : ''}
          <span className="cheat-hint">digits toggle · esc · edit tags.toml</span>
        </div>
        {vocab === null ? null : vocab.length === 0 ? (
          <div className="recipe-empty">
            No vocabulary — add tags to tags.toml in the config folder.
          </div>
        ) : !photo ? (
          <div className="recipe-empty">No photo selected.</div>
        ) : (
          <div className="recipe-list">
            {vocab.map((t, i) => (
              <div
                key={t}
                className={`recipe-row${i === sel ? ' recipe-sel' : ''}`}
                onClick={(e) => {
                  e.stopPropagation();
                  void session.toggleTag(t);
                }}
              >
                <span className="recipe-num">{i + 1}</span>
                <span className="tag-check">{current.has(t) ? '✓' : ''}</span>
                <span>{t}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
