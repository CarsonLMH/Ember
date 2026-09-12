import type { KeyBinding } from '../lib/ipc';

interface GroupDefinition {
  id: string;
  title: string | null;
  actions: readonly string[];
}

interface SectionDefinition {
  id: string;
  title: string;
  groups: readonly GroupDefinition[];
}

export interface ShortcutGroup {
  id: string;
  title: string | null;
  bindings: KeyBinding[];
}

export interface ShortcutSection {
  id: string;
  title: string;
  groups: ShortcutGroup[];
}

const SECTIONS: readonly SectionDefinition[] = [
  {
    id: 'navigate',
    title: 'Navigate',
    groups: [{ id: 'navigate-main', title: null, actions: ['next', 'prev', 'home', 'end'] }],
  },
  {
    id: 'rate-recover',
    title: 'Rate & recover',
    groups: [
      {
        id: 'rate-recover-main',
        title: null,
        actions: [
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
        ],
      },
    ],
  },
  {
    id: 'filter-sort',
    title: 'Filter & sort',
    groups: [
      { id: 'filter-general', title: 'General', actions: ['filter_cycle', 'filter_all'] },
      {
        id: 'filter-exact',
        title: 'Exact rating',
        actions: ['filter_star1', 'filter_star2', 'filter_star3', 'filter_star4', 'filter_star5'],
      },
      {
        id: 'filter-min',
        title: 'Rating or more',
        actions: ['filter_min1', 'filter_min2', 'filter_min3', 'filter_min4', 'filter_min5'],
      },
      {
        id: 'filter-other',
        title: 'Other filters',
        actions: ['recipe_filter', 'tag_filter', 'person_filter', 'focus_filter'],
      },
      { id: 'sort', title: 'Sort', actions: ['sort_cycle', 'sort_reverse'] },
    ],
  },
  {
    id: 'inspect',
    title: 'Inspect photo',
    groups: [
      {
        id: 'inspect-main',
        title: null,
        actions: ['zoom_100', 'focus_zoom', 'af_overlay', 'histogram', 'blinkies', 'face_badges'],
      },
    ],
  },
  {
    id: 'panels-display',
    title: 'Panels & display',
    groups: [
      {
        id: 'panels-display-main',
        title: null,
        actions: ['filmstrip', 'exif_panel', 'people_panel', 'tag_palette', 'immersion'],
      },
    ],
  },
  {
    id: 'folder-help',
    title: 'Folder & help',
    groups: [
      {
        id: 'folder-help-main',
        title: null,
        actions: ['open_folder', 'refresh', 'cheat_sheet'],
      },
    ],
  },
  {
    id: 'other',
    title: 'Other',
    groups: [{ id: 'other-main', title: null, actions: ['perf_hud'] }],
  },
];

const actionGroup = new Map<string, string>();
for (const section of SECTIONS) {
  for (const group of section.groups) {
    for (const action of group.actions) actionGroup.set(action, group.id);
  }
}

/** Presentation-only grouping. Unknown actions stay discoverable under Other. */
export function groupBindings(bindings: KeyBinding[]): ShortcutSection[] {
  const buckets = new Map<string, KeyBinding[]>();
  for (const binding of bindings) {
    const groupId = actionGroup.get(binding.action) ?? 'other-main';
    const bucket = buckets.get(groupId) ?? [];
    bucket.push(binding);
    buckets.set(groupId, bucket);
  }

  return SECTIONS.flatMap((section) => {
    const groups = section.groups.flatMap((group) => {
      const order = new Map(group.actions.map((action, index) => [action, index]));
      const grouped = [...(buckets.get(group.id) ?? [])].sort(
        (a, b) =>
          (order.get(a.action) ?? Number.MAX_SAFE_INTEGER) -
          (order.get(b.action) ?? Number.MAX_SAFE_INTEGER),
      );
      return grouped.length > 0 ? [{ id: group.id, title: group.title, bindings: grouped }] : [];
    });
    return groups.length > 0 ? [{ id: section.id, title: section.title, groups }] : [];
  });
}
