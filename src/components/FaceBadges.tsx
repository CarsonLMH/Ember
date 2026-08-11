import { useEffect, useState } from 'react';
import * as session from '../lib/session';
import * as viewer from '../lib/viewer';
import { faceReject, faceSetName, setFacesIgnored, type FaceOut } from '../lib/ipc';

/**
 * Named-face badges over the photo (SPEC §14) and the on-photo correction
 * menu. Named-only by default — a HUD full of "unknown" boxes would be noise
 * during culling; unnamed faces get a single quiet count that reveals them
 * on demand, so a face you just un-named is still labelable here (`p` opens
 * the People panel, so this layer doesn't duplicate that).
 *
 * The layer is inert (pointer-events: none) except the badges themselves, so
 * pan/pinch/double-click still reach the canvas underneath.
 */
export default function FaceBadges({ faces }: { faces: FaceOut[] }) {
  const [, bump] = useState(0);
  const [menuFor, setMenuFor] = useState<FaceOut | null>(null);
  const [naming, setNaming] = useState(false);
  /** Unnamed faces stay invisible until asked for — but "Not X" leaves a face
   * unnamed, and it must be re-labelable without hunting through the panel. */
  const [showUnnamed, setShowUnnamed] = useState(false);

  // Reposition when the canvas transform changes (resize, zoom in/out, flip).
  // In fit mode these are discrete events, not a gesture stream.
  useEffect(() => viewer.subscribe(() => bump((v) => v + 1)), []);
  useEffect(() => {
    setMenuFor(null);
    setNaming(false);
    setShowUnnamed(false);
  }, [faces]);
  // Escape or a click anywhere else closes the menu. The pointerdown capture
  // listener sees canvas clicks too (the layer is inert), so any click that
  // isn't inside the menu dismisses it.
  useEffect(() => {
    if (!menuFor) return;
    const close = () => {
      setMenuFor(null);
      setNaming(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close();
    };
    const onPointer = (e: PointerEvent) => {
      const target = e.target as HTMLElement | null;
      if (target?.closest('.face-menu, .face-badge')) return;
      close();
    };
    window.addEventListener('keydown', onKey);
    window.addEventListener('pointerdown', onPointer, true);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('pointerdown', onPointer, true);
    };
  }, [menuFor]);

  const named = faces.filter((f) => f.personId !== null && !f.ignored);
  const unnamedFaces = faces.filter((f) => f.personId === null && !f.ignored);

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
            // Anchored to the box's horizontal centre, but never sized by it:
            // a distant face gives a tiny box, and a truncated name is useless.
            style={{ left: box.left + box.width / 2, top: box.top + box.height }}
            title={`${f.personName}${f.assignedBy === 'auto' ? ' (auto-recognized)' : ''} — click to correct`}
            onClick={(e) => {
              e.stopPropagation();
              setMenuFor(menuFor?.faceId === f.faceId ? null : f);
              setNaming(false);
            }}
          >
            {f.personName}
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

      {/* Unnamed faces, revealed on demand: dashed grey boxes, click to name. */}
      {showUnnamed &&
        unnamedFaces.map((f) => {
          const box = viewer.normalizedRectToCss(f.rect);
          if (!box) return null;
          return (
            <button
              key={`u-${f.faceId}`}
              className="face-ring face-ring-unnamed"
              style={{ left: box.left, top: box.top, width: box.width, height: box.height }}
              title="Name this face"
              onClick={(e) => {
                e.stopPropagation();
                setMenuFor(f);
                setNaming(true);
              }}
            />
          );
        })}

      {unnamedFaces.length > 0 && (
        <span className="face-unnamed-row">
          <button
            className="face-unnamed"
            onClick={() => setShowUnnamed((v) => !v)}
            title="Show unnamed faces on this photo so you can name them"
          >
            {unnamedFaces.length} unnamed face{unnamedFaces.length === 1 ? '' : 's'}
            {showUnnamed ? ' — click a box to name' : ''}
          </button>
        </span>
      )}

      {menuFor &&
        (() => {
          const { faceId, personId, personName } = menuFor;
          const box = viewer.normalizedRectToCss(menuFor.rect);
          if (!box) return null;
          // An unnamed face has nothing to correct — go straight to naming.
          const nameOnly = personId === null || naming;
          return (
            <div
              className="face-menu"
              style={{ left: box.left + box.width / 2, top: box.top + box.height + 26 }}
              onClick={(e) => e.stopPropagation()}
            >
              {nameOnly ? (
                <>
                  <input
                    className="people-input"
                    autoFocus
                    list="face-menu-names"
                    placeholder="Who is this?"
                    onKeyDown={(e) => {
                      if (e.key === 'Escape') {
                        setNaming(false);
                        if (personId === null) setMenuFor(null);
                      }
                      if (e.key !== 'Enter') return;
                      const name = (e.target as HTMLInputElement).value.trim();
                      if (name) void act(() => faceSetName([faceId], name));
                    }}
                  />
                  {/* Existing names autocomplete; a new one creates a person. */}
                  <datalist id="face-menu-names">
                    {session.getState().persons.map((p) => (
                      <option key={p.id} value={p.name} />
                    ))}
                  </datalist>
                </>
              ) : (
                <>
                  <div className="face-menu-head">{personName}</div>
                  {/* Correction only. Reassignment goes through the name
                      input (typing an existing name folds into that person),
                      so the menu doesn't grow with the roster. */}
                  <button onClick={() => void act(() => faceReject(faceId, personId!))}>
                    Not {personName}
                  </button>
                  <button onClick={() => setNaming(true)}>This is someone else…</button>
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
