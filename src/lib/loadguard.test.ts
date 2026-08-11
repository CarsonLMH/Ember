import { describe, expect, it } from 'vitest';
import { makeLoadGuard } from './loadguard';

/** The People panel's folder-ownership transition, extracted: the panel is
 * remounted (`key={folderId}`) on folder change, so its guard is disposed and
 * a fresh instance gets a fresh one. These tests pin the two properties the
 * component relies on; what they cannot pin — that React really does remount
 * on a key change — is React's own contract, and rendering-level coverage
 * remains an acknowledged gap (no DOM harness in this repo). */
describe('makeLoadGuard', () => {
  it('a newer load supersedes every older one', () => {
    const guard = makeLoadGuard();
    const first = guard.begin();
    expect(first()).toBe(true);
    const second = guard.begin();
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it('dispose (unmount = folder switch) kills every outstanding probe', () => {
    const guard = makeLoadGuard();
    const inflight = guard.begin();
    guard.dispose();
    expect(inflight()).toBe(false);
    // Nothing revives a disposed guard — a probe minted after dispose is
    // stale too (the component is gone; its successor has its own guard).
    expect(guard.begin()()).toBe(false);
  });

  it('an old promise resolving after the transition fails BOTH its state write and its failure toast', async () => {
    const guard = makeLoadGuard();
    const applied: string[] = [];
    const notified: string[] = [];
    const load = (label: string, outcome: 'ok' | 'fail') => {
      const fresh = guard.begin();
      return Promise.resolve().then(() => {
        if (outcome === 'ok') {
          if (fresh()) applied.push(label);
        } else if (fresh()) {
          notified.push(label);
        }
      });
    };
    const slowOld = load('old-folder', 'ok');
    const slowOldFailure = load('old-folder-error', 'fail');
    guard.dispose(); // the folder changed under both in-flight loads
    await Promise.all([slowOld, slowOldFailure]);
    expect(applied).toEqual([]);
    expect(notified).toEqual([]);
  });
});
