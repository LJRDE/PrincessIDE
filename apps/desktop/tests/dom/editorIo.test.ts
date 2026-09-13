// @vitest-environment jsdom
/**
 * Editor file I/O wiring (§3 fs).
 *
 * Until fs:read / fs:write existed the editor could only ever show its built-in
 * scratch buffer — there was no path from disk into the buffer or back out.
 * These tests pin the wiring rather than the engine:
 *
 *   1. the open/save controls exist and are mounted (the BUG-002 lesson:
 *      a component that exists is not a component that is used);
 *   2. `fileSelectorFn` injected into `mountApp` really reaches the editor view;
 *   3. a missing engine is reported explicitly (contract §0.4) — the same rule
 *      the rest of the shell is held to, and the one that would otherwise let a
 *      broken open look like a successful one.
 *
 * The engine-side behaviour (path confinement, atomic writes) is covered by the
 * Rust tests in `src-tauri/src/fs_handler.rs`.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mountApp } from '../../src/app.js';

let root: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  root = document.createElement('div');
  root.id = 'app';
  document.body.appendChild(root);
});

const el = <T extends HTMLElement>(testid: string): T => {
  const found = root.querySelector<T>(`[data-testid="${testid}"]`);
  if (!found) throw new Error(`missing [data-testid=${testid}]`);
  return found;
};

const status = (): string => el('editor-status').textContent ?? '';

/** Let the click handler's awaited IPC promise settle. */
const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

describe('editor file I/O', () => {
  it('mounts the open/save controls', () => {
    mountApp(root);

    expect(root.querySelector('[data-testid="editor-open-btn"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="editor-save-btn"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="editor-path-input"]')).not.toBeNull();
    expect(status()).toContain('scratch buffer');
  });

  it('says there is nothing to save instead of failing silently', async () => {
    mountApp(root);
    el<HTMLButtonElement>('editor-save-btn').click();
    await settle();

    expect(status()).toContain('nothing to save');
    expect(status()).toContain('open a file first');
  });

  it('routes the injected file selector from mountApp into the editor view', async () => {
    const fileSelectorFn = vi.fn(async (): Promise<string | null> => null);
    mountApp(root, { fileSelectorFn });

    el<HTMLButtonElement>('editor-open-btn').click();
    await settle();

    // Proves the D17-style injection actually reaches the view: without the
    // app.ts wiring this would still be zero.
    expect(fileSelectorFn).toHaveBeenCalledTimes(1);
  });

  it('reports a failed read explicitly rather than pretending it worked', async () => {
    mountApp(root);

    const input = el<HTMLInputElement>('editor-path-input');
    input.value = '/nonexistent/definitely-not-here.c';
    el<HTMLButtonElement>('editor-open-btn').click();
    await settle();

    // There is no Tauri engine under jsdom, so this read cannot succeed.  The
    // point is that the failure surfaces in the UI.
    expect(status()).toMatch(/open failed/);
    expect(el('editor-status').dataset['error']).toBe('true');
  });

  it('falls back to the path box when no native dialog is available', async () => {
    mountApp(root); // no fileSelectorFn: browser-preview behaviour
    el<HTMLButtonElement>('editor-open-btn').click();
    await settle();

    expect(status()).toContain('no native file dialog');
  });
});
