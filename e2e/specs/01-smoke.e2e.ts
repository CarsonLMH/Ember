import { expect } from '@wdio/globals';
import {
  waitForFolderOpen,
  hudPos,
  hudName,
  key,
  goHome,
  goEnd,
  FIXTURE_COUNT,
} from '../lib/harness.js';

describe('smoke: launch and folder open', () => {
  it('opens the fixture folder and lands on the first photo in capture order', async () => {
    await waitForFolderOpen();
    expect(await hudPos()).toEqual({ n: 1, m: FIXTURE_COUNT });
    expect(await hudName()).toBe('IMG_0001');
  });

  it('navigates with real arrow keys and stops at both boundaries', async () => {
    await key('ArrowRight', 3);
    expect((await hudPos()).n).toBe(4);
    await key('ArrowLeft');
    expect((await hudPos()).n).toBe(3);
    await goEnd();
    expect(await hudName()).toBe(`IMG_${String(FIXTURE_COUNT).padStart(4, '0')}`);
    await goHome();
    expect(await hudName()).toBe('IMG_0001');
  });
});
