import { execSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { browser, $, $$ } from '@wdio/globals';

const e2eDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const rootDir = path.resolve(e2eDir, '..');
export const appBinary = path.join(rootDir, 'src-tauri', 'target', 'debug', 'ember');
export const fixturesDir =
  process.env.EMBER_E2E_FIXTURES ?? path.join(e2eDir, 'fixtures', 'basic');
export const FIXTURE_COUNT = 12;
export const readmeFixturesDir = path.join(e2eDir, 'fixtures', 'readme');

// Perf mode uses a big, never-rated (so XMP-clean, cacheable) fixture set:
// p99 over ~100 flips is just the worst sample; 250 flips over 320 photos is
// a statistic.
export const stormFixturesDir = path.join(e2eDir, 'fixtures', 'storm');
export const STORM_FIXTURE_COUNT = 320;

// Distinct from the 4445 default so a stray tool on the default port can never
// be mistaken for (or killed as) the app under test.
export const EMBEDDED_PORT = 4455;

/** W3C WebDriver code for the Meta/Command key. */
export const META = '\uE03D';

/** Deterministic rating map shared by the durability kill/verify spec pair. */
export const ratingFor = (index: number): number => index % 6;

/** Folder is open and the keymap gate has passed once the HUD renders. */
export async function waitForFolderOpen(): Promise<void> {
  await $('.hud-pos').waitForDisplayed({ timeout: 30_000 });
}

/** Parses the "N / M" HUD position chip. */
export async function hudPos(): Promise<{ n: number; m: number }> {
  const text = await $('.hud-pos').getText();
  const m = text.match(/(\d+)\s*\/\s*(\d+)/);
  if (!m) throw new Error(`unparseable .hud-pos text: "${text}"`);
  return { n: Number(m[1]), m: Number(m[2]) };
}

/** Star count for the current photo — '★'.repeat(rating), '·' when unrated. */
export async function hudStars(): Promise<number> {
  const text = await $('.hud-stars').getText();
  return text === '·' ? 0 : text.length;
}

export async function hudName(): Promise<string> {
  return $('.hud-name').getText();
}

/** Real key injection through the WebDriver actions pipeline. */
export async function key(k: string | string[], times = 1, pauseMs = 60): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await browser.keys(k);
    if (pauseMs > 0) await browser.pause(pauseMs);
  }
}

/**
 * The embedded WebDriver's key synthesis translates arrows, characters, and
 * digits, but drops Home/End and the Meta modifier — so cursor resets are
 * arrow-spam (boundary presses are no-ops) and specs must not use chords.
 */
export async function goHome(): Promise<void> {
  const { m } = await hudPos();
  await key('ArrowLeft', m + 1, 30);
  const { n } = await hudPos();
  if (n !== 1) throw new Error(`goHome landed at ${n}`);
}

export async function goEnd(): Promise<void> {
  const { m } = await hudPos();
  await key('ArrowRight', m + 1, 30);
  const { n } = await hudPos();
  if (n !== m) throw new Error(`goEnd landed at ${n}/${m}`);
}

/**
 * Rates the current photo and lands deterministically on the next one,
 * whether or not the rating auto-advanced. Waits for the journal-acked UI
 * change rather than sleeping. Position is read fresh — sessions resume the
 * previous instance's cursor, so callers can't assume where they are.
 */
export async function rateAndAdvance(rating: number): Promise<void> {
  const before = await hudPos();
  await key(String(rating), 1, 0);
  await browser.waitUntil(
    async () => {
      const { n } = await hudPos();
      if (n === before.n + 1) return true; // auto-advanced
      return n === before.n && (await hudStars()) === rating; // acked in place
    },
    { timeout: 10_000, timeoutMsg: `rating ${rating} never acked at pos ${before.n}` },
  );
  if ((await hudPos()).n === before.n && before.n < before.m) await key('ArrowRight');
}

/**
 * kill -9 on the app under test, located by its embedded WebDriver port —
 * never by binary path, which a live `tauri dev` session would share.
 *
 * The app's `exiftool -stay_open` children deliberately ignore stdin EOF, so
 * a hard kill orphans them to launchd. Capture their PIDs first and reap them
 * by exact PID afterwards — never by pattern, which would match the workers
 * of a live user instance.
 */
export function killAppHard(): void {
  // -sTCP:LISTEN: only the app's server socket — a bare port match would also
  // catch the wdio runner's client connections.
  const appPid = execSync(`lsof -ti tcp:${EMBEDDED_PORT} -sTCP:LISTEN`, {
    encoding: 'utf8',
  }).trim();
  if (!/^\d+$/.test(appPid)) throw new Error(`no single app pid on :${EMBEDDED_PORT}: "${appPid}"`);
  let children: string[] = [];
  try {
    children = execSync(`pgrep -P ${appPid} -f exiftool`, { encoding: 'utf8' })
      .trim()
      .split('\n')
      .filter((p) => /^\d+$/.test(p));
  } catch {
    // pgrep exits 1 when the app spawned no exiftool workers yet — fine.
  }
  execSync(`kill -9 ${appPid}`);
  for (const pid of children) {
    try {
      const comm = execSync(`ps -p ${pid} -o command=`, { encoding: 'utf8' });
      if (comm.includes('exiftool')) execSync(`kill -9 ${pid}`);
    } catch {
      // already gone with its parent — the good case.
    }
  }
}

/** Reads a value from the PerfHud table by row label, e.g. perfRow('p99') → "12.3 ms". */
export async function perfRow(label: string): Promise<string> {
  for (const row of await $$('.perf-hud tr').getElements()) {
    const cells = await row.$$('td').getElements();
    if (cells.length === 2 && (await cells[0].getText()) === label) {
      return cells[1].getText();
    }
  }
  throw new Error(`no PerfHud row labeled "${label}"`);
}
