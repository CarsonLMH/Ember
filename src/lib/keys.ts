import { getKeymap, type KeyBinding } from './ipc';

/**
 * Keymap-driven input. Bindings come from keymap.toml (Rust merges defaults);
 * this module turns a KeyboardEvent into an action name.
 *
 * Matching: a combo string "Cmd+Shift+z" (modifier order Cmd,Ctrl,Alt,Shift)
 * plus a raw-key fallback so symbol bindings like "?" and "`" work without
 * spelling out Shift on every layout.
 */
let lookup = new Map<string, string>();
let bindings: KeyBinding[] = [];

function normalizeKeyName(key: string): string {
  return /^[A-Za-z]$/.test(key) ? key.toLowerCase() : key;
}

function normalizeCombo(combo: string): string {
  const parts = combo.split('+');
  const key = normalizeKeyName(parts[parts.length - 1] ?? '');
  const mods = new Set(parts.slice(0, -1).map((m) => m.toLowerCase()));
  return (
    (mods.has('cmd') || mods.has('meta') ? 'Cmd+' : '') +
    (mods.has('ctrl') ? 'Ctrl+' : '') +
    (mods.has('alt') || mods.has('option') ? 'Alt+' : '') +
    (mods.has('shift') ? 'Shift+' : '') +
    key
  );
}

/** Install a binding set (exported separately so tests can skip the IPC). */
export function applyBindings(newBindings: KeyBinding[]): void {
  bindings = newBindings;
  lookup = new Map();
  for (const b of bindings) {
    for (const combo of b.keys) {
      lookup.set(normalizeCombo(combo), b.action);
    }
  }
}

export async function loadKeymap(): Promise<{ warning: string | null }> {
  const result = await getKeymap();
  applyBindings(result.bindings);
  return { warning: result.warning };
}

export function currentBindings(): KeyBinding[] {
  return bindings;
}

export function actionFor(e: KeyboardEvent): string | null {
  // Shifted digits report symbols ("!") in e.key; recover the digit from code.
  const key =
    e.shiftKey && e.code.startsWith('Digit')
      ? e.code.slice(5)
      : normalizeKeyName(e.key);
  const combo =
    (e.metaKey ? 'Cmd+' : '') +
    (e.ctrlKey ? 'Ctrl+' : '') +
    (e.altKey ? 'Alt+' : '') +
    (e.shiftKey ? 'Shift+' : '') +
    key;
  // Raw-key fallback covers bindings like "?" where Shift is implicit — but
  // only for unmodified/shift-only events. A held Cmd/Ctrl/Alt must never
  // fall through to a bare-key action (Cmd+S is not "sort").
  if (e.metaKey || e.ctrlKey || e.altKey) {
    return lookup.get(combo) ?? null;
  }
  return lookup.get(combo) ?? lookup.get(normalizeKeyName(e.key)) ?? null;
}
