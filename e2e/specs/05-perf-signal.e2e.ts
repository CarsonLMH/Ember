import { readdirSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { browser, expect, $ } from '@wdio/globals';
import { waitForFolderOpen, hudPos, key, perfRow, STORM_FIXTURE_COUNT } from '../lib/harness.js';

interface PerfReport {
  kind: string;
  flips: number;
  p99: number;
  max: number;
  missServes: number;
  coldOpenMs: number | null;
}

const reportsDir = path.join(
  homedir(),
  'Library',
  'Application Support',
  'com.cleung.ember.e2e',
  'perf-reports',
);

// The perf tripwire (npm run e2e:perf — requires a QUIET machine).
//
// This spec does NOT own the ≤50ms product budget — gate.sh does. The e2e
// binary is instrumented (wdio invoke interception, embedded WebDriver
// server, HUD open during the storm), and that instrumentation alone costs
// tens of ms of tail latency: calibration on 2026-08-08 measured the same
// storm/fixtures at p99=2ms on a clean debug build vs ~61ms on the e2e
// build. The 150ms line catches order-of-magnitude regressions (a broken
// cache path measures 500ms+) without pretending to measure the budget.
//
// The numbers come from the app's own 300-flip storm (the HUD button), not
// WebDriver-paced flips — the service executes JS in the app's main thread
// before every command, so we click once, wait without polling, and read
// the saved report covering exactly the storm's measured slice.
describe('perf tripwire: flip-storm on the instrumented e2e build', () => {
  it('stays inside the instrumented tripwire with zero miss-serves', async () => {
    await waitForFolderOpen();
    expect((await hudPos()).m).toBe(STORM_FIXTURE_COUNT);
    // Let the preview sweep cover the folder so we measure steady state.
    await browser.pause(12_000);

    // Real-key warm-up segment — the human event-queue path stays exercised.
    await key('ArrowRight', 40, 70);

    await key('`', 1, 300);
    await $('.perf-hud').waitForDisplayed();
    await $('.perf-hud button').click();
    // Storm ≈ 1.5s settle + 300 × 80ms. No WebDriver traffic while it runs.
    await browser.pause(28_000);
    await browser.waitUntil(
      async () => (await $('.perf-hud button').getText()) !== 'storming…',
      { timeout: 30_000, timeoutMsg: 'storm never finished' },
    );

    const newest = readdirSync(reportsDir)
      .filter((f) => f.startsWith('report-'))
      .sort()
      .at(-1);
    if (!newest) throw new Error(`no perf report written to ${reportsDir}`);
    const report = JSON.parse(readFileSync(path.join(reportsDir, newest), 'utf8')) as PerfReport;

    expect(report.flips).toBeGreaterThanOrEqual(299);
    expect(report.missServes).toBe(0);
    expect(report.p99).toBeLessThanOrEqual(150);
    if (report.coldOpenMs !== null) expect(report.coldOpenMs).toBeLessThanOrEqual(1000);

    // Cumulative HUD misses cover the real-key warm-up segment too.
    expect(parseInt(await perfRow('misses'), 10)).toBe(0);
  });
});
