import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, mocked } from 'storybook/test';
import { buildInfo, type KeyBinding } from '../lib/ipc';
import { applyBindings } from '../lib/keys';
import CheatSheet from './CheatSheet';

const sampleBindings: KeyBinding[] = [
  { action: 'next', keys: ['ArrowRight', 'ArrowDown'], label: 'Next photo' },
  { action: 'prev', keys: ['ArrowLeft', 'ArrowUp'], label: 'Previous photo' },
  { action: 'home', keys: ['Home'], label: 'Jump to first photo' },
  { action: 'end', keys: ['End'], label: 'Jump to last photo' },
  { action: 'rate1', keys: ['1'], label: 'Rate ★' },
  { action: 'rate2', keys: ['2'], label: 'Rate ★★' },
  { action: 'rate3', keys: ['3'], label: 'Rate ★★★' },
  { action: 'rate4', keys: ['4'], label: 'Rate ★★★★' },
  { action: 'rate5', keys: ['5'], label: 'Rate ★★★★★' },
  { action: 'rate0', keys: ['0'], label: 'Clear stars' },
  { action: 'auto_advance', keys: ['v'], label: 'Toggle auto-advance' },
  { action: 'trash', keys: ['x', 'Backspace', 'Delete'], label: 'Trash photo pair' },
  { action: 'undo', keys: ['Cmd+z'], label: 'Undo' },
  { action: 'redo', keys: ['Cmd+Shift+z'], label: 'Redo' },
  { action: 'filter_cycle', keys: ['u'], label: 'Cycle filter: all → unstarred → starred' },
  { action: 'filter_all', keys: ['Shift+0'], label: 'Filter: show all' },
  { action: 'filter_star1', keys: ['Shift+1'], label: 'Filter: exactly ★' },
  { action: 'filter_star2', keys: ['Shift+2'], label: 'Filter: exactly ★★' },
  { action: 'filter_star3', keys: ['Shift+3'], label: 'Filter: exactly ★★★' },
  { action: 'filter_star4', keys: ['Shift+4'], label: 'Filter: exactly ★★★★' },
  { action: 'filter_star5', keys: ['Shift+5'], label: 'Filter: exactly ★★★★★' },
  { action: 'filter_min1', keys: ['Ctrl+Shift+1'], label: 'Filter: ★ or more' },
  { action: 'filter_min2', keys: ['Ctrl+Shift+2'], label: 'Filter: ★★ or more' },
  { action: 'filter_min3', keys: ['Ctrl+Shift+3'], label: 'Filter: ★★★ or more' },
  { action: 'filter_min4', keys: ['Ctrl+Shift+4'], label: 'Filter: ★★★★ or more' },
  { action: 'filter_min5', keys: ['Ctrl+Shift+5'], label: 'Filter: ★★★★★ or more' },
  { action: 'recipe_filter', keys: ['c'], label: 'Recipe filter quick-switcher' },
  { action: 'tag_filter', keys: ['Shift+g'], label: 'Tag filter quick-switcher' },
  { action: 'person_filter', keys: ['Shift+p'], label: 'Person filter quick-switcher' },
  { action: 'focus_filter', keys: ['Shift+a'], label: 'Filter: soft at the AF point' },
  {
    action: 'sort_cycle',
    keys: ['s'],
    label: 'Cycle sort: date captured → filename → rating',
  },
  { action: 'sort_reverse', keys: ['Shift+s'], label: 'Reverse sort order' },
  { action: 'zoom_100', keys: ['z'], label: 'Toggle fit ↔ 100%' },
  { action: 'focus_zoom', keys: ['f'], label: '100% at the AF point' },
  { action: 'af_overlay', keys: ['a'], label: 'Toggle AF point overlay' },
  { action: 'histogram', keys: ['h'], label: 'Cycle histogram: off → luminance → RGB' },
  { action: 'blinkies', keys: ['b'], label: 'Toggle clipping warnings' },
  { action: 'face_badges', keys: ['Shift+f'], label: 'Toggle face badges on the photo' },
  { action: 'filmstrip', keys: ['t'], label: 'Toggle filmstrip' },
  { action: 'exif_panel', keys: ['i'], label: 'Toggle metadata panel' },
  { action: 'people_panel', keys: ['p'], label: 'Toggle People panel' },
  { action: 'tag_palette', keys: ['g'], label: 'Tag palette' },
  { action: 'immersion', keys: ['Shift+t'], label: 'Toggle picture-only mode' },
  { action: 'open_folder', keys: ['Cmd+o'], label: 'Open folder' },
  { action: 'refresh', keys: ['r'], label: 'Rescan folder' },
  { action: 'cheat_sheet', keys: ['?'], label: 'Show this cheat sheet' },
  { action: 'perf_hud', keys: ['`'], label: 'Toggle performance HUD' },
];

applyBindings(sampleBindings);

const meta = {
  title: 'Chrome/Keyboard shortcuts',
  component: CheatSheet,
  args: { onClose: fn() },
  beforeEach: async () => {
    mocked(buildInfo).mockResolvedValue({
      version: '1.4.0',
      commit: 'storybook',
      builtAt: 'synthetic fixture',
    });
  },
} satisfies Meta<typeof CheatSheet>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
  play: async ({ args, canvas, canvasElement, userEvent }) => {
    const dialog = canvas.getByRole('dialog', { name: 'Keyboard shortcuts' });
    await expect(dialog).toBeVisible();
    await expect(dialog).toHaveFocus();
    await expect(canvas.getByRole('heading', { name: 'Rate & recover' })).toBeVisible();
    await expect(canvas.getByRole('heading', { name: 'Exact rating' })).toBeVisible();
    await expect(canvas.getByRole('heading', { name: 'Rating or more' })).toBeVisible();

    await userEvent.click(canvas.getByRole('heading', { name: 'Navigate' }));
    await expect(args.onClose).not.toHaveBeenCalled();

    const backdrop = canvasElement.querySelector<HTMLElement>('.cheat-backdrop');
    if (!backdrop) throw new Error('CheatSheet backdrop did not render');
    await userEvent.click(backdrop);
    await expect(args.onClose).toHaveBeenCalledOnce();
  },
};
