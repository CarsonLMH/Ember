/** Freshness guard for a component whose async loads can outlive their
 * relevance (the People panel's folder-scoped fetches).
 *
 * Each `begin()` supersedes every earlier load, and `dispose()` invalidates
 * them all. The panel is remounted (React `key={folderId}`) on folder change,
 * so a folder switch IS a dispose: promises started under the old folder can
 * never pass the new folder's guard, because they don't share one. A stale
 * load's `fresh()` gates BOTH its state writes and its failure notifications
 * — an old folder's error toast is as stale as its data.
 */
export interface LoadGuard {
  /** Start a load; the returned probe answers "am I still the newest, on a
   * live component?" after each await. */
  begin(): () => boolean;
  /** Unmount: every outstanding probe answers false forever. */
  dispose(): void;
}

export function makeLoadGuard(): LoadGuard {
  let seq = 0;
  let disposed = false;
  return {
    begin(): () => boolean {
      const mine = ++seq;
      return () => !disposed && mine === seq;
    },
    dispose(): void {
      disposed = true;
    },
  };
}
