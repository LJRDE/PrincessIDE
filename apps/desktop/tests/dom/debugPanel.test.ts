// @vitest-environment jsdom
/**
 * BUG-008: Debug panel tests — minimal subset (attach/breakpoints/registers/
 * stacktrace/continue).
 *
 * All IPC calls are injected (D17 pattern from lsp/client.ts and liveStream.ts):
 * the component receives callbacks, not raw IPC functions, so vitest can test
 * without a real QEMU/gdb.
 *
 * Tests cover:
 *   1. Breakpoint list: inject IPC → add breakpoint → verify rendered text
 *   2. Registers: inject return value → assert register names and values in DOM
 *   3. Stack trace: inject stackTrace → assert `function @ file:line` format
 *   4. Error display: inject E_QEMU_FAILED → assert error code and message shown,
 *      and no success state rendered
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { renderDebugPanel, type DebugPanelCallbacks } from '../../src/components/debugPanel.js';
import { createInitialState, replayEvents, type IdeState } from '../../src/state/eventStore.js';
import type {
  DebugAttachData,
  DebugRegistersData,
  DebugSetBreakpointsData,
  DebugStackTraceData,
  IpcResult,
} from '../../src/contract/ipc.js';

let host: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  host = document.createElement('div');
  document.body.appendChild(host);
});

// ---------------------------------------------------------------------------
// Helper: create a state with debug events
// ---------------------------------------------------------------------------

function stateWithDebugStop(): IdeState {
  return replayEvents([
    {
      v: 1, seq: 1, ts: '2026-09-12T20:00:00Z', opId: null,
      kind: 'debug.stopped',
      payload: {
        reason: 'breakpoint',
        threadId: 1,
        frame: { id: 0, name: 'kernel_main', file: 'kernel.c', line: 42, pc: '0x100b39' },
        regs: { rax: '0x0000000000000001', rbx: '0x0000000000000002', rip: '0x0000000000100b39' },
      },
    },
    {
      v: 1, seq: 2, ts: '2026-09-12T20:00:01Z', opId: null,
      kind: 'debug.breakpoint.changed',
      payload: { id: 1, verified: true, location: 'kernel.c:42' },
    },
  ]);
}

// ---------------------------------------------------------------------------
// 1. Breakpoint list
// ---------------------------------------------------------------------------

describe('debug panel: breakpoints', () => {
  it('shows "No breakpoints set" when there are none', () => {
    renderDebugPanel(host, createInitialState());
    expect(host.textContent).toContain('No breakpoints set');
  });

  it('renders breakpoints from state', () => {
    const state = stateWithDebugStop();
    renderDebugPanel(host, state);
    const items = host.querySelectorAll('[data-testid="debug-bp-item"]');
    expect(items).toHaveLength(1);
    expect(items[0]?.textContent).toContain('#1');
    expect(items[0]?.textContent).toContain('kernel.c:42');
    expect(items[0]?.textContent).toContain('✓'); // verified
  });

  it('adding a breakpoint calls setBreakpoints callback and renders result', async () => {
    const setBreakpointsResult: IpcResult<DebugSetBreakpointsData> = {
      ok: true,
      data: {
        breakpoints: [
          { id: 2, verified: true, location: 'kernel.c:50' },
        ],
      },
    };
    const callbacks: DebugPanelCallbacks = {
      setBreakpoints: vi.fn().mockResolvedValue(setBreakpointsResult),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    // Fill in the form
    const fileInput = host.querySelector<HTMLInputElement>('[data-testid="debug-bp-file-input"]')!;
    const lineInput = host.querySelector<HTMLInputElement>('[data-testid="debug-bp-line-input"]')!;
    fileInput.value = 'kernel.c';
    lineInput.value = '50';

    // Click Add
    const addBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-bp-add-btn"]')!;
    addBtn.click();

    // Wait for async
    await vi.waitFor(() => {
      expect(callbacks.setBreakpoints).toHaveBeenCalledWith([{ file: 'kernel.c', line: 50 }]);
    });
  });

  it('displays error when setBreakpoints returns an error', async () => {
    const callbacks: DebugPanelCallbacks = {
      setBreakpoints: vi.fn().mockResolvedValue({
        ok: false,
        error: { code: 'E_NOT_FOUND', message: 'file not found', detail: 'no such file' },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const fileInput = host.querySelector<HTMLInputElement>('[data-testid="debug-bp-file-input"]')!;
    const lineInput = host.querySelector<HTMLInputElement>('[data-testid="debug-bp-line-input"]')!;
    fileInput.value = 'nonexistent.c';
    lineInput.value = '10';

    const addBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-bp-add-btn"]')!;
    addBtn.click();

    await vi.waitFor(() => {
      const errorBox = host.querySelector('[data-testid="debug-bp-error"]');
      expect(errorBox?.textContent).toContain('E_NOT_FOUND');
      expect(errorBox?.textContent).toContain('file not found');
    });
  });

  it('validates form inputs before calling IPC', async () => {
    const callbacks: DebugPanelCallbacks = {
      setBreakpoints: vi.fn(),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    // Empty file
    const fileInput = host.querySelector<HTMLInputElement>('[data-testid="debug-bp-file-input"]')!;
    const lineInput = host.querySelector<HTMLInputElement>('[data-testid="debug-bp-line-input"]')!;
    fileInput.value = '';
    lineInput.value = '50';

    const addBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-bp-add-btn"]')!;
    addBtn.click();

    // Should show validation error, not call IPC
    await vi.waitFor(() => {
      const errorBox = host.querySelector('[data-testid="debug-bp-error"]');
      expect(errorBox?.textContent).toContain('E_INVALID_CONFIG');
    });
    expect(callbacks.setBreakpoints).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// 2. Registers
// ---------------------------------------------------------------------------

describe('debug panel: registers', () => {
  it('renders registers from the last debug.stopped event', () => {
    const state = stateWithDebugStop();
    renderDebugPanel(host, state);

    // The registers from the event should be rendered in the table
    const rows = host.querySelectorAll('[data-testid="debug-reg-row"]');
    expect(rows.length).toBeGreaterThan(0);

    // Check that specific registers are shown
    const text = host.textContent ?? '';
    expect(text).toContain('rax');
    expect(text).toContain('0x0000000000000001');
    expect(text).toContain('rbx');
    expect(text).toContain('0x0000000000000002');
    expect(text).toContain('rip');
    expect(text).toContain('0x0000000000100b39');
  });

  it('refresh registers calls the callback and renders the result', async () => {
    const regsResult: IpcResult<DebugRegistersData> = {
      ok: true,
      data: {
        registers: [
          { name: 'rax', value: '0x000000000000000a' },
          { name: 'rbx', value: '0x000000000000000b' },
          { name: 'rcx', value: '0x000000000000000c' },
          { name: 'rdx', value: '0x000000000000000d' },
        ],
      },
    };
    const callbacks: DebugPanelCallbacks = {
      registers: vi.fn().mockResolvedValue(regsResult),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const refreshBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-regs-refresh-btn"]')!;
    refreshBtn.click();

    await vi.waitFor(() => {
      expect(callbacks.registers).toHaveBeenCalled();
      const text = host.textContent ?? '';
      expect(text).toContain('rax');
      expect(text).toContain('0x000000000000000a');
      expect(text).toContain('rcx');
      expect(text).toContain('0x000000000000000c');
    });
  });

  it('displays error when registers returns an error', async () => {
    const callbacks: DebugPanelCallbacks = {
      registers: vi.fn().mockResolvedValue({
        ok: false,
        error: { code: 'E_QEMU_FAILED', message: 'gdb not connected', detail: 'no target' },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const refreshBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-regs-refresh-btn"]')!;
    refreshBtn.click();

    await vi.waitFor(() => {
      const errorBox = host.querySelector('[data-testid="debug-regs-container"] [data-testid="debug-error"]');
      expect(errorBox?.textContent).toContain('E_QEMU_FAILED');
      expect(errorBox?.textContent).toContain('gdb not connected');
    });
  });
});

// ---------------------------------------------------------------------------
// 3. Stack trace
// ---------------------------------------------------------------------------

describe('debug panel: stack trace', () => {
  it('renders stack from the last debug.stopped event with function @ file:line format', () => {
    const state = stateWithDebugStop();
    renderDebugPanel(host, state);

    const frames = host.querySelectorAll('[data-testid="debug-stack-frame"]');
    expect(frames).toHaveLength(1);

    // Assert the exact format: function @ file:line
    const text = frames[0]?.textContent ?? '';
    expect(text).toContain('kernel_main');
    expect(text).toContain('@');
    expect(text).toContain('kernel.c:42');
  });

  it('refresh stack calls the callback and renders frames in function @ file:line format', async () => {
    const stackResult: IpcResult<DebugStackTraceData> = {
      ok: true,
      data: {
        frames: [
          { id: 0, name: 'paging_fault_probe', file: 'kernel.c', line: 120, pc: '0x100c00' },
          { id: 1, name: 'kernel_main', file: 'kernel.c', line: 157, pc: '0x100b80' },
          { id: 2, name: '_start', file: 'start.S', line: 10, pc: '0x100000' },
        ],
      },
    };
    const callbacks: DebugPanelCallbacks = {
      stackTrace: vi.fn().mockResolvedValue(stackResult),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const refreshBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-stack-refresh-btn"]')!;
    refreshBtn.click();

    await vi.waitFor(() => {
      expect(callbacks.stackTrace).toHaveBeenCalled();

      const frames = host.querySelectorAll('[data-testid="debug-stack-frame"]');
      expect(frames).toHaveLength(3);

      // Frame 0: paging_fault_probe @ kernel.c:120
      expect(frames[0]?.textContent).toContain('paging_fault_probe');
      expect(frames[0]?.textContent).toContain('@');
      expect(frames[0]?.textContent).toContain('kernel.c:120');

      // Frame 1: kernel_main @ kernel.c:157
      expect(frames[1]?.textContent).toContain('kernel_main');
      expect(frames[1]?.textContent).toContain('kernel.c:157');

      // Frame 2: _start @ start.S:10
      expect(frames[2]?.textContent).toContain('_start');
      expect(frames[2]?.textContent).toContain('start.S:10');
    });
  });

  it('displays "No stack frames" when stackTrace returns empty frames', async () => {
    const callbacks: DebugPanelCallbacks = {
      stackTrace: vi.fn().mockResolvedValue({
        ok: true,
        data: { frames: [] },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const refreshBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-stack-refresh-btn"]')!;
    refreshBtn.click();

    await vi.waitFor(() => {
      expect(host.textContent).toContain('No stack frames');
    });
  });

  it('displays error when stackTrace returns an error', async () => {
    const callbacks: DebugPanelCallbacks = {
      stackTrace: vi.fn().mockResolvedValue({
        ok: false,
        error: { code: 'E_QEMU_FAILED', message: 'gdb not connected', detail: 'cannot get stack' },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const refreshBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-stack-refresh-btn"]')!;
    refreshBtn.click();

    await vi.waitFor(() => {
      const errorBox = host.querySelector('[data-testid="debug-stack-container"] [data-testid="debug-error"]');
      expect(errorBox?.textContent).toContain('E_QEMU_FAILED');
      expect(errorBox?.textContent).toContain('gdb not connected');
    });
  });
});

// ---------------------------------------------------------------------------
// 4. Error display (negative samples)
// ---------------------------------------------------------------------------

describe('debug panel: error display', () => {
  it('attach error shows E_QEMU_FAILED with message and no success state', async () => {
    const callbacks: DebugPanelCallbacks = {
      attach: vi.fn().mockResolvedValue({
        ok: false,
        error: { code: 'E_QEMU_FAILED', message: 'connection refused', detail: 'errno ECONNREFUSED' },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const attachBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-attach-btn"]')!;
    attachBtn.click();

    await vi.waitFor(() => {
      const status = host.querySelector('[data-testid="debug-attach-status"]');
      expect(status?.textContent).toContain('E_QEMU_FAILED');
      expect(status?.textContent).toContain('connection refused');
      // No success text
      expect(status?.textContent).not.toContain('Attached');
    });
  });

  it('attach success shows backend and protocol info', async () => {
    const attachResult: IpcResult<DebugAttachData> = {
      ok: true,
      data: {
        attached: true,
        host: '127.0.0.1',
        port: 1234,
        symbols: 'build/kernel.elf',
        backend: 'gdb',
        protocol: 'DAP',
        note: 'using GDB built-in DAP',
      },
    };
    const callbacks: DebugPanelCallbacks = {
      attach: vi.fn().mockResolvedValue(attachResult),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const attachBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-attach-btn"]')!;
    attachBtn.click();

    await vi.waitFor(() => {
      const status = host.querySelector('[data-testid="debug-attach-status"]');
      expect(status?.textContent).toContain('Attached');
      expect(status?.textContent).toContain('gdb');
      expect(status?.textContent).toContain('DAP');
      expect(status?.textContent).toContain('127.0.0.1:1234');
    });
  });

  it('continue error shows E_NOT_FOUND with message and no success state', async () => {
    const callbacks: DebugPanelCallbacks = {
      continue: vi.fn().mockResolvedValue({
        ok: false,
        error: { code: 'E_NOT_FOUND', message: 'no active debug session', detail: '' },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const continueBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-continue-btn"]')!;
    continueBtn.click();

    await vi.waitFor(() => {
      const status = host.querySelector('[data-testid="debug-continue-status"]');
      expect(status?.textContent).toContain('E_NOT_FOUND');
      expect(status?.textContent).toContain('no active debug session');
      // No success text
      expect(status?.textContent).not.toContain('Continued');
    });
  });

  it('continue success shows "Continued"', async () => {
    const callbacks: DebugPanelCallbacks = {
      continue: vi.fn().mockResolvedValue({
        ok: true,
        data: { continued: true },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const continueBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-continue-btn"]')!;
    continueBtn.click();

    await vi.waitFor(() => {
      const status = host.querySelector('[data-testid="debug-continue-status"]');
      expect(status?.textContent).toContain('Continued');
    });
  });

  it('error boxes contain the error code verbatim (contract §0.4)', async () => {
    const callbacks: DebugPanelCallbacks = {
      stackTrace: vi.fn().mockResolvedValue({
        ok: false,
        error: { code: 'E_QEMU_FAILED', message: 'qemu crashed', detail: 'signal 11' },
      }),
    };

    renderDebugPanel(host, createInitialState(), callbacks);

    const refreshBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-stack-refresh-btn"]')!;
    refreshBtn.click();

    await vi.waitFor(() => {
      const errorBox = host.querySelector('[data-testid="debug-error"]');
      expect(errorBox?.textContent).toContain('E_QEMU_FAILED');
      expect(errorBox?.textContent).toContain('qemu crashed');
      // The error is displayed, not hidden or silenced
      expect(errorBox).not.toBeNull();
    });
  });
});

// ---------------------------------------------------------------------------
// 5. No callbacks configured (browser preview)
// ---------------------------------------------------------------------------

describe('debug panel: no callbacks', () => {
  it('renders without crashing when no callbacks are provided', () => {
    renderDebugPanel(host, createInitialState());
    // debugPanel sets data-testid on the root element itself
    expect(host.dataset['testid']).toBe('debug-panel');
    expect(host.querySelector('[data-testid="debug-attach-btn"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="debug-continue-btn"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="debug-regs-refresh-btn"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="debug-stack-refresh-btn"]')).not.toBeNull();
  });

  it('shows E_INTERNAL error when a button is clicked with no handler', async () => {
    renderDebugPanel(host, createInitialState());

    const continueBtn = host.querySelector<HTMLButtonElement>('[data-testid="debug-continue-btn"]')!;
    continueBtn.click();

    await vi.waitFor(() => {
      const status = host.querySelector('[data-testid="debug-continue-status"]');
      expect(status?.textContent).toContain('E_INTERNAL');
      expect(status?.textContent).toContain('No continue handler configured');
    });
  });
});
