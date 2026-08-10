import { useEffect, useState } from 'react';
import * as session from '../lib/session';
import * as viewer from '../lib/viewer';
import { faceAssign, faceReject, faceSetName, setFacesIgnored, type FaceOut } from '../lib/ipc';

/**
 * Named-face badges over the photo (SPEC §14) and the on-photo correction
 * menu. Named-only by default — a HUD full of "unknown" boxes would be noise
 * during culling; unnamed faces get a single quiet count that opens the panel.
 *
 * The layer is inert (pointer-events: none) except the badges themselves, so
 * pan/pinch/double-click still reach the canvas underneath.
 */
export default function FaceBadges({
  faces,
  onOpenPanel,
}: {
  faces: FaceOut[];
  onOpenPanel: () => void;
}) {
  const [, bump] = useState(0);
  const [menuFor, setMenuFor] = useState<FaceOut | null>(null);
  const [naming, setNaming] = useState(false);

  // Reposition when the canvas transform changes (resize, zoom in/out, flip).
  // In fit mode these are discrete events, not a gesture stream.
  useEffect(() => viewer.subscribe(() => bump((v) => v + 1)), []);
  useEffect(() => {
    setMenuFor(null);
    setNaming(false);
  }, [faces]);
  // Escape closes the menu (App's global handler also runs — harmless).
  useEffect(() => {
    if (!menuFor) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setMenuFor(null);
        setNaming(false);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [menuFor]);

  const named = faces.filter((f) => f.personId !== null && !f.ignored);
  const unnamed = faces.filter((f) => f.personId === null && !f.ignored).length;

  const act = async (fn: () => Promise<unknown>) => {
    setMenuFor(null);
    setNaming(false);
    try {
      await fn();
    } catch (e) {
      session.notify(String(e));
      return;
    }
    void session.peopleChanged();
  };

  return (
    <div className="face-layer">
      {named.map((f) => {
        const box = viewer.normalizedRectToCss(f.rect);
        if (!box) return null;
        return (
          <button
            key={f.faceId}
            className="face-badge"
            style={{ left: box.left, top: box.top + box.height, width: box.width }}
            title={`${f.personName}${f.assignedBy === 'auto' ? ' (auto-recognized)' : ''} — click to correct`}
            onClick={(e) => {
              e.stopPropagation();
              setMenuFor(menuFor?.faceId === f.faceId ? null : f);
              setNaming(false);
            }}
          >
            <span className="face-badge-name">{f.personName}</span>
          </button>
        );
      })}

      {named.map((f) => {
        const box = viewer.normalizedRectToCss(f.rect);
        if (!box) return null;
        return (
          <div
            key={`ring-${f.faceId}`}
            className={`face-ring${f.assignedBy === 'auto' ? ' face-ring-auto' : ''}`}
            style={{ left: box.left, top: box.top, width: box.width, height: box.height }}
          />
        );
      })}

      {unnamed > 0 && (
        <button className="face-unnamed" onClick={onOpenPanel} title="Open the People panel">
          {unnamed} unnamed face{unnamed === 1 ? '' : 's'}
        </button>
      )}

      {menuFor &&
        menuFor.personId !== null &&
        (() => {
          const { faceId, personId, personName } = menuFor;
          const box = viewer.normalizedRectToCss(menuFor.rect);
          if (!box) return null;
          const others = session.getState().persons.filter((p) => p.id !== personId && !p.hidden);
          return (
            <div
              className="face-menu"
              style={{ left: box.left, top: box.top + box.height + 26 }}
              onClick={(e) => e.stopPropagation()}
            >
              {naming ? (
                <input
                  className="people-input"
                  autoFocus
                  placeholder="Who is this?"
                  onKeyDown={(e) => {
                    if (e.key === 'Escape') setNaming(false);
                    if (e.key !== 'Enter') return;
                    const name = (e.target as HTMLInputElement).value.trim();
                    if (name) void act(() => faceSetName([faceId], name));
                  }}
                />
              ) : (
                <>
                  <div className="face-menu-head">{personName}</div>
                  <button onClick={() => void act(() => faceReject(faceId, personId))}>
                    Not {personName}
                  </button>
                  {others.map((p) => (
                    <button key={p.id} onClick={() => void act(() => faceAssign(faceId, p.id))}>
                      This is {p.name}
                    </button>
                  ))}
                  <button onClick={() => setNaming(true)}>Someone else…</button>
                  <button onClick={() => void act(() => setFacesIgnored([faceId], true))}>
                    Not a person / don't label
                  </button>
                </>
              )}
            </div>
          );
        })()}
    </div>
  );
}
