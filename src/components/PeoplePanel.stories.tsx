import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, mocked } from 'storybook/test';
import {
  faceClusters,
  faceScanStatus,
  listPersons,
  onFacesProgress,
  type FaceScanStatus,
} from '../lib/ipc';
import PeoplePanel from './PeoplePanel';

const setEmptyState = (status: FaceScanStatus) => {
  mocked(faceScanStatus).mockResolvedValue(status);
  mocked(listPersons).mockResolvedValue([]);
  mocked(faceClusters).mockResolvedValue({ clusters: [], loose: [] });
  mocked(onFacesProgress).mockResolvedValue(() => {});
};

const meta = {
  title: 'Chrome/People dock',
  component: PeoplePanel,
  args: {
    folderId: 1,
    trashedCount: 0,
    peopleVersion: 0,
    onClose: fn(),
    notify: fn(),
    onJump: fn(),
  },
} satisfies Meta<typeof PeoplePanel>;

export default meta;
type Story = StoryObj<typeof meta>;

export const IndexingDisabled: Story = {
  beforeEach: () => {
    setEmptyState({
      enabled: false,
      total: 12,
      scanned: 0,
      errors: 0,
      retrying: 0,
      pending: 0,
      engineError: null,
    });
  },
  play: async ({ args, canvas, userEvent }) => {
    await expect(
      await canvas.findByText('Face indexing is off and no face data is stored.'),
    ).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Close' }));
    await expect(args.onClose).toHaveBeenCalledOnce();
  },
};

export const IndexedEmpty: Story = {
  beforeEach: () => {
    setEmptyState({
      enabled: true,
      total: 12,
      scanned: 12,
      errors: 0,
      retrying: 0,
      pending: 0,
      engineError: null,
    });
  },
  play: async ({ canvas }) => {
    await expect(await canvas.findByText('12 of 12 indexed')).toBeVisible();
    await expect(canvas.getByText('No recurring faces found in this folder.')).toBeVisible();
  },
};

export const UnnamedCluster: Story = {
  beforeEach: () => {
    mocked(faceScanStatus).mockResolvedValue({
      enabled: true,
      total: 12,
      scanned: 12,
      errors: 0,
      retrying: 0,
      pending: 0,
      engineError: null,
    });
    mocked(listPersons).mockResolvedValue([]);
    mocked(faceClusters).mockResolvedValue({
      clusters: [
        {
          faceIds: [10, 11],
          chips: [
            { photoId: 'storybook-a', faceIndex: 0, faceId: 10, revision: 1 },
            { photoId: 'storybook-b', faceIndex: 0, faceId: 11, revision: 1 },
          ],
          size: 2,
          photoCount: 2,
        },
      ],
      loose: [],
    });
    mocked(onFacesProgress).mockResolvedValue(() => {});
  },
  play: async ({ args, canvas, userEvent }) => {
    const [chip] = await canvas.findAllByRole('button', { name: 'Show this photo' });
    chip.focus();
    await userEvent.keyboard('{Enter}');
    await expect(args.onJump).toHaveBeenCalledWith('storybook-a');

    const nameInput = canvas.getByPlaceholderText('Who is this?');
    nameInput.focus();
    await userEvent.keyboard('{Escape}');
    await expect(args.onClose).toHaveBeenCalledOnce();
  },
};
