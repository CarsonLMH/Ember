import { describe, expect, it } from 'vitest';
import type { KeyBinding } from '../lib/ipc';
import { groupBindings } from './shortcutGroups';

const actions = [
  'next',
  'prev',
  'home',
  'end',
  'rate1',
  'rate2',
  'rate3',
  'rate4',
  'rate5',
  'rate0',
  'auto_advance',
  'trash',
  'undo',
  'redo',
  'filter_cycle',
  'filter_all',
  'filter_star1',
  'filter_star2',
  'filter_star3',
  'filter_star4',
  'filter_star5',
  'filter_min1',
  'filter_min2',
  'filter_min3',
  'filter_min4',
  'filter_min5',
  'recipe_filter',
  'tag_filter',
  'person_filter',
  'focus_filter',
  'sort_cycle',
  'sort_reverse',
  'zoom_100',
  'focus_zoom',
  'af_overlay',
  'histogram',
  'blinkies',
  'face_badges',
  'filmstrip',
  'exif_panel',
  'people_panel',
  'tag_palette',
  'immersion',
  'open_folder',
  'refresh',
  'cheat_sheet',
  'perf_hud',
] as const;

const bindings: KeyBinding[] = actions.map((action) => ({
  action,
  keys: [`custom-${action}`],
  label: `Label ${action}`,
}));

describe('groupBindings', () => {
  it('places every current action exactly once in intent-first section order', () => {
    const sections = groupBindings(bindings);
    expect(sections.map((section) => section.title)).toEqual([
      'Navigate',
      'Rate & recover',
      'Filter & sort',
      'Inspect photo',
      'Panels & display',
      'Folder & help',
      'Other',
    ]);

    const flattened = sections.flatMap((section) =>
      section.groups.flatMap((group) => group.bindings),
    );
    expect(flattened).toHaveLength(bindings.length);
    expect(new Set(flattened.map((binding) => binding.action)).size).toBe(bindings.length);
    for (const binding of bindings) {
      expect(flattened.find((item) => item.action === binding.action)).toBe(binding);
    }
  });

  it('splits exact and minimum rating filters into findable subgroups', () => {
    const filters = groupBindings(bindings).find((section) => section.id === 'filter-sort');
    expect(filters?.groups.map((group) => group.title)).toEqual([
      'General',
      'Exact rating',
      'Rating or more',
      'Other filters',
      'Sort',
    ]);
    expect(
      filters?.groups.find((group) => group.id === 'filter-exact')?.bindings.map((b) => b.action),
    ).toEqual(['filter_star1', 'filter_star2', 'filter_star3', 'filter_star4', 'filter_star5']);
    expect(
      filters?.groups.find((group) => group.id === 'filter-min')?.bindings.map((b) => b.action),
    ).toEqual(['filter_min1', 'filter_min2', 'filter_min3', 'filter_min4', 'filter_min5']);
  });

  it('uses the curated action order instead of the backend input order', () => {
    const sections = groupBindings([...bindings].reverse());
    const actionsIn = (sectionId: string) =>
      sections
        .find((section) => section.id === sectionId)
        ?.groups.flatMap((group) => group.bindings.map((binding) => binding.action));

    expect(actionsIn('rate-recover')).toEqual([
      'rate1',
      'rate2',
      'rate3',
      'rate4',
      'rate5',
      'rate0',
      'auto_advance',
      'trash',
      'undo',
      'redo',
    ]);
    expect(actionsIn('inspect')).toEqual([
      'zoom_100',
      'focus_zoom',
      'af_overlay',
      'histogram',
      'blinkies',
      'face_badges',
    ]);
    expect(actionsIn('panels-display')).toEqual([
      'filmstrip',
      'exif_panel',
      'people_panel',
      'tag_palette',
      'immersion',
    ]);
  });

  it('keeps unknown future actions visible under Other', () => {
    const future: KeyBinding = {
      action: 'future_action',
      keys: ['Hyper+f'],
      label: 'Future action',
    };
    const sections = groupBindings([future]);
    expect(sections).toHaveLength(1);
    expect(sections[0].id).toBe('other');
    expect(sections[0].groups[0].bindings[0]).toBe(future);
  });
});
