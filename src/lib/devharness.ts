import { devFlags, facesSpikeStats, frontendLog, quitApp } from './ipc';
import * as perf from './perf';
import * as session from './session';
import * as viewer from './viewer';

/** Forward webview errors to the Rust process's stderr — headless runs are
 * otherwise undebuggable (WKWebView console output never reaches stdout). */
export function installLogForwarding(): void {
  const origError = console.error.bind(console);
  const origWarn = console.warn.bind(console);
  console.error = (...args: unknown[]) => {
    frontendLog('error', args.map(String).join(' '));
    origError(...args);
  };
  console.warn = (...args: unknown[]) => {
    frontendLog('warn', args.map(String).join(' '));
    origWarn(...args);
  };
  window.addEventListener('error', (e) => frontendLog('error', `${e.message} @ ${e.filename}:${e.lineno}`));
  window.addEventListener('unhandledrejection', (e) => frontendLog('error', `unhandled rejection: ${String(e.reason)}`));
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * Env-driven headless modes:
 * - EMBER_OPEN=<dir>            auto-open a folder
 * - EMBER_STORM=1               flip storm → save report → quit
 * - EMBER_CHAOS=1               rate continuously, logging each ack (kill -9 me)
 * - EMBER_VERIFY=1              dump persisted verdicts → quit
 */
export async function runIfRequested(): Promise<void> {
  const flags = await devFlags();
  if (!flags.open) return;
  frontendLog('info', `auto-open: ${flags.open}`);
  await session.openFolder(flags.open);
  const { photos, error } = session.getState();
  frontendLog('info', `scanned ${photos.length} photos${error ? ` (error: ${error})` : ''}`);

  if (flags.verify) {
    // Wait for capture-time sort so cursor position is comparable across runs.
    for (let i = 0; i < 30 && !session.getState().sortedByCapture; i++) await sleep(100);
    const st = session.getState();
    frontendLog('info', `resume-opened cursor=${st.cursor} id=${st.photos[st.cursor]?.id}`);
    for (const p of st.photos) {
      frontendLog('info', `verdict ${p.id} ${p.rating}`);
    }
    frontendLog('info', 'verify-dump-complete');
    await sleep(300);
    await quitApp();
    return;
  }

  if (flags.resumeTest) {
    for (let i = 0; i < 30 && !session.getState().sortedByCapture; i++) await sleep(100);
    for (let i = 0; i < 10; i++) {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
      await sleep(120);
    }
    await sleep(500); // let the fire-and-forget persistView land
    const st = session.getState();
    frontendLog('info', `resume-saved cursor=${st.cursor} id=${st.photos[st.cursor]?.id}`);
    await sleep(200);
    await quitApp();
    return;
  }

  if (flags.chaos) {
    frontendLog('info', 'chaos mode: rating continuously — kill me anytime');
    let i = 0;
    for (;;) {
      const photo = session.getState().photos[session.getState().cursor];
      if (!photo) break;
      const rating = (i % 5) + 1;
      await session.rate(rating, performance.now());
      frontendLog('info', `acked ${photo.id} ${rating}`);
      i += 1;
      await sleep(15);
    }
    return;
  }

  if (flags.zoomTest) {
    for (let i = 0; i < 30 && !session.getState().sortedByCapture; i++) await sleep(100);
    await sleep(3000); // let previews near the cursor land
    const press = (key: string) =>
      window.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
    const isBlank = (): boolean => {
      const c = document.querySelector('canvas');
      const g = c?.getContext('2d');
      if (!c || !g) return true;
      // Sample a cross of points; all ≈ background = nothing drawn.
      const pts = [
        [c.width / 2, c.height / 2],
        [c.width / 4, c.height / 2],
        [(3 * c.width) / 4, c.height / 2],
        [c.width / 2, c.height / 4],
        [c.width / 2, (3 * c.height) / 4],
      ];
      return pts.every(([x, y]) => {
        const d = g.getImageData(Math.floor(x), Math.floor(y), 1, 1).data;
        return Math.abs(d[0] - 20) < 6 && Math.abs(d[1] - 20) < 6 && Math.abs(d[2] - 20) < 6;
      });
    };
    let failures = 0;
    const check = async (label: string, settleMs: number) => {
      await sleep(settleMs);
      const blank = isBlank();
      if (blank) failures += 1;
      frontendLog('info', `zoomtest ${label}: ${blank ? 'BLANK' : 'ok'}`);
    };
    await check('baseline-fit', 300);
    press('z'); // zoom in
    await check('z-zoom', 700);
    press('z'); // back to fit
    await check('z-fit', 400);
    press('z'); // the reported triple-press case
    await check('z-zoom-again', 700);
    press('ArrowRight'); // locked-zoom flip
    await check('zoomed-flip-1', 500);
    press('ArrowRight');
    await check('zoomed-flip-2', 700);
    press('z');
    await check('back-to-fit', 400);
    press('f'); // AF-point zoom
    await check('af-zoom', 1200);
    // Synthetic trackpad pinch through the real gesture handlers.
    const canvas = document.querySelector('canvas');
    if (canvas) {
      const gesture = (type: string, scale: number) => {
        const evt = new Event(type, { cancelable: true }) as Event & {
          scale?: number;
          clientX?: number;
          clientY?: number;
        };
        evt.scale = scale;
        const r = canvas.getBoundingClientRect();
        evt.clientX = r.left + r.width / 2;
        evt.clientY = r.top + r.height / 2;
        canvas.dispatchEvent(evt);
      };
      gesture('gesturestart', 1);
      for (let s = 1.1; s <= 2.6; s += 0.15) {
        gesture('gesturechange', s);
        await sleep(16);
      }
      gesture('gestureend', 2.6);
      await check('pinch-in', 500);
      const zoomedIn = viewer.isZoomed();
      frontendLog('info', `zoomtest pinch-in zoomed: ${zoomedIn ? 'ok' : 'FAIL'}`);
      if (!zoomedIn) failures += 1;
      // Pinch is relative: on full-res photos (100% ≈ 2.6× fit) one gesture
      // can't reach fit — assert repeated pinch-outs get there, blank-free.
      for (let round = 1; round <= 3 && viewer.isZoomed(); round++) {
        gesture('gesturestart', 1);
        for (let s = 0.95; s >= 0.3; s -= 0.1) {
          gesture('gesturechange', s);
          await sleep(16);
        }
        gesture('gestureend', 0.3);
        await check(`pinch-out-${round}`, 500);
      }
      const backToFit = !viewer.isZoomed();
      frontendLog('info', `zoomtest pinch-out fit: ${backToFit ? 'ok' : 'FAIL'}`);
      if (!backToFit) failures += 1;
    }
    frontendLog('info', `zoomtest done: ${failures === 0 ? 'PASS' : `FAIL (${failures} blanks)`}`);
    await sleep(300);
    await quitApp();
    return;
  }

  if (flags.storm) {
    frontendLog('info', 'waiting 30s for preview sweep before storm');
    await sleep(30_000);
    const pre = session.getState();
    frontendLog('info', `pre-storm cursor=${pre.cursor}/${pre.photos.length}`);
    // Faces gate runs (EMBER_FACES_FORCE=1): snapshot spike counters around
    // the measured window — the storm only counts if inference was ACTIVE
    // during it, never assumed (review-1 gate integrity).
    const spikeBefore = flags.facesForce ? await facesSpikeStats().catch(() => null) : null;
    const report = await perf.flipStorm(300, 80, (r) => session.rate(r, performance.now()));
    const post = session.getState();
    frontendLog('info', `post-storm cursor=${post.cursor}/${post.photos.length}`);
    let spikeTag = '';
    if (flags.facesForce) {
      const s0 = spikeBefore;
      const s1 = await facesSpikeStats().catch(() => null);
      const active = !!(s0 && s1 && s1.photos > s0.photos);
      spikeTag = ` facesSpike=${active ? 'ACTIVE' : 'NOT-ACTIVE'}`;
      frontendLog(
        'info',
        `faces-spike during storm: photos ${s0?.photos ?? '?'}→${s1?.photos ?? '?'} ` +
          `passes=${s1?.passes ?? '?'} avg=${s1?.avgTotalMs.toFixed(1) ?? '?'}ms ` +
          `(decode=${s1?.avgDecodeMs.toFixed(1) ?? '?'} detect=${s1?.avgDetectMs.toFixed(1) ?? '?'} ` +
          `embed=${s1?.avgEmbedMs.toFixed(1) ?? '?'}) faces=${s1?.faces ?? '?'} ` +
          `errors=${s1?.errors ?? '?'} rss=${s1?.rssMb ?? '?'}MB`,
      );
    }
    const ack = report.ackSamples
      ? ` ack(n=${report.ackSamples} p50=${report.ackP50?.toFixed(1)} p99=${report.ackP99?.toFixed(1)} max=${report.ackMax?.toFixed(1)})`
      : '';
    frontendLog(
      'info',
      `storm done: p50=${report.p50.toFixed(1)} p99=${report.p99.toFixed(1)} max=${report.max.toFixed(1)} misses=${report.missServes}/${report.flips} coldOpen=${report.coldOpenMs?.toFixed(0)}ms${ack}${spikeTag}`,
    );
    await sleep(500);
    await quitApp();
  }
}
