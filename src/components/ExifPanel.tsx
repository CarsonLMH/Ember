import { useEffect, useState } from 'react';
import { saveRecipe } from '../lib/ipc';
import { ensureMeta, getCachedMeta, type Meta } from '../lib/metaCache';

interface RowDef {
  label: string;
  keys: string[];
  format?: (v: string) => string;
}

interface GroupDef {
  title: string;
  rows: RowDef[];
}

/** Grouped, scannable — the opposite of a raw-JSON metadata dump. Keys are
 * exiftool -G1 names with readable values; rows with no data don't render. */
const GROUPS: GroupDef[] = [
  {
    title: 'Exposure',
    rows: [
      {
        label: 'Shutter',
        keys: ['ExifIFD:ExposureTime'],
        // Readable dumps give "1/1100" already; decimals become fractions.
        format: (v) => {
          const n = Number(v);
          if (!Number.isFinite(n)) return `${v}s`;
          return n >= 1 ? `${n}s` : `1/${Math.round(1 / n)}s`;
        },
      },
      { label: 'Aperture', keys: ['ExifIFD:FNumber'], format: (v) => `ƒ/${v}` },
      { label: 'ISO', keys: ['ExifIFD:ISO'] },
      { label: 'Exp. comp', keys: ['ExifIFD:ExposureCompensation'], format: (v) => `${v} EV` },
      { label: 'Program', keys: ['ExifIFD:ExposureProgram'] },
      { label: 'Metering', keys: ['ExifIFD:MeteringMode'] },
      { label: 'Flash', keys: ['ExifIFD:Flash'] },
    ],
  },
  {
    title: 'Lens',
    rows: [
      { label: 'Lens', keys: ['ExifIFD:LensModel', 'Composite:LensID'] },
      { label: 'Focal length', keys: ['ExifIFD:FocalLength'] },
      { label: '35mm equiv.', keys: ['ExifIFD:FocalLengthIn35mmFormat'] },
    ],
  },
  {
    title: 'Film / Recipe',
    rows: [
      { label: 'Film simulation', keys: ['FujiFilm:FilmMode'] },
      {
        // Manual DR records DevelopmentDynamicRange ("200" → DR200); Auto
        // records the chosen strength in AutoDynamicRange ("100%").
        label: 'Dynamic range',
        keys: [
          'FujiFilm:DevelopmentDynamicRange',
          'FujiFilm:AutoDynamicRange',
          'FujiFilm:DynamicRangeSetting',
        ],
        format: (v) =>
          /^\d+$/.test(v) ? `DR${v}` : v.endsWith('%') ? `Auto (DR${v.slice(0, -1)})` : v,
      },
      { label: 'White balance', keys: ['FujiFilm:WhiteBalance', 'ExifIFD:WhiteBalance'] },
      { label: 'WB shift', keys: ['FujiFilm:WhiteBalanceFineTune'] },
      { label: 'Highlight tone', keys: ['FujiFilm:HighlightTone'] },
      { label: 'Shadow tone', keys: ['FujiFilm:ShadowTone'] },
      { label: 'Color', keys: ['FujiFilm:Saturation'] },
      { label: 'Sharpness', keys: ['FujiFilm:Sharpness'] },
      { label: 'Noise reduction', keys: ['FujiFilm:NoiseReduction', 'FujiFilm:HighISONoiseReduction'] },
      { label: 'Clarity', keys: ['FujiFilm:Clarity'] },
      { label: 'Grain', keys: ['FujiFilm:GrainEffectRoughness'] },
      { label: 'Grain size', keys: ['FujiFilm:GrainEffectSize'] },
      { label: 'Color chrome', keys: ['FujiFilm:ColorChromeEffect'] },
      { label: 'CC FX blue', keys: ['FujiFilm:ColorChromeFXBlue'] },
    ],
  },
  {
    title: 'Autofocus',
    rows: [
      { label: 'AF area', keys: ['FujiFilm:AFAreaMode', 'FujiFilm:AFMode'] },
      { label: 'Focus mode', keys: ['FujiFilm:FocusMode'] },
      { label: 'Camera focus check', keys: ['FujiFilm:FocusWarning'] },
      { label: 'Camera blur check', keys: ['FujiFilm:BlurWarning'] },
    ],
  },
  {
    title: 'Camera',
    rows: [
      { label: 'Camera', keys: ['IFD0:Model'] },
      { label: 'Firmware', keys: ['IFD0:Software'] },
      { label: 'Shutter type', keys: ['FujiFilm:ShutterType'] },
      { label: 'Captured', keys: ['ExifIFD:DateTimeOriginal', 'ExifIFD:CreateDate'] },
    ],
  },
  {
    title: 'File',
    rows: [
      {
        label: 'Size',
        keys: ['System:FileSize', 'File:FileSize'],
        format: (v) => {
          const n = Number(v);
          return Number.isFinite(n) ? `${(n / 1048576).toFixed(2)} MB` : v;
        },
      },
      { label: 'Dimensions', keys: ['Composite:ImageSize'] },
      { label: 'Type', keys: ['File:FileType'] },
      { label: 'Modified', keys: ['System:FileModifyDate', 'File:FileModifyDate'] },
    ],
  },
];

function valueOf(meta: Meta, row: RowDef): string | null {
  for (const k of row.keys) {
    const v = meta[k];
    if (v === undefined || v === null || v === '') continue;
    const s = String(v);
    return row.format ? row.format(s) : s;
  }
  return null;
}

/** Inline "save these settings as a new recipe" flow for unknown photos. */
function RecipeHeader({
  photoId,
  recipe,
  onRecipeSaved,
}: {
  photoId: string;
  recipe: { name: string | null; hasMeta: boolean } | null;
  onRecipeSaved: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setEditing(false);
    setName('');
    setError(null);
  }, [photoId]);

  // Photo known to carry no MakerNotes: nothing to match against.
  if (recipe && !recipe.hasMeta) return null;

  const save = async () => {
    try {
      await saveRecipe(photoId, name);
      setEditing(false);
      onRecipeSaved();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section className="exif-group">
      <h3>Recipe</h3>
      {recipe === null ? (
        <div className="exif-row">
          <span className="exif-label">Matching</span>
          <span className="exif-value">…</span>
        </div>
      ) : recipe.name ? (
        <div className="exif-row">
          <span className="exif-label">Matched</span>
          <span className="exif-value">{recipe.name}</span>
        </div>
      ) : editing ? (
        <div className="recipe-save">
          <input
            autoFocus
            placeholder="recipe name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === 'Enter') void save();
              if (e.key === 'Escape') setEditing(false);
            }}
          />
          <button onClick={() => void save()} disabled={!name.trim()}>
            save
          </button>
          {error && <div className="recipe-error">{error}</div>}
        </div>
      ) : (
        <div className="recipe-save">
          <span className="exif-label">No recipe matches</span>
          <button onClick={() => setEditing(true)}>save as new recipe…</button>
        </div>
      )}
    </section>
  );
}

export default function ExifPanel({
  photoId,
  recipe,
  focusScore,
  onRecipeSaved,
}: {
  photoId: string;
  recipe: { name: string | null; hasMeta: boolean } | null;
  /** Ember's AF-patch sharpness score; null until the focus sweep gets there. */
  focusScore: number | null;
  onRecipeSaved: () => void;
}) {
  const [meta, setMeta] = useState<Meta | null>(getCachedMeta(photoId) ?? null);
  // Only admit to loading when it's actually slow — prefetched neighbors
  // resolve in one frame and must not flash a loading state.
  const [slow, setSlow] = useState(false);

  useEffect(() => {
    const cached = getCachedMeta(photoId);
    if (cached) {
      setMeta(cached);
      setSlow(false);
      return;
    }
    setMeta(null);
    setSlow(false);
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const slowTimer = setTimeout(() => {
      if (alive) setSlow(true);
    }, 200);
    const load = (attempt: number) => {
      void ensureMeta(photoId).then((parsed) => {
        if (!alive) return;
        if (parsed) setMeta(parsed);
        else if (attempt < 12) timer = setTimeout(() => load(attempt + 1), 800);
      });
    };
    load(0);
    return () => {
      alive = false;
      clearTimeout(slowTimer);
      if (timer) clearTimeout(timer);
    };
  }, [photoId]);

  return (
    <div className="exif-panel">
      <RecipeHeader photoId={photoId} recipe={recipe} onRecipeSaved={onRecipeSaved} />
      {meta === null && slow && <div className="exif-loading">reading metadata…</div>}
      {meta &&
        GROUPS.map((group) => {
          const rows = group.rows
            .map((row) => ({ label: row.label, value: valueOf(meta, row) }))
            .filter((r) => r.value !== null);
          // Ember's own score joins the camera's AF facts (higher = sharper,
          // comparable within a burst — see settings.toml [focus]).
          if (group.title === 'Autofocus' && focusScore !== null) {
            rows.unshift({ label: 'Ember score', value: String(Math.round(focusScore)) });
          }
          if (rows.length === 0) return null;
          return (
            <section key={group.title} className="exif-group">
              <h3>{group.title}</h3>
              {rows.map((r) => (
                <div key={r.label} className="exif-row">
                  <span className="exif-label">{r.label}</span>
                  <span className="exif-value">{r.value}</span>
                </div>
              ))}
            </section>
          );
        })}
    </div>
  );
}
