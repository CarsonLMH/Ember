# Ember's metadata contract

What Ember writes, where, and what other tools may rely on. This is a
stability promise: changes to this contract are breaking changes and get a
major version bump and a migration note.

## Where verdicts live

| File kind | Rating | Tags | Notes |
|---|---|---|---|
| JPEG | embedded XMP (`xmp:Rating`) + macOS xattr | embedded XMP (`dc:subject`) | Write is atomic (temp + rename); image data stays byte-identical — hash-verified in tests |
| RAF | `.xmp` sidecar next to the file | same sidecar | **RAF files are never modified.** Sidecar follows the Adobe convention: same basename, `.xmp` extension |
| JPEG+RAF pair | both of the above | both | Whichever file a downstream tool reads, the verdict is present |

Additionally, every file with a rating gets the Finder/Spotlight xattr
`com.apple.metadata:kMDItemStarRating`, so ratings are visible in Finder and
any Mac tool that reads it.

## Fields

- **`xmp:Rating`** — integer 0–5. 0 is an explicit "cleared" verdict, not an
  absence. Ember never writes negative ratings (no reject flag exists).
- **`dc:subject`** — an `rdf:Bag` of free-form tag strings, replaced as a
  whole on every write (exiftool list semantics: clear, then re-add).
  Ordering is not meaningful and not preserved.
- **Capture time** — read-only. Ember reads `EXIF DateTimeOriginal` (falling
  back to file mtime) for ordering, and never writes any date field.

## Precedence on read (adoption)

When Ember first sees a photo it has no journal history for, it adopts an
existing rating so verdicts made in other tools appear. Read order:

1. `kMDItemStarRating` xattr (a Mac culler's live value)
2. Embedded XMP rating (JPEG)
3. `.xmp` sidecar rating (RAF)

Adoption never overwrites a verdict the user made inside Ember — the journal
is authoritative once an action exists.

## Durability semantics

Verdicts are acknowledged only after landing in Ember's append-only local
journal (SQLite, WAL, `synchronous=FULL`). File writes trail seconds behind
through a crash-safe write-behind queue, which drains on quit (bounded at
5s). A crash between journal and file write is repaired on next launch —
the journal replays. Third-party tools should treat the files as
eventually-consistent within seconds of the last user action, and always
consistent after a clean quit.

## What Ember will never write

- Any byte of a RAF file.
- Date/time fields, GPS, or any capture metadata.
- Color labels, pick/reject flags (they don't exist in Ember's model).
- Anything outside `xmp:Rating`, `dc:subject`, and the star xattr.
- Face data (SPEC §14) — deliberately DB-only: embeddings, person names and
  face rects live in ember.sqlite3 and are never written to files or
  sidecars. ("Export people as XMP keywords" is an open fast-follow.)
