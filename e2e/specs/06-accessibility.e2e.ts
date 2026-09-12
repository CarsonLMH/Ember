import AxeBuilder from '@axe-core/webdriverio';
import { $, browser, expect } from '@wdio/globals';
import type { Result } from 'axe-core';
import { hudPos, key, waitForFolderOpen } from '../lib/harness.js';

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

  it('makes shortcut help a focused modal without letting culling continue behind it', async () => {
    await waitForFolderOpen();
    const filmstrip = $('.filmstrip');
    await filmstrip.click();
    const before = await hudPos();

    await key('?');
    const dialog = $('.cheat-sheet');
    await dialog.waitForDisplayed();
    expect(await dialog.getAttribute('role')).toBe('dialog');
    expect(await dialog.getAttribute('aria-modal')).toBe('true');
    expect(
      await browser.execute(() => {
        const active = document.activeElement;
        return Boolean(active && document.querySelector('.cheat-sheet')?.contains(active));
      }),
    ).toBe(true);

    // The dialog itself receives initial focus. Reverse traversal from there
    // must wrap to the scrollable reference instead of escaping into the
    // culling UI; forward traversal cycles between that region and Close.
    await browser.execute(() => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Tab',
          code: 'Tab',
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(await $('.cheat-sections').isFocused()).toBe(true);
    await browser.execute(() => {
      document.activeElement?.dispatchEvent(
        new KeyboardEvent('keydown', {
          key: 'Tab',
          code: 'Tab',
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(await $('.cheat-close').isFocused()).toBe(true);
    await key('Tab');
    expect(await $('.cheat-sections').isFocused()).toBe(true);

    await key('ArrowRight');
    expect(await hudPos()).toEqual(before);
    await $('.cheat-section-navigate h3').click();
    expect(await dialog.isDisplayed()).toBe(true);

    const results = await new AxeBuilder({ client: browser })
      .setLegacyMode()
      .withTags([...WCAG_TAGS])
      .analyze();
    if (results.violations.length > 0) {
      throw new Error(`Shortcut dialog violations:\n${describeViolations(results.violations)}`);
    }

    await key('Escape');
    await dialog.waitForExist({ reverse: true });
    expect(await filmstrip.isFocused()).toBe(true);
  });
});
