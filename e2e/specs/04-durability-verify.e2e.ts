import { expect } from '@wdio/globals';
import {
  waitForFolderOpen,
  hudStars,
  key,
  goHome,
  ratingFor,
  FIXTURE_COUNT,
} from '../lib/harness.js';

// Part 2: a brand-new app instance (fresh wdio worker) replays the journal.
// Every rating acked before the kill -9 in part 1 must be here.
describe('durability: journal replay after the crash', () => {
  it('shows every verdict recorded before the kill', async () => {
    await waitForFolderOpen();
    await goHome();
    for (let i = 0; i < FIXTURE_COUNT; i += 1) {
      expect(await hudStars()).toBe(ratingFor(i));
      if (i < FIXTURE_COUNT - 1) await key('ArrowRight');
    }
  });
});
