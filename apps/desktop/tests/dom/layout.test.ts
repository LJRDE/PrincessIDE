// @vitest-environment jsdom
/**
 * Shell layout: activity rail | editor | dock.
 *
 * The previous layout put all five panels into two equal columns, so the editor
 * — the thing the window exists for — got a quarter of the space.  This pins
 * the replacement's contract:
 *
 *   1. the editor is always mounted and always in the main column;
 *   2. every other panel is reachable from the rail and hidden until asked for;
 *   3. the dock opens, switches and closes without unmounting anything, because
 *      unmounting the editor would throw away undo history and cursor position.
 *
 * All panel testids stay in the DOM at all times — the registry test relies on
 * that to catch a view that is registered but never mounted.
 */

import { beforeEach, describe, expect, it } from 'vitest';
import { mountApp } from '../../src/app.js';

let root: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  root = document.createElement('div');
  root.id = 'app';
  document.body.appendChild(root);
});

const q = (testid: string): HTMLElement | null =>
  root.querySelector<HTMLElement>(`[data-testid="${testid}"]`);

/**
 * The shell hides a panel by hiding the `<section>` it was mounted into.  That
 * section is addressed by `data-panel-id` rather than by testid, because panels
 * name their sections inconsistently (`toolchain-panel` vs
 * `action-panel-section`) and some reuse their id for an element *inside* the
 * section — `[data-testid="action-panel"]` is the inner container, not the
 * thing that gets hidden.
 */
const sectionOf = (viewId: string): HTMLElement | null =>
  root.querySelector<HTMLElement>(`[data-panel-id="${viewId}"]`);

const railBtn = (viewId: string): HTMLButtonElement => {
  const btn = q(`rail-${viewId}`);
  if (!btn) throw new Error(`no rail button for ${viewId}`);
  return btn as HTMLButtonElement;
};

const PANELS = ['action-panel', 'toolchain-panel', 'events-panel', 'debug-panel'];

describe('shell layout', () => {
  it('keeps the editor in the main column, not in the dock', () => {
    mountApp(root);

    const editorSection = sectionOf('editor-panel');
    expect(editorSection).not.toBeNull();
    expect(editorSection?.closest('.main')).not.toBeNull();
    expect(editorSection?.closest('.dock')).toBeNull();
    expect(editorSection?.hidden).toBe(false);

    // ...and it is a real CodeMirror, not an empty host.
    expect(root.querySelector('.cm-content')).not.toBeNull();
  });

  it('gives the rail one button per non-editor panel and no editor button', () => {
    mountApp(root);

    expect(q('activity-rail')).not.toBeNull();
    for (const id of PANELS) {
      expect(root.querySelector(`[data-testid="rail-${id}"]`)).not.toBeNull();
    }
    // The editor is always on screen, so a rail entry for it would be a lie.
    expect(root.querySelector('[data-testid="rail-editor-panel"]')).toBeNull();
  });

  it('still mounts every panel, so the registry test can see them all', () => {
    mountApp(root);
    for (const id of PANELS) {
      expect(sectionOf(id)).not.toBeNull();
    }
  });

  it('starts with the dock closed and every panel hidden', () => {
    mountApp(root);

    expect(q('dock')?.hidden).toBe(true);
    for (const id of PANELS) {
      expect(sectionOf(id)?.hidden).toBe(true);
    }
  });

  it('opens the dock on the clicked panel, and only that one', () => {
    mountApp(root);

    railBtn('events-panel').click();

    expect(q('dock')?.hidden).toBe(false);
    expect(sectionOf('events-panel')?.hidden).toBe(false);
    for (const id of PANELS.filter((p) => p !== 'events-panel')) {
      expect(sectionOf(id)?.hidden).toBe(true);
    }
    expect(q('dock-title')?.textContent).toBeTruthy();
    expect(railBtn('events-panel').getAttribute('aria-pressed')).toBe('true');
  });

  it('switches panels instead of stacking them', () => {
    mountApp(root);

    railBtn('action-panel').click();
    railBtn('debug-panel').click();

    expect(sectionOf('debug-panel')?.hidden).toBe(false);
    expect(sectionOf('action-panel')?.hidden).toBe(true);
    expect(railBtn('debug-panel').getAttribute('aria-pressed')).toBe('true');
    expect(railBtn('action-panel').getAttribute('aria-pressed')).toBe('false');
  });

  it('closes the dock when the active rail item is clicked again', () => {
    mountApp(root);

    railBtn('toolchain-panel').click();
    expect(q('dock')?.hidden).toBe(false);

    railBtn('toolchain-panel').click();
    expect(q('dock')?.hidden).toBe(true);
    expect(sectionOf('toolchain-panel')?.hidden).toBe(true);
  });

  it('closes the dock from its close button', () => {
    mountApp(root);

    railBtn('events-panel').click();
    q('dock-close')?.click();

    expect(q('dock')?.hidden).toBe(true);
    expect(railBtn('events-panel').getAttribute('aria-pressed')).toBe('false');
  });

  it('never unmounts the editor while panels come and go', () => {
    mountApp(root);

    const cm = root.querySelector('.cm-content');
    expect(cm).not.toBeNull();

    for (const id of PANELS) railBtn(id).click();

    // Same node: the editor was never rebuilt, so undo/cursor survive.
    expect(root.querySelector('.cm-content')).toBe(cm);
  });
});
