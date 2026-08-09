import { useEffect, useState } from 'react';
import * as session from '../lib/session';

interface Option {
  label: string;
  value: string | null;
}

/** Keyboard-first recipe filter: arrows + enter, digits pick, esc closes.
 * Owns all keys while open — the global handler stands down (App.tsx). */
export default function RecipeSwitcher({ onClose }: { onClose: () => void }) {
  const [options] = useState<Option[]>(() => {
    const s = session.getState();
    return [
      { label: 'all recipes', value: null },
      ...s.recipeNames.map((n) => ({ label: n, value: n })),
      { label: 'unknown recipe', value: session.UNKNOWN_RECIPE },
    ];
  });
  const [sel, setSel] = useState(() => {
    const i = options.findIndex((o) => o.value === session.getState().recipeFilter);
    return i >= 0 ? i : 0;
  });

  const pick = (o: Option) => {
    void session.setRecipeFilter(o.value);
    onClose();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
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

  const hasRecipes = options.length > 2;
  return (
    <div className="cheat-backdrop" onClick={onClose}>
      <div className="cheat-sheet recipe-switcher">
        <div className="cheat-title">
          Recipe filter
          <span className="cheat-hint">↑↓ + enter · digits pick · esc</span>
        </div>
        {hasRecipes ? (
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
        ) : (
          <div className="recipe-empty">
            No recipes yet — save one from the metadata panel (I).
          </div>
        )}
      </div>
    </div>
  );
}
