import { browser, expect, $ } from '@wdio/globals';
import {
  waitForFolderOpen,
  hudPos,
  hudStars,
  key,
  goHome,
  rateAndAdvance,
  FIXTURE_COUNT,
} from '../lib/harness.js';

describe('culling loop: rate, clear, trash, restore', () => {
  before(async () => {
    await waitForFolderOpen();
    await goHome();
  });

  it('rates a photo and the verdict is acked in the HUD', async () => {
    await rateAndAdvance(3);
    expect((await hudPos()).n).toBe(2);
    await key('ArrowLeft');
    expect(await hudStars()).toBe(3);
  });

  it('clears the stars it just set with 0', async () => {
    expect(await hudStars()).toBe(3); // still on the photo rated above
    await key('0', 1, 0);
    await browser.waitUntil(async () => (await hudStars()) === 0, {
      timeoutMsg: 'clearing stars never acked',
    });
  });

  it('trashes the photo with X and restores it from the trash panel', async () => {
    const { m } = await hudPos();
    await key('x', 1, 0);
    await browser.waitUntil(async () => (await hudPos()).m === m - 1, {
      timeoutMsg: 'trash never reflected in HUD count',
    });

    // Cmd+Z would be the keyboard path, but the embedded driver drops the
    // Meta modifier — restore through the session trash panel instead.
    await $('button.hud-trash-btn*=trashed').click();
    const restoreBtn = $('.trash-list li button');
    await restoreBtn.waitForClickable();
    await restoreBtn.click();
    await browser.waitUntil(async () => (await hudPos()).m === m, {
      timeoutMsg: 'trash-panel restore never brought the photo back',
    });
    expect((await hudPos()).m).toBe(FIXTURE_COUNT);
    await $('.dock-close').click(); // close panel
  });
});
