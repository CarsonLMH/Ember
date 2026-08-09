import { expect } from '@wdio/globals';
import {
  waitForFolderOpen,
  hudPos,
  hudStars,
  key,
  goHome,
  rateAndAdvance,
  killAppHard,
  ratingFor,
  FIXTURE_COUNT,
} from '../lib/harness.js';

// Part 1 of the crash-durability pair: rate everything, then kill -9 the app
// with zero warning. 04-durability-verify boots a fresh instance and asserts
// every verdict survived. The journal contract (write precedes UI ack) is
// exactly what makes the HUD assertions here sufficient evidence.
describe('durability: verdicts acked before an unannounced kill -9', () => {
  it('rates every photo through real keys', async () => {
    await waitForFolderOpen();
    await goHome();
    for (let i = 0; i < FIXTURE_COUNT; i += 1) {
      const rating = ratingFor(i);
      if (rating === 0) {
        await key('ArrowRight'); // photo stays B-roll; just move on
      } else {
        await rateAndAdvance(rating);
      }
    }
  });

  it('re-reads the full verdict map in-session', async () => {
    await goHome();
    for (let i = 0; i < FIXTURE_COUNT; i += 1) {
      expect(await hudStars()).toBe(ratingFor(i));
      if (i < FIXTURE_COUNT - 1) await key('ArrowRight');
    }
    expect((await hudPos()).n).toBe(FIXTURE_COUNT);
  });

  it('dies without warning', async () => {
    killAppHard();
  });
});
