import { useEffect, useState } from 'react';
import { getTagVocab } from '../lib/ipc';
import * as session from '../lib/session';

interface Option {
  label: string;
  value: string | null;
}

/** Keyboard-first tag filter, mirroring RecipeSwitcher: pick one, close. */
export default function TagSwitcher({ onClose }: { onClose: () => void }) {
  const [options, setOptions] = useState<Option[] | null>(null);
  const [sel, setSel] = useState(0);

  useEffect(() => {
    void getTagVocab()
      .catch(() => [] as string[])
      .then((vocab) => {
        // Vocabulary first, then any in-use tags outside it (renamed vocab etc.).
        const s = session.getState();
        const inUse = new Set(s.all.flatMap((p) => p.tags ?? []));
        const extras = [...inUse].filter((t) => !vocab.includes(t)).sort();
        const opts: Option[] = [
          { label: 'all photos', value: null },
          ...[...vocab, ...extras].map((t) => ({ label: t, value: t })),
        ];
        setOptions(opts);
        const i = opts.findIndex((o) => o.value === s.tagFilter);
        setSel(i >= 0 ? i : 0);
      });
  }, []);

  const pick = (o: Option) => {
    session.setTagFilter(o.value);
    onClose();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!options) return;
      if (e.key === 'Escape') {
        onClose();
      } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        const d = e.key === 'ArrowDown' ? 1 : -1;
        setSel((s) => Math.min(options.length - 1, Math.max(0, s + d)));
      } else if (e.key === 'Enter') {
        pick(options[sel]);
      } else if (/^[1-9]$/.test(e.key) && Number(e.key) <= options.length) {
        pick(options[Number(e.key) - 1]);
      } else {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  return (
    <div className="cheat-backdrop" onClick={onClose}>
      <div className="cheat-sheet recipe-switcher">
        <div className="cheat-title">
          Tag filter
          <span className="cheat-hint">↑↓ + enter · digits pick · esc</span>
        </div>
        {options && options.length > 1 ? (
          <div className="recipe-list">
            {options.map((o, i) => (
              <div
                key={o.label}
                className={`recipe-row${i === sel ? ' recipe-sel' : ''}`}
                onClick={(e) => {
                  e.stopPropagation();
                  pick(o);
                }}
              >
                <span className="recipe-num">{i + 1}</span>
                <span>{o.label}</span>
              </div>
            ))}
          </div>
        ) : options ? (
          <div className="recipe-empty">No tags yet — press g to tag photos first.</div>
        ) : null}
      </div>
    </div>
  );
}
