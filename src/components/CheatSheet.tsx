import { currentBindings } from '../lib/keys';

/** Overlay listing every action with its ACTUAL current keys (post-remap). */
export default function CheatSheet({ onClose }: { onClose: () => void }) {
  const bindings = currentBindings();
  return (
    <div className="cheat-backdrop" onClick={onClose}>
      <div className="cheat-sheet">
        <div className="cheat-title">
          Keyboard shortcuts
          <span className="cheat-hint">edit in keymap.toml · esc to close</span>
        </div>
        <div className="cheat-grid">
          {bindings.map((b) => (
            <div key={b.action} className="cheat-row">
              <span className="cheat-keys">
                {b.keys.map((k) => (
                  <kbd key={k}>{k}</kbd>
                ))}
              </span>
              <span className="cheat-label">{b.label}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
