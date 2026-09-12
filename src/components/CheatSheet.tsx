import { Fragment, useEffect, useRef, useState } from 'react';
import { buildInfo, type BuildInfo } from '../lib/ipc';
import { currentBindings } from '../lib/keys';
import { groupBindings } from './shortcutGroups';

/** The stamp never changes while the app runs — fetch once per launch. */
let cachedInfo: BuildInfo | null = null;

/** Overlay listing every action with its ACTUAL current keys (post-remap). */
export default function CheatSheet({ onClose }: { onClose: () => void }) {
  const bindings = currentBindings();
  const sections = groupBindings(bindings);
  const [info, setInfo] = useState<BuildInfo | null>(cachedInfo);
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

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

  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.focus();
    return () => {
      if (previous?.isConnected) previous.focus();
    };
  }, []);

  const onDialogKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      onCloseRef.current();
      return;
    }
    if (event.key !== 'Tab') return;
    const focusable = [
      ...(dialogRef.current?.querySelectorAll<HTMLElement>(
        'button:not(:disabled), [tabindex]:not([tabindex="-1"])',
      ) ?? []),
    ];
    if (focusable.length === 0) {
      event.preventDefault();
      dialogRef.current?.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const activeIndex = focusable.indexOf(document.activeElement as HTMLElement);
    const next = event.shiftKey
      ? activeIndex <= 0
        ? last
        : focusable[activeIndex - 1]
      : activeIndex < 0 || activeIndex === focusable.length - 1
        ? first
        : focusable[activeIndex + 1];
    event.preventDefault();
    next.focus();
  };

  return (
    <div
      className="cheat-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="cheat-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="cheat-title"
        tabIndex={-1}
        onClick={(event) => event.stopPropagation()}
        onKeyDown={onDialogKeyDown}
      >
        <div className="cheat-titlebar">
          <div>
            <h2 id="cheat-title" className="cheat-title">Keyboard shortcuts</h2>
            <span className="cheat-hint">Actual bindings from keymap.toml</span>
          </div>
          <button className="cheat-close" onClick={onClose} aria-label="Close keyboard shortcuts">
            Close
          </button>
        </div>
        <div
          className="cheat-sections"
          role="region"
          tabIndex={0}
          aria-label="Keyboard shortcut reference"
        >
          {sections.map((section) => (
            <section
              key={section.id}
              className={`cheat-section cheat-section-${section.id}`}
              aria-labelledby={`cheat-section-${section.id}`}
            >
              <h3 id={`cheat-section-${section.id}`}>{section.title}</h3>
              {section.groups.map((group) => (
                <div key={group.id} className="cheat-group">
                  {group.title && <h4>{group.title}</h4>}
                  <ul>
                    {group.bindings.map((binding) => (
                      <li key={binding.action} className="cheat-row">
                        <span className="cheat-keys">
                          {binding.keys.map((key, index) => (
                            <Fragment key={key}>
                              {index > 0 && <span className="cheat-or">or</span>}
                              <kbd>{key}</kbd>
                            </Fragment>
                          ))}
                        </span>
                        <span className="cheat-label">{binding.label}</span>
                      </li>
                    ))}
                  </ul>
                </div>
              ))}
            </section>
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
