import { useEffect, useState } from 'react';
import * as session from '../lib/session';

interface Option {
  label: string;
  value: number | null;
}

/** Keyboard-first person filter, mirroring TagSwitcher: pick one, close.
 * Only people who actually appear in this folder are offered — a global
 * roster would make the digit shortcuts meaningless folder to folder. */
export default function PersonSwitcher({ onClose }: { onClose: () => void }) {
  const s = session.getState();
  const options: Option[] = [
    { label: 'all photos', value: null },
    ...s.persons
      .filter((p) => p.folderCount > 0)
      .map((p) => ({ label: `${p.name} (${p.folderCount})`, value: p.id })),
  ];
  const [sel, setSel] = useState(() => {
    const i = options.findIndex((o) => o.value === s.personFilter);
    return i >= 0 ? i : 0;
  });

  const pick = (o: Option) => {
    void session.setPersonFilter(o.value);
    onClose();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
      } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        const d = e.key === 'ArrowDown' ? 1 : -1;
        setSel((v) => Math.min(options.length - 1, Math.max(0, v + d)));
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
          Person filter
          <span className="cheat-hint">↑↓ + enter · digits pick · esc</span>
        </div>
        {options.length > 1 ? (
          <div className="recipe-list">
            {options.map((o, i) => (
              <div
                key={o.value ?? 'all'}
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
        ) : (
          <div className="recipe-empty">
            Nobody named in this folder yet — press p to name faces first.
          </div>
        )}
      </div>
    </div>
  );
}
