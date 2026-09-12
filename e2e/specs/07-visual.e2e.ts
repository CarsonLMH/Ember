import '@wdio/visual-service';
import { $, browser, expect } from '@wdio/globals';
import { goHome, hudPos, hudStars, key, waitForFolderOpen } from '../lib/harness.js';

async function normalizeVerdicts(): Promise<void> {
  await goHome();
  const { m } = await hudPos();
  for (let i = 0; i < m; i += 1) {
    if ((await hudStars()) > 0) {
      await key('0', 1, 0);
      await browser.waitUntil(async () => (await hudStars()) === 0, {
        timeoutMsg: `clearing stars never acked at visual fixture ${i + 1}`,
      });
    }
    if (i < m - 1) await key('ArrowRight');
  }
  await goHome();
}

async function setPhotoContentHidden(hidden: boolean): Promise<void> {
  await browser.execute((shouldHide) => {
    for (const selector of ['.viewer-canvas', '.filmstrip']) {
      const element = document.querySelector<HTMLElement>(selector);
      if (element) element.style.visibility = shouldHide ? 'hidden' : '';
    }
  }, hidden);
}

async function normalizeChrome(): Promise<void> {
  if (!(await $('.filmstrip').isDisplayed())) {
    await key('t', 1, 0);
    await $('.filmstrip').waitForDisplayed();
  }
  if (!(await $('.exif-panel').isDisplayed())) {
    await key('i', 1, 0);
    await $('.exif-panel').waitForDisplayed();
  }
}

describe('visual regression: stable application chrome', () => {
  before(async () => {
    await waitForFolderOpen();
    // UI preferences live outside the SQLite sandbox and deliberately survive
    // app restarts. Put the visual surface in one explicit state instead of
    // approving whichever filmstrip/panel choices the previous run left.
    await normalizeChrome();
    // Folder-open renders before the background capture-time sort finishes.
    // A snapshot of that transitional "sorting…" note is timing-dependent,
    // so wait on the state marker rather than approving it into the baseline.
    await $('.hud[data-sorted-by-capture="true"]').waitForExist({
      timeout: 15_000,
      timeoutMsg: 'capture-time sort never settled before visual comparison',
    });
    // Earlier durability specs deliberately leave ratings behind. Reset each
    // synthetic photo through the normal journaled UI path so this spec has
    // the same state alone and in the complete suite.
    await normalizeVerdicts();
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
