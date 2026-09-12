import { browser, expect, $ } from '@wdio/globals';
import {
  waitForFolderOpen,
  hudPos,
  hudName,
  key,
  goHome,
  goEnd,
  FIXTURE_COUNT,
} from '../lib/harness.js';

async function immersionKey(repeat = false): Promise<void> {
  await browser.execute((isRepeat) => {
    window.dispatchEvent(
      new KeyboardEvent('keydown', {
        key: 'T',
        code: 'KeyT',
        shiftKey: true,
        repeat: isRepeat,
        bubbles: true,
      }),
    );
  }, repeat);
}

async function shiftedDigit(digit: number): Promise<void> {
  await browser.execute((value) => {
    window.dispatchEvent(
      new KeyboardEvent('keydown', {
        key: value === 0 ? ')' : '!@#$%'[value - 1],
        code: `Digit${value}`,
        shiftKey: true,
        bubbles: true,
      }),
    );
  }, digit);
}

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

  it('mounts the filmstrip at the current photo with its thumbnail ready', async () => {
    await key('t');
    await $('.filmstrip').waitForExist({ reverse: true });
    await goEnd();
    await key('t');

    const current = $('.strip-current');
    await current.waitForDisplayed();
    const thumb = current.$('img.strip-thumb');
    await browser.waitUntil(async () => Number(await thumb.getProperty('naturalWidth')) > 0, {
      timeoutMsg: 'current filmstrip thumbnail never became ready',
    });
    expect(
      await browser.execute(() => {
        const strip = document.querySelector('.filmstrip')?.getBoundingClientRect();
        const row = document.querySelector('.strip-current')?.getBoundingClientRect();
        return Boolean(strip && row && row.top >= strip.top && row.bottom <= strip.bottom);
      }),
    ).toBe(true);
    await goHome();
  });

  it('hides all chrome for immersion and restores the prior layout with Escape', async () => {
    const exifWasVisible = await $('.exif-panel').isDisplayed();
    if (!exifWasVisible) await key('i');
    await $('.exif-panel').waitForDisplayed();
    const widthBefore = (await $('.viewer-canvas').getSize()).width;

    await immersionKey();
    expect(await $('.app').getAttribute('data-immersive')).toBe('true');
    expect(await $('.filmstrip').isDisplayed()).toBe(false);
    expect(await $('.hud').isDisplayed()).toBe(false);
    expect(await $('.hud-right').isDisplayed()).toBe(false);
    expect(await $('.exif-panel').isDisplayed()).toBe(false);
    expect((await $('.viewer-canvas').getSize()).width).toBeGreaterThan(widthBefore);

    await key('?');
    await $('.cheat-backdrop').waitForDisplayed();
    await key('Escape');
    await $('.cheat-backdrop').waitForExist({ reverse: true });
    await browser.execute(() => {
      window.dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Escape', repeat: true, bubbles: true }),
      );
    });
    expect(await $('.app').getAttribute('data-immersive')).toBe('true');

    await key('3');
    await browser.waitUntil(async () =>
      browser.execute(() => document.querySelector('.hud-stars')?.textContent === '★★★'),
    );
    await key('0');

    // Leaving must still work when a filter makes the visible set empty.
    await shiftedDigit(5);
    await browser.waitUntil(async () => (await $('.overlay-msg').getText()).includes('No photos match'));
    await immersionKey();
    expect(await $('.app').getAttribute('data-immersive')).toBe('false');
    await shiftedDigit(0);
    await $('.exif-panel').waitForDisplayed();

    await immersionKey();
    await key('ArrowRight');
    await key('Escape');
    expect(await $('.app').getAttribute('data-immersive')).toBe('false');
    await $('.filmstrip').waitForDisplayed();
    await $('.exif-panel').waitForDisplayed();
    expect((await hudPos()).n).toBe(2);

    if (!exifWasVisible) await key('i');
    await goHome();
  });

  it('keeps native Tab focus traversal and preserves 100% zoom through immersion', async () => {
    await $('.filmstrip').click();
    await key('Tab');
    expect(await $('.app').getAttribute('data-immersive')).toBe('false');

    const exifWasVisible = await $('.exif-panel').isDisplayed();
    if (!exifWasVisible) await key('i');
    await $('.exif-panel').waitForDisplayed();
    const canvasWidthBefore = Number(await $('.viewer-canvas').getProperty('width'));
    await key('z');
    const zoomText = async () =>
      browser.execute(() =>
        [...document.querySelectorAll('.hud-chip')]
          .map((el) => el.textContent ?? '')
          .find((text) => /^\d+%$/.test(text)) ?? '',
      );
    await browser.waitUntil(async () => (await zoomText()) === '100%');

    await immersionKey();
    await browser.waitUntil(
      async () => Number(await $('.viewer-canvas').getProperty('width')) > canvasWidthBefore,
      { timeoutMsg: 'canvas did not expand for immersion' },
    );
    const hidden = await browser.execute(() => document.hidden);
    if (!hidden) {
      // Glass-time geometry needs a presenting webview. Hidden functional
      // runs intentionally skip this paint assertion because WebKit suspends
      // requestAnimationFrame even while DOM and protocol work stay live.
      await browser.executeAsync((done) =>
        requestAnimationFrame(() => requestAnimationFrame(() => done())),
      );
      expect(await zoomText()).toBe('100%');
    }
    await key('Escape');
    expect(await $('.app').getAttribute('data-immersive')).toBe('false');
    expect(await zoomText()).toBe('100%');
    await key('z');
    if (!exifWasVisible) await key('i');
  });
});
