import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, mocked } from 'storybook/test';
import { buildInfo, type KeyBinding } from '../lib/ipc';
import { applyBindings } from '../lib/keys';
import CheatSheet from './CheatSheet';

const sampleBindings: KeyBinding[] = [
  { action: 'next', keys: ['ArrowRight', 'ArrowDown'], label: 'Next photo' },
  { action: 'prev', keys: ['ArrowLeft', 'ArrowUp'], label: 'Previous photo' },
  { action: 'rate1', keys: ['1'], label: 'Rate ★' },
  { action: 'rate5', keys: ['5'], label: 'Rate ★★★★★' },
  { action: 'trash', keys: ['x', 'Backspace', 'Delete'], label: 'Trash photo pair' },
  { action: 'undo', keys: ['Cmd+z'], label: 'Undo' },
  { action: 'recipes', keys: ['c'], label: 'Recipe filter quick-switcher' },
  { action: 'tags', keys: ['g'], label: 'Tag palette' },
  { action: 'people', keys: ['p'], label: 'Toggle People panel' },
  { action: 'metadata', keys: ['i'], label: 'Toggle metadata panel' },
  { action: 'filmstrip', keys: ['t'], label: 'Toggle filmstrip' },
  { action: 'help', keys: ['?'], label: 'Show this cheat sheet' },
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
    await expect(canvas.getByText('Keyboard shortcuts')).toBeVisible();
    const backdrop = canvasElement.querySelector<HTMLElement>('.cheat-backdrop');
    if (!backdrop) throw new Error('CheatSheet backdrop did not render');
    await userEvent.click(backdrop);
    await expect(args.onClose).toHaveBeenCalledOnce();
  },
};
