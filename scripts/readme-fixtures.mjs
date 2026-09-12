#!/usr/bin/env node
// Build a privacy-safe demo folder for the public README screenshot. The source
// photograph was generated for Ember and contains fictional adults; no personal
// library, Ember database, or user metadata is involved.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, rmSync, utimesSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = path.join(root, 'docs', 'assets', 'demo-coast.jpg');
const outDir = path.join(root, 'e2e', 'fixtures', 'readme');

rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

for (let i = 1; i <= 8; i += 1) {
  const jpg = path.join(outDir, `COAST_${String(i).padStart(4, '0')}.jpg`);
  copyFileSync(source, jpg);
  if (i % 2 === 0) {
    // A mirrored frame gives the filmstrip honest visual variation without
    // introducing another image source or pretending these are real photos.
    execFileSync('sips', ['--flip', 'horizontal', jpg], { stdio: 'pipe' });
  }
  execFileSync(
    'exiftool',
    [
      '-overwrite_original',
      '-Make=FUJIFILM',
      '-Model=X-T50',
      '-LensModel=XF23mmF2 R WR',
      '-FNumber=4',
      '-ExposureTime=1/500',
      '-ISO=400',
      '-FocalLength=23 mm',
      `-DateTimeOriginal=2026:09:01 17:42:${String(i).padStart(2, '0')}`,
      jpg,
    ],
    { stdio: 'pipe' },
  );
  const capturedAt = new Date(Date.UTC(2026, 8, 2, 0, 42, i));
  utimesSync(jpg, capturedAt, capturedAt);
}

console.log(`README fixtures: 8 generated-photo JPEGs in ${outDir}`);
