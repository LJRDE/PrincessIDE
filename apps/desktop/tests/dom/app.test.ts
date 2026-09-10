// @vitest-environment jsdom
/**
 * Shell integration test: mount the real app in a DOM and check what a user
 * would see in a plain browser (i.e. without the Tauri engine behind it).
 *
 * Two things matter here and nowhere else:
 *   1. the shell renders its panels around a live CodeMirror editor;
 *   2. a missing engine is reported *explicitly* (contract §0.4) — never a blank
 *      screen and never a silent success.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mountApp } from '../../src/app.js';
import { listBundledFixtures } from '../../src/state/fixtureLoader.js';

let root: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  root = document.createElement('div');
  root.id = 'app';
  document.body.appendChild(root);
});

describe('app shell', () => {
  it('mounts every panel and a real CodeMirror editor', () => {
    const app = mountApp(root);

    expect(root.querySelector('[data-testid="toolchain-panel"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="editor-panel"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="events-panel"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="env"]')?.textContent).toContain('browser preview');
    expect(root.querySelector('[data-testid="lsp-status"]')?.textContent).toContain('D17');

    // CodeMirror really mounted (jsdom, no layout, but the DOM is there).
    const editorContent = root.querySelector('.cm-content');
    expect(editorContent).not.toBeNull();
    expect(editorContent?.textContent).toContain('PrincessIDE reference kernel booted');

    app.destroy();
  });

  it('replays the bundled fixture into the log panel on mount', () => {
    const app = mountApp(root);
    expect(listBundledFixtures()).toHaveLength(3);
    expect(root.querySelectorAll('.log-line').length).toBeGreaterThan(0);
    expect(root.querySelector('[data-testid="events-panel"]')?.textContent).toContain(
      'PrincessIDE reference kernel booted',
    );
    expect(root.querySelector('[data-testid="fault-symbol"]')?.textContent).toContain(
      'refkernel_fault_probe',
    );
    expect(app.state().lastSeq).toBe(17);
    app.destroy();
  });

  it('switches fixtures through the picker', () => {
    const app = mountApp(root);
    const picker = root.querySelector('[data-testid="fixture-picker"]') as HTMLSelectElement;
    expect([...picker.options].map((o) => o.value)).toEqual([
      'debug-session.ndjson',
      'lsp-session.ndjson',
      'refkernel-session.ndjson',
    ]);
    picker.value = 'debug-session.ndjson';
    picker.dispatchEvent(new Event('change'));
    expect(app.state().lastSeq).toBe(9);
    expect(root.querySelector('[data-testid="events-panel"]')?.textContent).toContain('Breakpoint 1 at');
    app.destroy();
  });

  it('reports the missing engine IPC through the toolchain panel instead of pretending', async () => {
    const app = mountApp(root);
    await app.refreshTools();

    const box = root.querySelector('[data-testid="tool-table-error"]');
    expect(box).not.toBeNull();
    // Outside Tauri, `invoke` is unavailable: E_INTERNAL with the real message.
    expect(box?.textContent).toContain('E_INTERNAL');
    expect(root.querySelector('table.tool-table')).toBeNull();
    app.destroy();
  });

  it('renders an engine error envelope verbatim when the engine answers with a failure', async () => {
    const app = mountApp(root);
    // Simulate the tab being served by Tauri's IPC with a real failure envelope.
    const internals = {
      invoke: vi.fn().mockResolvedValue({
        ok: false,
        error: { code: 'E_TOOLCHAIN_MISSING', message: 'nasm is missing', detail: 'doctor: MISSING REQUIRED: nasm' },
      }),
    };
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = internals;
    try {
      await app.refreshTools();
      const box = root.querySelector('[data-testid="tool-table-error"]');
      expect(box?.textContent).toContain('E_TOOLCHAIN_MISSING');
      expect(box?.textContent).toContain('doctor: MISSING REQUIRED: nasm');
    } finally {
      delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
      app.destroy();
    }
  });
});
