#!/usr/bin/env node
// Regenerates the e2e fixture folder from scratch: solid-color JPEGs with
// staggered DateTimeOriginal so capture sort is deterministic. Always rebuilt
// per run — the XMP write-through queue embeds ratings into these files, so
// reusing them would leak verdicts from the previous run into adoption.
import { execFileSync } from 'node:child_process';
import { mkdirSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';

const outDir = process.argv[2];
const count = Number(process.argv[3] ?? 12);
const ifMissing = process.argv[4] === '--if-missing';
if (!outDir) {
  console.error('usage: e2e-fixtures.mjs <outDir> [count] [--if-missing]');
  process.exit(1);
}

// --if-missing: reuse an intact set (perf fixtures are never rated, so they
// stay XMP-clean and are expensive to rebuild at storm scale).
if (ifMissing) {
  try {
    const jpgs = (await import('node:fs')).readdirSync(outDir).filter((f) => f.endsWith('.jpg'));
    if (jpgs.length === count) {
      console.log(`e2e fixtures: reusing ${count} JPEGs in ${outDir}`);
      process.exit(0);
    }
  } catch {
    // dir missing — fall through and build it
  }
}

const chunk = (type, data) => {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(zlib.crc32(body) >>> 0);
  return Buffer.concat([len, body, crc]);
};

// Minimal 8-bit RGB PNG; sips turns it into a full-size JPEG so the decode
// path carries realistic per-pixel cost.
const png = (w, h, [r, g, b]) => {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // color type: truecolor
  const row = Buffer.alloc(1 + w * 3); // leading 0 = no filter
  for (let x = 0; x < w; x += 1) {
    row[1 + x * 3] = r;
    row[2 + x * 3] = g;
    row[3 + x * 3] = b;
  }
  const raw = Buffer.concat(Array.from({ length: h }, () => row));
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
};

rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

const scratch = path.join(tmpdir(), `ember-e2e-fixture-${process.pid}.png`);
for (let i = 1; i <= count; i += 1) {
  const hue = (i * 137) % 360; // spread colors so flips are visually distinct
  const c = Math.round(127 + 127 * Math.cos((hue * Math.PI) / 180));
  const s = Math.round(127 + 127 * Math.sin((hue * Math.PI) / 180));
  writeFileSync(scratch, png(8, 8, [c, s, 255 - c]));
  const jpg = path.join(outDir, `IMG_${String(i).padStart(4, '0')}.jpg`);
  execFileSync('sips', [
    '-s', 'format', 'jpeg',
    '-s', 'formatOptions', '90',
    '--resampleHeightWidth', '4000', '6000',
    scratch,
    '--out', jpg,
  ], { stdio: 'pipe' });
  execFileSync('exiftool', [
    '-overwrite_original',
    '-Make=FUJIFILM',
    '-Model=X-T50',
    `-DateTimeOriginal=2026:08:01 ${String(12 + Math.floor(i / 3600)).padStart(2, '0')}:${String(Math.floor(i / 60) % 60).padStart(2, '0')}:${String(i % 60).padStart(2, '0')}`,
    jpg,
  ], { stdio: 'pipe' });
  const capturedAt = new Date(Date.UTC(2026, 7, 1, 19, 0, i));
  utimesSync(jpg, capturedAt, capturedAt);
}
rmSync(scratch, { force: true });
console.log(`e2e fixtures: ${count} JPEGs in ${outDir}`);
