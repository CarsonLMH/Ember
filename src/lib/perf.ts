import type { FlipSample, PerfReport, ServedFrom } from './types';
import { focusWindow, savePerfReport } from './ipc';

const RING_MAX = 2000;

const ring: FlipSample[] = [];
let coldOpenMs: number | null = null;
let listeners: Array<() => void> = [];

function notify() {
  for (const l of listeners) l();
}

export function subscribe(fn: () => void): () => void {
  listeners.push(fn);
  return () => {
    listeners = listeners.filter((l) => l !== fn);
  };
}

let occludedSamples = 0;

/**
 * Record one flip. `start` is the keydown's event.timeStamp (captures event-queue
 * latency).
 *
 * The sample is recorded synchronously at draw-complete, then UPGRADED to
 * glass time (double requestAnimationFrame ≈ frame commit) when the rAF fires.
 * WKWebView throttles rAF to ~1Hz for occluded windows — recording inside the
 * rAF would silently drop most samples and report multi-second latencies for
 * instant renders. Occluded samples keep their draw-complete time and are
 * counted separately.
 */
export function recordFlip(start: number, servedFrom: ServedFrom, photoId: string): void {
  const sample: FlipSample = {
    at: performance.now(),
    latencyMs: performance.now() - start,
    servedFrom,
    photoId,
  };
  ring.push(sample);
  if (ring.length > RING_MAX) ring.shift();
  notify();
  const drawDoneMs = sample.latencyMs;
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      const glassMs = performance.now() - start;
      if (!document.hidden && glassMs - drawDoneMs <= 1000) {
        sample.latencyMs = glassMs;
      } else {
        occludedSamples += 1;
      }
      notify();
    });
  });
}

export function markColdOpen(ms: number): void {
  coldOpenMs = ms;
  notify();
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[Math.max(0, idx)];
}

export interface PerfStats {
  flips: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
  missServes: number;
  occluded: number;
  coldOpenMs: number | null;
}

export function stats(lastN?: number): PerfStats {
  const samples = lastN ? ring.slice(-lastN) : ring;
  const lat = samples.map((s) => s.latencyMs).sort((a, b) => a - b);
  return {
    flips: samples.length,
    p50: percentile(lat, 50),
    p95: percentile(lat, 95),
    p99: percentile(lat, 99),
    max: lat.length ? lat[lat.length - 1] : 0,
    missServes: samples.filter((s) => s.servedFrom !== 'bitmap-cache').length,
    occluded: occludedSamples,
    coldOpenMs,
  };
}

export function reset(): void {
  ring.length = 0;
  occludedSamples = 0;
  notify();
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** MessageChannel macrotask yield — unlike setTimeout, NOT throttled to ~1s in
 * hidden windows, so the harness storm keeps its real pacing when the window
 * is invisible (the whole point of hidden gate runs). */
function macroYield(): Promise<void> {
  return new Promise((resolve) => {
    const ch = new MessageChannel();
    ch.port1.onmessage = () => resolve();
    ch.port2.postMessage(0);
  });
}

async function pacedWait(ms: number): Promise<void> {
  const end = performance.now() + ms;
  while (performance.now() < end) await macroYield();
}

/**
 * Flip-storm harness: dispatches real ArrowRight keydowns through the actual
 * event path, then reports on exactly those flips. This is the slice-1 exit
 * criterion: p99 ≤ 50ms AND zero non-cache serves in steady state.
 */
export async function flipStorm(
  flips = 300,
  intervalMs = 80,
  rateFn?: (rating: number) => Promise<void>,
): Promise<PerfReport> {
  // Glass-time measurement needs an unoccluded window (rAF throttles otherwise).
  await focusWindow().catch(() => {});
  // Start from the top so a resumed cursor can't run the storm off the end,
  // then give the preloader a beat to re-anchor.
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
  await sleep(1500);
  const before = ring.length;
  const acks: number[] = [];
  for (let i = 0; i < flips; i++) {
    window.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }),
    );
    // Every 10th flip also rates the current photo: measures the journal-ack
    // round trip (rating → fsync'd verdict) under the same storm load. The
    // rating advance renders via showCurrent(null), so flip stats stay pure.
    if (rateFn && i % 10 === 9) {
      const t0 = performance.now();
      await rateFn((i % 5) + 1);
      acks.push(performance.now() - t0);
    }
    await pacedWait(intervalMs);
  }
  // Let trailing glass-time upgrades land (samples themselves are already in).
  await pacedWait(300);
  const measured = ring.slice(before);
  const lat = measured.map((s) => s.latencyMs).sort((a, b) => a - b);
  const ackSorted = [...acks].sort((a, b) => a - b);
  const report: PerfReport = {
    kind: 'flip-storm',
    flips: measured.length,
    p50: percentile(lat, 50),
    p95: percentile(lat, 95),
    p99: percentile(lat, 99),
    max: lat.length ? lat[lat.length - 1] : 0,
    missServes: measured.filter((s) => s.servedFrom !== 'bitmap-cache').length,
    coldOpenMs,
    generatedAt: new Date().toISOString(),
    ...(acks.length
      ? {
          ackSamples: acks.length,
          ackP50: percentile(ackSorted, 50),
          ackP99: percentile(ackSorted, 99),
          ackMax: ackSorted[ackSorted.length - 1],
        }
      : {}),
  };
  try {
    const path = await savePerfReport(report);
    console.info(`flip-storm report saved: ${path}`, report);
  } catch (e) {
    console.warn('flip-storm report not saved', e, report);
  }
  return report;
}
