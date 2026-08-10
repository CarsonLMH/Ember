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
  undoNaming,
  type ChipRef,
  type FaceCluster,
  type FaceScanStatus,
  type PersonOut,
} from '../lib/ipc';

/** Chip img with the 404-retry pattern: the face route enqueues a repair on
 * miss and the retry query busts WebKit's negative cache when it lands. */
function FaceChip({ photoId, faceIndex, size = 44 }: { photoId: string; faceIndex: number; size?: number }) {
  const [attempt, setAttempt] = useState(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  return (
    <img
      className="face-chip"
      style={{ width: size, height: size }}
      src={`${faceChipUrl(photoId, faceIndex)}?r=${attempt}`}
      loading="lazy"
      alt=""
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

interface UndoToast {
  opId: number;
  label: string;
}

export default function PeoplePanel({
  folderId,
  trashedCount,
  onClose,
  notify,
}: {
  folderId: number;
  /** Trash/restore/undo change folder-scoped counts — reload when it moves. */
  trashedCount: number;
  onClose: () => void;
  notify: (msg: string) => void;
}) {
  const [status, setStatus] = useState<FaceScanStatus | null>(null);
  const [persons, setPersons] = useState<PersonOut[] | null>(null);
  const [clusters, setClusters] = useState<FaceCluster[] | null>(null);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [expandedFaces, setExpandedFaces] = useState<ChipRef[]>([]);
  const [renaming, setRenaming] = useState<number | null>(null);
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

  const showUndo = (opId: number, label: string) => {
    setUndoToast({ opId, label });
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setUndoToast(null), 10_000);
  };

  const nameCluster = async (cluster: FaceCluster, name: string) => {
    if (!name.trim()) return;
    try {
      const res = await faceSetName(cluster.faceIds, name);
      showUndo(res.opId, `Named ${res.affectedFaceIds.length} faces “${res.person.name}”`);
    } catch (e) {
      notify(`Naming failed — ${String(e)}`);
    }
    load();
  };

  const undo = async (opId: number) => {
    setUndoToast(null);
    try {
      const n = await undoNaming(opId);
      notify(n > 0 ? `Naming undone (${n} faces restored)` : 'Nothing to undo — later edits win');
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
                          <FaceChip photoId={f.photoId} faceIndex={f.faceIndex} />
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

          {clusters && clusters.length > 0 && <div className="people-section">Unnamed</div>}
          {clusters?.length === 0 && persons?.every((p) => p.folderCount === 0) && !scanning && (
            <div className="trash-empty">
              {status && status.scanned === 0
                ? 'No faces indexed yet.'
                : 'No recurring faces found in this folder.'}
            </div>
          )}
          <ul>
            {clusters?.map((c) => (
              <li key={c.faceIds[0]} className="people-cluster">
                <div className="people-faces">
                  {c.chips.map((chip) => (
                    <FaceChip key={chip.faceId} photoId={chip.photoId} faceIndex={chip.faceIndex} />
                  ))}
                  {c.size > c.chips.length && (
                    <span className="people-more">+{c.size - c.chips.length}</span>
                  )}
                </div>
                <div className="people-hint">
                  seen in {c.photoCount} photo{c.photoCount === 1 ? '' : 's'}
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
            ))}
          </ul>

          {undoToast && (
            <div className="people-toast">
              <span>{undoToast.label}</span>
              <button onClick={() => void undo(undoToast.opId)}>Undo</button>
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
