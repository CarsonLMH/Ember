import { useCallback, useEffect, useRef, useState } from 'react';
import {
  deleteFaceData,
  faceCalibrationReport,
  faceChipUrl,
  faceClusters,
  faceReject,
  faceScanStatus,
  faceSetName,
  listPersons,
  mergePersons,
  onFacesProgress,
  personFaces,
  renamePerson,
  setFacesEnabled,
  setFacesIgnored,
  undoNaming,
  type ChipRef,
  type FaceCluster,
  type FaceClusters,
  type FaceScanStatus,
  type PersonOut,
} from '../lib/ipc';

/** Chip img with the 404-retry pattern: the face route enqueues a repair on
 * miss and the retry query busts WebKit's negative cache when it lands.
 * Clicking jumps the viewer to the photo — the cheapest possible answer to
 * "who IS this?": full context, zero new UI. */
function FaceChip({
  photoId,
  faceIndex,
  size = 44,
  onJump,
  onPress,
  pressTitle,
  selected = false,
}: {
  photoId: string;
  faceIndex: number;
  size?: number;
  onJump?: (photoId: string) => void;
  /** Overrides click (e.g. selection in the loose grid). */
  onPress?: () => void;
  pressTitle?: string;
  selected?: boolean;
}) {
  const [attempt, setAttempt] = useState(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const clickable = onPress ?? (onJump ? () => onJump(photoId) : undefined);
  return (
    <img
      className={`face-chip${clickable ? ' face-chip-link' : ''}${selected ? ' face-chip-selected' : ''}`}
      style={{ width: size, height: size }}
      src={`${faceChipUrl(photoId, faceIndex)}?r=${attempt}`}
      loading="lazy"
      alt=""
      title={onPress ? pressTitle : onJump ? 'Show this photo' : undefined}
      onClick={clickable}
      onError={() => {
        // Each miss enqueues a repair; on a wiped cache the preview must
        // regenerate first, so the tail retries stretch out (~30s total).
        if (attempt < 15) {
          setTimeout(
            () => {
              if (alive.current) setAttempt((a) => a + 1);
            },
            attempt < 5 ? 800 : 2500,
          );
        }
      }}
    />
  );
}

type UndoToast =
  | { kind: 'naming'; opId: number; label: string }
  | { kind: 'dismiss'; faceIds: number[]; label: string };

export default function PeoplePanel({
  folderId,
  trashedCount,
  onClose,
  notify,
  onJump,
}: {
  folderId: number;
  /** Trash/restore/undo change folder-scoped counts — reload when it moves. */
  trashedCount: number;
  onClose: () => void;
  notify: (msg: string) => void;
  /** Move the culling cursor to a photo (chip click → context). */
  onJump: (photoId: string) => void;
}) {
  const [status, setStatus] = useState<FaceScanStatus | null>(null);
  const [persons, setPersons] = useState<PersonOut[] | null>(null);
  const [clusters, setClusters] = useState<FaceClusters | null>(null);
  const [showLoose, setShowLoose] = useState(false);
  /** Selection in the loose grid — naming/dismissing works on this set. */
  const [looseSelected, setLooseSelected] = useState<Set<number>>(new Set());
  const [expanded, setExpanded] = useState<number | null>(null);
  const [expandedFaces, setExpandedFaces] = useState<ChipRef[]>([]);
  const [renaming, setRenaming] = useState<number | null>(null);
  /** Faces ✕'d out of a mixed cluster — removed from the row and from the
   * next naming; cleared after each naming/dismiss so re-clustered faces
   * never come back invisibly hidden. */
  const [excluded, setExcluded] = useState<Set<number>>(new Set());
  /** Clusters the user expanded past the chip preview cap. */
  const [openClusters, setOpenClusters] = useState<Set<number>>(new Set());
  const [mergePrompt, setMergePrompt] = useState<{
    source: PersonOut;
    targetId: number;
    targetName: string;
  } | null>(null);
  const [undoToast, setUndoToast] = useState<UndoToast | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const load = useCallback(() => {
    void faceScanStatus(folderId).then(setStatus).catch(() => {});
    void listPersons(folderId).then(setPersons).catch(() => {});
    void faceClusters(folderId).then(setClusters).catch(() => {});
  }, [folderId]);

  // Reload on open AND whenever trash state moves (trashing a photo of a
  // named person must drop their count while the panel is open).
  useEffect(load, [load, trashedCount]);

  // Live refresh while the worker scans — debounced against event bursts.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    const unlisten = onFacesProgress((p) => {
      if (p.folderId !== null && p.folderId !== folderId) return; // stale event
      if (timer) return;
      timer = setTimeout(() => {
        timer = null;
        load();
      }, 600);
    });
    return () => {
      if (timer) clearTimeout(timer);
      void unlisten.then((fn) => fn());
    };
  }, [folderId, load]);

  useEffect(() => {
    if (expanded === null) {
      setExpandedFaces([]);
      return;
    }
    void personFaces(expanded, folderId).then(setExpandedFaces).catch(() => {});
  }, [expanded, folderId]);

  const showUndo = (toast: UndoToast) => {
    setUndoToast(toast);
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setUndoToast(null), 10_000);
  };

  const nameCluster = async (cluster: FaceCluster, name: string) => {
    if (!name.trim()) return;
    const ids = cluster.faceIds.filter((id) => !excluded.has(id));
    if (!ids.length) return;
    try {
      const res = await faceSetName(ids, name);
      showUndo({
        kind: 'naming',
        opId: res.opId,
        label: `Named ${res.affectedFaceIds.length} faces “${res.person.name}”`,
      });
      // The ✕'d faces stay unnamed and re-cluster on their own next refresh;
      // the exclusion list has done its job.
      setExcluded(new Set());
    } catch (e) {
      notify(`Naming failed — ${String(e)}`);
    }
    load();
  };

  const dismissCluster = async (cluster: FaceCluster) => {
    // ✕'d faces are exempt from the dismiss too — they're a different
    // someone, not part of "this group is not a person".
    const ids = cluster.faceIds.filter((id) => !excluded.has(id));
    try {
      const n = await setFacesIgnored(ids, true);
      showUndo({ kind: 'dismiss', faceIds: ids, label: `Dismissed ${n} faces` });
      setExcluded(new Set());
    } catch (e) {
      notify(String(e));
    }
    load();
  };

  /** The faces just ✕'d out of a group are often exactly the ones the user
   * never wants to label — finish the thought in one click. */
  const dismissRemoved = async (cluster: FaceCluster) => {
    const ids = cluster.faceIds.filter((id) => excluded.has(id));
    if (!ids.length) return;
    try {
      const n = await setFacesIgnored(ids, true);
      showUndo({ kind: 'dismiss', faceIds: ids, label: `Dismissed ${n} removed faces` });
      setExcluded((prev) => {
        const next = new Set(prev);
        for (const id of ids) next.delete(id);
        return next;
      });
    } catch (e) {
      notify(String(e));
    }
    load();
  };

  /** Name the selected loose faces — "Nati" folds them into existing Nati,
   * and the harder shots become confirmed references that improve future
   * recognition (the sweep re-runs right after). */
  const nameLoose = async (name: string) => {
    const ids = [...looseSelected];
    if (!name.trim() || !ids.length) return;
    try {
      const res = await faceSetName(ids, name);
      showUndo({
        kind: 'naming',
        opId: res.opId,
        label: `Named ${res.affectedFaceIds.length} faces “${res.person.name}”`,
      });
      setLooseSelected(new Set());
    } catch (e) {
      notify(`Naming failed — ${String(e)}`);
    }
    load();
  };

  const undo = async (toast: UndoToast) => {
    setUndoToast(null);
    try {
      if (toast.kind === 'naming') {
        const n = await undoNaming(toast.opId);
        notify(n > 0 ? `Naming undone (${n} faces restored)` : 'Nothing to undo — later edits win');
      } else {
        await setFacesIgnored(toast.faceIds, false);
        notify('Dismissed faces restored to Unnamed');
      }
    } catch (e) {
      notify(String(e));
    }
    load();
  };

  const rename = async (person: PersonOut, name: string) => {
    setRenaming(null);
    if (!name.trim() || name.trim() === person.name) return;
    try {
      const outcome = await renamePerson(person.id, name);
      if (outcome.status === 'conflict') {
        // Typo'd the same person twice — offer to merge instead of erroring.
        setMergePrompt({ source: person, targetId: outcome.targetId, targetName: outcome.targetName });
      }
    } catch (e) {
      notify(String(e));
    }
    load();
  };

  const merge = async () => {
    if (!mergePrompt) return;
    const { source, targetId, targetName } = mergePrompt;
    setMergePrompt(null);
    try {
      const moved = await mergePersons(source.id, targetId);
      notify(`Merged “${source.name}” into “${targetName}” (${moved} faces)`);
    } catch (e) {
      notify(String(e));
    }
    setExpanded(null);
    load();
  };

  const notPerson = async (face: ChipRef, person: PersonOut) => {
    try {
      await faceReject(face.faceId, person.id);
    } catch (e) {
      notify(String(e));
    }
    void personFaces(person.id, folderId).then(setExpandedFaces).catch(() => {});
    load();
  };

  const toggleEnabled = async () => {
    if (!status) return;
    try {
      await setFacesEnabled(!status.enabled);
    } catch (e) {
      notify(String(e));
    }
    load();
  };

  const deleteAll = async () => {
    setConfirmDelete(false);
    try {
      await deleteFaceData();
      notify('All face data deleted — indexing is now off');
    } catch (e) {
      notify(String(e));
    }
    load();
  };

  const calibrate = async () => {
    try {
      const r = await faceCalibrationReport();
      const s = r.summary;
      const fmt = (v: number | null) => (v === null ? '—' : v.toFixed(3));
      notify(
        `Calibration saved (${s.positives} pos / ${s.negatives} neg): ` +
          `same-person p5=${fmt(s.posP5)}, other-person p95=${fmt(s.negP95)} → ${r.path}`,
      );
    } catch (e) {
      notify(String(e));
    }
  };

  const scanning = status && status.enabled && status.scanned < status.total;

  return (
    <div className="trash-panel people-panel">
      <div className="trash-head">
        <span>People</span>
        <button onClick={onClose}>close</button>
      </div>

      {status?.engineError && (
        <div className="people-error">Face engine unavailable: {status.engineError}</div>
      )}

      {status && !status.enabled ? (
        <div className="people-disabled">
          <p>Face indexing is off{status.scanned === 0 ? ' and no face data is stored' : ''}.</p>
          <p className="people-hint">
            Turning it on indexes this folder from scratch, on this Mac only.
          </p>
          <button onClick={() => void toggleEnabled()}>Enable face indexing</button>
        </div>
      ) : (
        <>
          {status &&
            (scanning ? (
              <div className="people-progress">
                Scanning faces… {status.scanned}/{status.total}
                {status.errors > 0 ? ` (${status.errors} failed)` : ''}
              </div>
            ) : (
              status.total > 0 && (
                <div className="people-hint">
                  All {status.total} photos indexed
                  {status.errors > 0 ? ` (${status.errors} failed)` : ''}
                </div>
              )
            ))}

          {mergePrompt && (
            <div className="people-toast">
              <span>
                Merge “{mergePrompt.source.name}” into “{mergePrompt.targetName}”?
              </span>
              <span className="people-toast-btns">
                <button onClick={() => void merge()}>Merge</button>
                <button onClick={() => setMergePrompt(null)}>Cancel</button>
              </span>
            </div>
          )}

          {persons && persons.filter((p) => p.folderCount > 0).length > 0 && (
            <div className="people-section">Named</div>
          )}
          <ul>
            {persons
              ?.filter((p) => p.folderCount > 0)
              .map((p) => (
                <li key={p.id} className="people-person">
                  <button
                    className="people-row"
                    onClick={() => setExpanded(expanded === p.id ? null : p.id)}
                    title="Show this person's faces"
                  >
                    {p.repPhotoId !== null && p.repFaceIndex !== null && (
                      <FaceChip photoId={p.repPhotoId} faceIndex={p.repFaceIndex} />
                    )}
                    {renaming === p.id ? (
                      <input
                        className="people-input"
                        defaultValue={p.name}
                        autoFocus
                        onClick={(e) => e.stopPropagation()}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') void rename(p, (e.target as HTMLInputElement).value);
                          if (e.key === 'Escape') setRenaming(null);
                        }}
                        onBlur={(e) => void rename(p, e.target.value)}
                      />
                    ) : (
                      <span
                        className="people-name"
                        onDoubleClick={(e) => {
                          e.stopPropagation();
                          setRenaming(p.id);
                        }}
                        title="Double-click to rename"
                      >
                        {p.name}
                      </span>
                    )}
                    <span className="people-count">{p.folderCount}</span>
                  </button>
                  {expanded === p.id && (
                    <div className="people-faces">
                      {expandedFaces.map((f) => (
                        <span key={f.faceId} className="people-face">
                          <FaceChip photoId={f.photoId} faceIndex={f.faceIndex} onJump={onJump} />
                          <button
                            className="people-not"
                            title={`Not ${p.name}`}
                            onClick={() => void notPerson(f, p)}
                          >
                            ✕
                          </button>
                        </span>
                      ))}
                    </div>
                  )}
                </li>
              ))}
          </ul>

          {clusters && clusters.clusters.length > 0 && (
            <div className="people-section">Unnamed</div>
          )}
          {clusters?.clusters.length === 0 &&
            persons?.every((p) => p.folderCount === 0) &&
            !scanning && (
              <div className="trash-empty">
                {status && status.scanned === 0
                  ? 'No faces indexed yet.'
                  : 'No recurring faces found in this folder.'}
              </div>
            )}
          <ul>
            {clusters?.clusters.map((c) => {
              const anchor = c.faceIds[0];
              const kept = c.chips.filter((chip) => !excluded.has(chip.faceId));
              const removedHere = c.chips.length - kept.length;
              const open = openClusters.has(anchor);
              const shown = open ? kept : kept.slice(0, 8);
              return (
                <li key={anchor} className="people-cluster">
                  <div className="people-faces">
                    {shown.map((chip) => (
                      <span key={chip.faceId} className="people-face">
                        <FaceChip
                          photoId={chip.photoId}
                          faceIndex={chip.faceIndex}
                          onJump={onJump}
                        />
                        <button
                          className="people-not"
                          title="Not the same person — remove from this group"
                          onClick={() =>
                            setExcluded((prev) => new Set(prev).add(chip.faceId))
                          }
                        >
                          ✕
                        </button>
                      </span>
                    ))}
                    {kept.length > shown.length && (
                      <button
                        className="people-more"
                        title="Show every face in this group"
                        onClick={() =>
                          setOpenClusters((prev) => new Set(prev).add(anchor))
                        }
                      >
                        +{kept.length - shown.length} more
                      </button>
                    )}
                  </div>
                  <div className="people-cluster-row">
                    <span className="people-hint">
                      seen in {c.photoCount} photo{c.photoCount === 1 ? '' : 's'}
                      {removedHere > 0 && (
                        <>
                          {' · '}
                          <button
                            className="people-dim"
                            title="Put the removed faces back into this group"
                            onClick={() =>
                              setExcluded((prev) => {
                                const next = new Set(prev);
                                for (const chip of c.chips) next.delete(chip.faceId);
                                return next;
                              })
                            }
                          >
                            restore {removedHere}
                          </button>
                          {' · '}
                          <button
                            className="people-dim"
                            title="The removed faces aren't people you'll label — hide them"
                            onClick={() => void dismissRemoved(c)}
                          >
                            don't label removed
                          </button>
                        </>
                      )}
                    </span>
                    <button
                      className="people-dim"
                      title="Statues, photos of photos, people you'll never label — hide this group"
                      onClick={() => void dismissCluster(c)}
                    >
                      not a person / don't label
                    </button>
                  </div>
                  <input
                    className="people-input"
                    placeholder="Who is this?"
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        const input = e.target as HTMLInputElement;
                        void nameCluster(c, input.value);
                        input.value = '';
                      }
                    }}
                  />
                </li>
              );
            })}
          </ul>

          {clusters && clusters.loose.length > 0 && (
            <div className="people-loose">
              <button className="people-dim" onClick={() => setShowLoose((v) => !v)}>
                {showLoose ? 'hide' : 'show'} {clusters.loose.length} face
                {clusters.loose.length === 1 ? '' : 's'} seen only once
              </button>
              {showLoose && (
                <>
                  <div className="people-faces">
                    {clusters.loose.map((chip) => (
                      <FaceChip
                        key={chip.faceId}
                        photoId={chip.photoId}
                        faceIndex={chip.faceIndex}
                        selected={looseSelected.has(chip.faceId)}
                        pressTitle="Select this face"
                        onPress={() =>
                          setLooseSelected((prev) => {
                            const next = new Set(prev);
                            if (!next.delete(chip.faceId)) next.add(chip.faceId);
                            return next;
                          })
                        }
                      />
                    ))}
                  </div>
                  {looseSelected.size > 0 && (
                    <>
                      <div className="people-cluster-row">
                        <span className="people-hint">{looseSelected.size} selected</span>
                        {looseSelected.size === 1 && (
                          <button
                            className="people-dim"
                            onClick={() => {
                              const id = [...looseSelected][0];
                              const chip = clusters.loose.find((c) => c.faceId === id);
                              if (chip) onJump(chip.photoId);
                            }}
                          >
                            show photo
                          </button>
                        )}
                      </div>
                      <input
                        className="people-input"
                        placeholder={`Who is this? (names ${looseSelected.size} selected)`}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') {
                            const input = e.target as HTMLInputElement;
                            void nameLoose(input.value);
                            input.value = '';
                          }
                        }}
                      />
                    </>
                  )}
                </>
              )}
            </div>
          )}

          {undoToast && (
            <div className="people-toast">
              <span>{undoToast.label}</span>
              <button onClick={() => void undo(undoToast)}>Undo</button>
            </div>
          )}

          <div className="people-footer">
            {confirmDelete ? (
              <div className="people-confirm">
                <span>Wipe all faces &amp; names? Indexing turns off.</span>
                <span className="people-toast-btns">
                  <button className="people-danger" onClick={() => void deleteAll()}>
                    Delete
                  </button>
                  <button onClick={() => setConfirmDelete(false)}>Keep</button>
                </span>
              </div>
            ) : (
              <>
                <button onClick={() => void toggleEnabled()}>
                  {scanning ? 'Pause indexing' : 'Turn off indexing'}
                </button>
                <button className="people-danger" onClick={() => setConfirmDelete(true)}>
                  Delete all face data
                </button>
                <button
                  className="people-dim"
                  title="Save a score-distribution report for tuning recognition thresholds"
                  onClick={() => void calibrate()}
                >
                  calibration report
                </button>
              </>
            )}
          </div>
        </>
      )}
    </div>
  );
}
