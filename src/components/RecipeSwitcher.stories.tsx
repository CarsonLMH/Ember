import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn } from 'storybook/test';
import RecipeSwitcher from './RecipeSwitcher';

const meta = {
  title: 'Chrome/Recipe switcher',
  component: RecipeSwitcher,
  args: { onClose: fn() },
} satisfies Meta<typeof RecipeSwitcher>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  play: async ({ args, canvas, userEvent }) => {
    await expect(canvas.getByText(/No recipes yet/)).toBeVisible();
    await userEvent.keyboard('{Escape}');
    await expect(args.onClose).toHaveBeenCalledOnce();
  },
};
