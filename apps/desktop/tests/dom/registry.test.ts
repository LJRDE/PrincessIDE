// @vitest-environment jsdom
/**
 * Registry guarantee test — P-A structural fix for BUG-001 / BUG-002.
 *
 * This test iterates the view registry and asserts that **every registered
 * view's `testid` appears in the DOM** after `mountApp()`.  This is the
 * structural guarantee that "a component exists but is never mounted" is
 * impossible.
 *
 * Negative sample: temporarily removing a view from the registry must cause
 * this test to fail.  (The actual negative-sample run is done manually in
 * the P-A acceptance workflow — see REPORT.md.)
 */

import { describe, expect, it, beforeEach } from 'vitest';
import { mountApp } from '../../src/app.js';
import { listViews } from '../../src/views/registry.js';

let root: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  root = document.createElement('div');
  root.id = 'app';
  document.body.appendChild(root);
});

describe('view registry guarantee', () => {
  it('every registered view has its testid in the DOM after mountApp()', () => {
    const views = listViews();
    // Sanity: we expect at least the 5 known views.
    expect(views.length).toBeGreaterThanOrEqual(5);

    const app = mountApp(root);

    for (const view of views) {
      const el = root.querySelector(`[data-testid="${view.testid}"]`);
      expect(el, `view "${view.id}" (testid="${view.testid}") should be in the DOM after mountApp()`).not.toBeNull();
    }

    app.destroy();
  });

  it('registry contains the expected view ids', () => {
    const ids = listViews().map((v) => v.id);
    expect(ids).toContain('action-panel');
    expect(ids).toContain('toolchain-panel');
    expect(ids).toContain('editor-panel');
    expect(ids).toContain('events-panel');
    expect(ids).toContain('debug-panel');
  });

  it('each view has a unique testid', () => {
    const views = listViews();
    const testids = views.map((v) => v.testid);
    const unique = new Set(testids);
    expect(unique.size).toBe(testids.length);
  });
});
