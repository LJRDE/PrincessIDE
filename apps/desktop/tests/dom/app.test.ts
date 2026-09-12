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
import type { ListenFn } from '../../src/lsp/client.js';
import type { AnyEvent } from '../../src/contract/events.js';

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

    // BUG-001: action panel and its buttons must be present.
    expect(root.querySelector('[data-testid="action-panel"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="project-open-btn"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="build-btn"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="run-btn"]')).not.toBeNull();

    // BUG-008: debug panel section must be present.
    expect(root.querySelector('[data-testid="debug-panel-section"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="debug-panel"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="debug-attach-btn"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="debug-continue-btn"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="debug-regs-refresh-btn"]')).not.toBeNull();
    expect(root.querySelector('[data-testid="debug-stack-refresh-btn"]')).not.toBeNull();

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

  it('live event bridge: build.started envelope updates action panel state', () => {
    // Create a fake listen function that we can push events through.
    let capturedHandler: ((event: { payload: unknown }) => void) | null = null;
    const fakeListen: ListenFn = vi.fn(async (handler) => {
      capturedHandler = handler;
      return () => { capturedHandler = null; };
    });

    const app = mountApp(root, { listenFn: fakeListen });

    // Verify the build button initially shows "Build" (not building).
    const buildBtnBefore = root.querySelector('[data-testid="build-btn"]');
    expect(buildBtnBefore?.textContent).toContain('Build');

    // Push a build.started envelope (as Tauri would emit on `princess:event`).
    const buildStartedEnvelope: AnyEvent = {
      v: 1,
      seq: 100,
      ts: '2026-09-12T20:00:00Z',
      opId: 'op-0001',
      kind: 'build.started',
      payload: {
        backend: 'make',
        toolchainId: 'x86_64-elf-gcc',
        argv: ['make'],
        cwd: '/project',
      },
    };

    // Simulate Tauri event: the envelope is wrapped in event.payload.
    expect(capturedHandler).not.toBeNull();
    capturedHandler!({ payload: buildStartedEnvelope });

    // After the event, the build button should show "Cancel Build".
    const buildBtnAfter = root.querySelector('[data-testid="build-btn"]');
    expect(buildBtnAfter?.textContent).toContain('Cancel Build');

    // The state should reflect the build is running.
    expect(app.state().build?.running).toBe(true);

    // The opId should be tracked.
    expect(app.lastBuildOpId()).toBe('op-0001');

    app.destroy();
  });

  it('live event bridge: run.started envelope updates action panel state', () => {
    let capturedHandler: ((event: { payload: unknown }) => void) | null = null;
    const fakeListen: ListenFn = vi.fn(async (handler) => {
      capturedHandler = handler;
      return () => { capturedHandler = null; };
    });

    const app = mountApp(root, { listenFn: fakeListen });

    // Push a run.started envelope.
    const runStartedEnvelope: AnyEvent = {
      v: 1,
      seq: 200,
      ts: '2026-09-12T20:01:00Z',
      opId: 'op-0002',
      kind: 'run.started',
      payload: {
        qemuArgv: ['qemu-system-x86_64', '-cdrom', 'kernel.iso'],
        gdbStub: null,
      },
    };

    expect(capturedHandler).not.toBeNull();
    capturedHandler!({ payload: runStartedEnvelope });

    // The run button should show "Stop".
    const runBtn = root.querySelector('[data-testid="run-btn"]');
    expect(runBtn?.textContent).toContain('Stop');

    // The state should reflect the run is running.
    expect(app.state()?.run?.running).toBe(true);

    // The opId should be tracked.
    expect(app.lastRunOpId()).toBe('op-0002');

    app.destroy();
  });

  it('live event bridge degrades silently in browser preview (no listenFn)', () => {
    // No listenFn provided — should not throw.
    const app = mountApp(root);
    expect(app.lastBuildOpId()).toBeNull();
    expect(app.lastRunOpId()).toBeNull();
    app.destroy();
  });
});
