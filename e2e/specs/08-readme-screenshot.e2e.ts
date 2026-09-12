import { execFileSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import path from 'node:path';
import { $, browser } from '@wdio/globals';
import { hudStars, key, rootDir, waitForFolderOpen } from '../lib/harness.js';

describe('public README screenshot', () => {
  it('captures the real app around a privacy-safe fixture', async () => {
    await waitForFolderOpen();
    await $('.hud[data-sorted-by-capture="true"]').waitForExist({
      timeout: 15_000,
      timeoutMsg: 'capture-time sort never settled before the README screenshot',
    });

    if (!(await $('.filmstrip').isDisplayed())) {
      await key('t', 1, 0);
      await $('.filmstrip').waitForDisplayed();
    }
    if (!(await $('.exif-panel').isDisplayed())) {
      await key('i', 1, 0);
      await $('.exif-panel').waitForDisplayed();
    }

    await key('4', 1, 0);
    await browser.waitUntil(async () => (await hudStars()) === 4, {
      timeoutMsg: 'demo rating never reached the journal-backed UI',
    });
    await browser.waitUntil(
      () =>
        browser.execute(() => {
          const canvas = document.querySelector<HTMLCanvasElement>('.viewer-canvas');
          const context = canvas?.getContext('2d');
          if (!canvas || !context || canvas.width === 0 || canvas.height === 0) return false;
          const pixel = context.getImageData(
            Math.floor(canvas.width / 2),
            Math.floor(canvas.height / 2),
            1,
            1,
          ).data;
          return pixel[3] > 0 && Math.max(pixel[0], pixel[1], pixel[2]) > 30;
        }),
      { timeout: 15_000, timeoutMsg: 'demo photograph never reached the canvas' },
    );

    const rawDir = path.join(rootDir, 'e2e', '.visual-output');
    const outputDir = path.join(rootDir, 'docs', 'assets');
    const raw = path.join(rawDir, 'readme-culling.png');
    mkdirSync(rawDir, { recursive: true });
    mkdirSync(outputDir, { recursive: true });
    await browser.saveScreenshot(raw);
    execFileSync(
      'sips',
      [
        '--resampleHeightWidth',
        '900',
        '1440',
        '--setProperty',
        'format',
        'jpeg',
        '--setProperty',
        'formatOptions',
        '90',
        raw,
        '--out',
        path.join(outputDir, 'ember-culling.jpg'),
      ],
      { stdio: 'pipe' },
    );
  });
});
