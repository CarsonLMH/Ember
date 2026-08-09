# Rename record — codename "ApolloTwo" → Ember (completed 2026-08-07)

The codename purge ran to completion on 2026-08-07 (the full runbook, and the complete pre-rename development history, live in the local private archive at `~/dev/Ember-archive`). Naming triple: **Ember** / `ember` / `com.cleung.ember`. The repo was simultaneously moved from `~/Documents/Projects/ApolloTwo` to `~/dev/Ember`.

## Deliberate survivors of the old name — the complete allowed list

Any occurrence of the codename NOT on this list is a bug:

1. **`src-tauri/src/lib.rs` — `migrate_codename_data()`**: names `com.cleung.apollotwo` and `apollotwo.sqlite3` on purpose, to adopt the old data dir (verdict journal + WAL, config TOMLs, perf-reports) on first launch. Covered by `codename_migration_moves_journal_and_never_clobbers`. **Remove the function, its call, and its test after a release or two**, once the migration has run on every machine that matters (currently: one).
2. **This file.**
3. **The local private archive** (`~/dev/Ember-archive`): holds the full pre-public history, including codename-era commits. Public history begins at v1.1 with a single clean commit; nothing codename-era ships in it.

## Still to do outside the repo (after the first Ember build ships via ship.sh)

- Trash `/Applications/ApolloTwo.app` (ApolloOne.app is the *competitor's* app — leave it).
- Delete `~/Library/Caches/com.cleung.apollotwo/` (previews regenerate under the new identifier).
- Delete `~/Library/Preferences/com.cleung.apollotwo.plist` and `~/Library/Saved Application State/com.cleung.apollotwo.savedState` if present.
- The five localStorage prefs (filmstrip, exif panel, auto-advance, histogram mode, blinkies) reset to defaults under the new identifier — accepted loss.
- Check the app icon artwork for any baked-in old-name text; regenerate if so.
- Before *public* launch: run the name-availability checks for "Ember" (MAS search, trademark, domain). Known neighbors: Realmac's discontinued Ember Mac app (~2016), Ember.js, Ember mugs — none in photo culling.

## Verification (run any time; expected result)

```sh
grep -ri "apollotwo" . --exclude-dir=.git --exclude-dir=node_modules --exclude-dir=target --exclude-dir=dist
# Expected: hits ONLY in docs/RENAME.md, README.md (one line), and lib.rs (migration fn + test).
```
