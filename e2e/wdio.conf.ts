import { execFileSync } from 'node:child_process';
import { existsSync, rmSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { browser } from '@wdio/globals';
import type { TauriCapabilities } from '@wdio/tauri-service';
import {
  appBinary,
  fixturesDir,
  rootDir,
  readmeFixturesDir,
  stormFixturesDir,
  EMBEDDED_PORT,
  FIXTURE_COUNT,
  STORM_FIXTURE_COUNT,
} from './lib/harness.js';

// EMBER_E2E_PERF=1 (npm run e2e:perf) switches to the perf gate: only the
// 05 spec, against the big storm fixture set, on a machine that must be quiet.
const PERF = process.env.EMBER_E2E_PERF === '1';
const README = process.env.EMBER_E2E_README === '1';
const VISIBLE = !README && (PERF || process.env.EMBER_E2E_VISIBLE === '1');
const UPDATE_VISUAL_BASELINES = process.env.EMBER_UPDATE_VISUAL_BASELINES === '1';

// visual-service v9 checks argv inside each worker, while WDIO does not pass
// unknown CLI flags through to those workers. The explicit npm update command
// carries an env marker, and every process reconstructs the supported flag.
if (UPDATE_VISUAL_BASELINES && !process.argv.includes('--update-visual-baseline')) {
  process.argv.push('--update-visual-baseline');
}

// The spawned app inherits this process's environment; EMBER_OPEN makes every
// session (including durability relaunches) open the fixture folder on boot.
process.env.EMBER_OPEN = PERF
  ? stormFixturesDir
  : README
    ? readmeFixturesDir
    : fixturesDir;
// Functional and accessibility runs must never activate or flash a native
// window over the user's desktop. Visual and glass-time perf checks are the
// explicit exceptions and have separate commands/warnings.
process.env.EMBER_HIDDEN = VISIBLE ? '0' : '1';

// e2e sandbox (identifier com.cleung.ember.e2e) — wiped per run so cold-open,
// adoption, and journal state are reproducible. Never touches com.cleung.ember.
const sandboxDirs = [
  path.join(homedir(), 'Library', 'Application Support', 'com.cleung.ember.e2e'),
  path.join(homedir(), 'Library', 'Caches', 'com.cleung.ember.e2e'),
];

export const config: WebdriverIO.Config = {
  runner: 'local',
  specs: README
    ? ['./specs/08-readme-screenshot.e2e.ts']
    : PERF
      ? ['./specs/05-perf-signal.e2e.ts']
      : VISIBLE
        ? [
            './specs/0[1-4]-*.e2e.ts',
            './specs/06-accessibility.e2e.ts',
            './specs/07-visual.e2e.ts',
          ]
        : ['./specs/0[1-4]-*.e2e.ts', './specs/06-accessibility.e2e.ts'],
  maxInstances: 1,
  capabilities: [
    {
      browserName: 'tauri',
      'tauri:options': { application: appBinary },
    } satisfies TauriCapabilities,
    // satisfies checks the shape; the cast bridges the vendor-extension key
    // that the stock W3C capability union doesn't know about.
  ] as unknown as WebdriverIO.Config['capabilities'],
  services: [
    [
      '@wdio/tauri-service',
      {
        appBinaryPath: appBinary,
        driverProvider: 'embedded',
        embeddedPort: EMBEDDED_PORT,
        env: { EMBER_HIDDEN: VISIBLE ? '0' : '1' },
        captureBackendLogs: true,
        captureFrontendLogs: true,
        startTimeout: 90_000,
      },
    ],
    ...(PERF
      ? []
      : [
          [
            '@wdio/visual-service',
            {
              baselineFolder: path.join(rootDir, 'e2e', 'visual-baselines'),
              screenshotPath: path.join(rootDir, 'e2e', '.visual-output'),
              formatImageName: '{tag}-{browserName}-{width}x{height}',
              autoSaveBaseline: UPDATE_VISUAL_BASELINES,
              alwaysSaveActualImage: true,
              disableCSSAnimation: true,
              waitForFontsLoaded: true,
              compareOptions: { ignoreAntialiasing: true },
            },
          ],
        ]),
  ] as WebdriverIO.Config['services'],
  framework: 'mocha',
  mochaOpts: { ui: 'bdd', timeout: 240_000 },
  reporters: ['spec'],
  // 'error' not 'warn': after the durability spec's deliberate kill -9, the
  // service WARN-spams window-state retries against the dead app.
  logLevel: 'error',
  waitforTimeout: 15_000,
  connectionRetryCount: 2,

  // The durability spec kill -9s the app, taking the embedded WebDriver
  // server with it. The runner's teardown DELETE then gets ECONNREFUSED and
  // would fail the whole spec file — swallow exactly that case.
  before: async () => {
    // overwriteCommand's typed name union omits deleteSession, but the
    // runtime accepts any protocol command — hence the cast.
    const overwrite = browser.overwriteCommand as unknown as (
      name: string,
      fn: (orig: (...a: unknown[]) => Promise<unknown>, ...args: unknown[]) => Promise<unknown>,
    ) => Promise<void>;
    await overwrite('deleteSession', async (orig, ...args) => {
      try {
        return await orig(...args);
      } catch (err) {
        if (String(err).includes('ECONNREFUSED')) return undefined;
        throw err;
      }
    });
  },

  onPrepare: () => {
    if (!existsSync(appBinary)) {
      throw new Error(`e2e binary missing at ${appBinary} — run \`npm run e2e:build\` first`);
    }
    for (const dir of sandboxDirs) rmSync(dir, { recursive: true, force: true });
    const gen = path.join(rootDir, 'scripts', 'e2e-fixtures.mjs');
    if (README) {
      const readmeGen = path.join(rootDir, 'scripts', 'readme-fixtures.mjs');
      execFileSync('node', [readmeGen], { stdio: 'inherit' });
    } else if (PERF) {
      // Storm fixtures are never rated → XMP-clean → safe to reuse across runs.
      execFileSync('node', [gen, stormFixturesDir, String(STORM_FIXTURE_COUNT), '--if-missing'], {
        stdio: 'inherit',
      });
    } else {
      // Functional fixtures get verdicts embedded by the XMP queue — always rebuild.
      execFileSync('node', [gen, fixturesDir, String(FIXTURE_COUNT)], { stdio: 'inherit' });
    }
  },
};
