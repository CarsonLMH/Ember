import AxeBuilder from '@axe-core/webdriverio';
import { browser } from '@wdio/globals';
import type { Result } from 'axe-core';
import { waitForFolderOpen } from '../lib/harness.js';

const WCAG_TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'] as const;

function describeViolations(violations: Result[]): string {
  return violations
    .map((violation) => {
      const targets = violation.nodes.map((node) => node.target.join(' ')).join(', ');
      return `${violation.id} (${violation.impact ?? 'unknown'}): ${violation.help}\n  ${targets}`;
    })
    .join('\n');
}

describe('accessibility: steady-state culling shell', () => {
  it('has no automatically detectable WCAG A/AA violations', async () => {
    await waitForFolderOpen();

    // Tauri exposes one webview and rejects WebDriver's `window/new`, which
    // axe normally uses to aggregate frame results. Ember has no frames, so
    // legacy mode preserves the full ruleset without losing coverage here.
    const results = await new AxeBuilder({ client: browser })
      .setLegacyMode()
      .withTags([...WCAG_TAGS])
      .analyze();

    if (results.violations.length > 0) {
      throw new Error(`Accessibility violations:\n${describeViolations(results.violations)}`);
    }
  });
});
