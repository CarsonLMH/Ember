import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn } from 'storybook/test';
import PersonSwitcher from './PersonSwitcher';

const meta = {
  title: 'Chrome/Person switcher',
  component: PersonSwitcher,
  args: { onClose: fn() },
} satisfies Meta<typeof PersonSwitcher>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  play: async ({ args, canvas, userEvent }) => {
    await expect(canvas.getByText(/Nobody named in this folder yet/)).toBeVisible();
    await userEvent.keyboard('{Escape}');
    await expect(args.onClose).toHaveBeenCalledOnce();
  },
};
