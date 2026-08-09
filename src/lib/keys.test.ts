import { beforeEach, describe, expect, it } from 'vitest';
import { actionFor, applyBindings } from './keys';

function ev(partial: Partial<KeyboardEvent>): KeyboardEvent {
  return {
    key: '',
    code: '',
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...partial,
  } as KeyboardEvent;
}

describe('actionFor', () => {
  beforeEach(() => {
    applyBindings([
      { action: 'sort_cycle', keys: ['s'], label: 'Cycle sort' },
      { action: 'refresh', keys: ['r'], label: 'Rescan folder' },
      { action: 'undo', keys: ['Cmd+z'], label: 'Undo' },
      { action: 'redo', keys: ['Cmd+Shift+z'], label: 'Redo' },
      { action: 'open_folder', keys: ['Cmd+o'], label: 'Open folder' },
      { action: 'filter_star1', keys: ['Shift+1'], label: 'Filter ★' },
      { action: 'cheat_sheet', keys: ['?'], label: 'Cheat sheet' },
      { action: 'perf_hud', keys: ['`'], label: 'Perf HUD' },
    ]);
  });

  it('matches bare keys case-insensitively', () => {
    expect(actionFor(ev({ key: 's', code: 'KeyS' }))).toBe('sort_cycle');
    expect(actionFor(ev({ key: 'S', code: 'KeyS', shiftKey: true }))).toBe('sort_cycle');
  });

  it('matches declared modifier combos in any spelled order', () => {
    applyBindings([{ action: 'redo', keys: ['Shift+Cmd+z'], label: 'Redo' }]);
    expect(
      actionFor(ev({ key: 'z', code: 'KeyZ', metaKey: true, shiftKey: true })),
    ).toBe('redo');
  });

  it('matches Cmd combos', () => {
    expect(actionFor(ev({ key: 'z', code: 'KeyZ', metaKey: true }))).toBe('undo');
    expect(actionFor(ev({ key: 'o', code: 'KeyO', metaKey: true }))).toBe('open_folder');
    expect(
      actionFor(ev({ key: 'z', code: 'KeyZ', metaKey: true, shiftKey: true })),
    ).toBe('redo');
  });

  it('recovers shifted digits from the code', () => {
    expect(actionFor(ev({ key: '!', code: 'Digit1', shiftKey: true }))).toBe('filter_star1');
  });

  it('matches symbol bindings where Shift is implicit', () => {
    expect(actionFor(ev({ key: '?', code: 'Slash', shiftKey: true }))).toBe('cheat_sheet');
    expect(actionFor(ev({ key: '`', code: 'Backquote' }))).toBe('perf_hud');
  });

  it('returns null for unbound keys', () => {
    expect(actionFor(ev({ key: 'q', code: 'KeyQ' }))).toBeNull();
  });

  it('never falls through a held modifier to a bare-key action', () => {
    expect(actionFor(ev({ key: 's', code: 'KeyS', metaKey: true }))).toBeNull();
    expect(actionFor(ev({ key: 'r', code: 'KeyR', metaKey: true }))).toBeNull();
    expect(actionFor(ev({ key: 's', code: 'KeyS', ctrlKey: true }))).toBeNull();
    expect(actionFor(ev({ key: 's', code: 'KeyS', altKey: true }))).toBeNull();
    expect(
      actionFor(ev({ key: 's', code: 'KeyS', metaKey: true, shiftKey: true })),
    ).toBeNull();
  });
});
