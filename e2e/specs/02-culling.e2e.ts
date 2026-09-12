import { browser, expect, $ } from '@wdio/globals';
import {
  waitForFolderOpen,
  hudPos,
  hudStars,
  key,
  goHome,
  FIXTURE_COUNT,
} from '../lib/harness.js';

async function dispatchChord(keyName: string, code: string, ctrlKey = false): Promise<void> {
  await browser.execute(
    (key, keyCode, ctrl) => {
      window.dispatchEvent(
        new KeyboardEvent('keydown', {
          key,
          code: keyCode,
          ctrlKey: ctrl,
          shiftKey: true,
          bubbles: true,
        }),
      );
    },
    keyName,
    code,
    ctrlKey,
  );
}

async function hudHasChip(label: string): Promise<boolean> {
  return browser.execute((expected) =>
    [...document.querySelectorAll('.hud-chip')].some((el) => el.textContent === expected),
  label);
}

describe('culling loop: rate, clear, trash, restore', () => {
  before(async () => {
    await waitForFolderOpen();
    await goHome();
  });

  it('rates a photo and the verdict is acked in the HUD', async () => {
    await key('3', 1, 0);
    await browser.waitUntil(async () => (await hudStars()) === 3, {
      timeoutMsg: 'rating never acked in the HUD',
    });
    expect((await hudPos()).n).toBe(1);
    expect(await hudStars()).toBe(3);
    await key('ArrowRight');
    expect((await hudPos()).n).toBe(2);
    await key('ArrowLeft');
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
    await $('.dock[aria-label="Trash"]').waitForDisplayed();

    // Docks own the right column but never the culling keyboard. Escape closes
    // one; `i` swaps it for metadata exactly as the shortcut promises.
    await key('Escape', 1, 0);
    await $('.dock').waitForExist({ reverse: true });
    await $('button.hud-trash-btn*=trashed').click();
    await key('i', 1, 0);
    await $('.dock').waitForExist({ reverse: true });
    await $('.exif-panel').waitForDisplayed();
    await key('i', 1, 0);
    await $('.exif-panel').waitForExist({ reverse: true });

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

  it('distinguishes exact and minimum ratings and names every sort mode plainly', async () => {
    await goHome();
    for (const rating of [3, 4, 5]) {
      await key(String(rating), 1, 0);
      await browser.waitUntil(async () => (await hudStars()) === rating);
      if (rating < 5) await key('ArrowRight');
    }

    await dispatchChord('$', 'Digit4');
    await browser.waitUntil(async () => (await hudPos()).m === 1);
    expect(await hudStars()).toBe(4);

    await dispatchChord('$', 'Digit4', true);
    await browser.waitUntil(async () => (await hudPos()).m === 2);
    expect(await hudStars()).toBeGreaterThanOrEqual(4);

    await dispatchChord(')', 'Digit0');
    await browser.waitUntil(async () => (await hudPos()).m === FIXTURE_COUNT);

    await key('s');
    await browser.waitUntil(() => hudHasChip('filename'));
    await key('s');
    await browser.waitUntil(() => hudHasChip('rating'));
    await goHome();
    expect(await hudStars()).toBe(5);

    await dispatchChord('S', 'KeyS');
    await browser.waitUntil(() => hudHasChip('rating ↓'));
    await key('s');
    await browser.waitUntil(() => hudHasChip('date captured ↓'));
    await dispatchChord('S', 'KeyS');
    await browser.waitUntil(async () => !(await hudHasChip('date captured ↓')));

    await goHome();
    for (let i = 0; i < 3; i += 1) {
      await key('0', 1, 0);
      await browser.waitUntil(async () => (await hudStars()) === 0);
      if (i < 2) await key('ArrowRight');
    }
    await goHome();
  });
});
