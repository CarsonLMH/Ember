import { useEffect, useState } from 'react';
import { buildInfo, type BuildInfo } from '../lib/ipc';
import { currentBindings } from '../lib/keys';

/** The stamp never changes while the app runs — fetch once per launch. */
let cachedInfo: BuildInfo | null = null;

/** Overlay listing every action with its ACTUAL current keys (post-remap). */
export default function CheatSheet({ onClose }: { onClose: () => void }) {
  const bindings = currentBindings();
  const [info, setInfo] = useState<BuildInfo | null>(cachedInfo);

  useEffect(() => {
    if (cachedInfo) return;
    let alive = true;
    void buildInfo()
      .then((i) => {
        cachedInfo = i;
        if (alive) setInfo(i);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

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
        {info && (
          <div className="cheat-footer">
            Ember {info.version} · {info.commit} · built {info.builtAt} · changes in CHANGELOG.md
          </div>
        )}
      </div>
    </div>
  );
}
