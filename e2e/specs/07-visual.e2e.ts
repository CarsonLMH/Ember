import '@wdio/visual-service';
import { $, browser, expect } from '@wdio/globals';
import { key, waitForFolderOpen } from '../lib/harness.js';

async function setPhotoContentHidden(hidden: boolean): Promise<void> {
  await browser.execute((shouldHide) => {
    for (const selector of ['.viewer-canvas', '.filmstrip']) {
      const element = document.querySelector<HTMLElement>(selector);
      if (element) element.style.visibility = shouldHide ? 'hidden' : '';
    }
  }, hidden);
}

describe('visual regression: stable application chrome', () => {
  before(async () => {
    await waitForFolderOpen();
  });

  it('matches the steady-state culling HUD', async () => {
    await setPhotoContentHidden(true);
    try {
      expect(await browser.checkScreen('culling-shell')).toBe(0);
    } finally {
      await setPhotoContentHidden(false);
    }
  });

  it('matches the keyboard shortcut sheet without its build stamp', async () => {
    await key('?', 1, 0);
    const sheet = $('.cheat-sheet');
    const buildStamp = $('.cheat-footer');
    await sheet.waitForDisplayed();
    await buildStamp.waitForDisplayed();

    await setPhotoContentHidden(true);
    try {
      expect(
        await browser.checkScreen('keyboard-shortcuts', {
          ignore: [buildStamp],
        }),
      ).toBe(0);
    } finally {
      await setPhotoContentHidden(false);
    }

    await key('Escape', 1, 0);
  });
});
