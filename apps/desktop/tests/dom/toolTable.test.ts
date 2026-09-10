// @vitest-environment jsdom
/**
 * Toolchain table rendering (princess:tools:detect, contract §3).
 *
 * The data below is shaped exactly like the Rust `ToolsDetectData` the engine
 * returns, including a MISSING row, so the missing-tool path is rendered too.
 */

import { beforeEach, describe, expect, it } from 'vitest';
import {
  createToolTableState,
  renderToolTable,
  toRows,
} from '../../src/components/toolTable.js';
import type { ToolsDetectData } from '../../src/contract/ipc.js';

const data: ToolsDetectData = {
  tools: [
    {
      name: 'cargo',
      status: 'ok',
      version: 'cargo 1.98.1 (797e8a9bc 2026-08-05)',
      path: '/root/PrincessIDE/.toolchain/cargo/bin/cargo',
      available: true,
      required: true,
      checks: [],
    },
    {
      name: 'qemu-system-x86_64',
      status: 'ok',
      version: 'QEMU emulator version 7.2.22 (Debian 1:7.2+dfsg-7+deb12u18+b3)',
      path: '/root/PrincessIDE/.toolchain/prefix/usr/bin/qemu-system-x86_64',
      available: true,
      required: true,
      checks: [],
    },
    // A verification row: doctor printed no version and no path for it.
    {
      name: 'clangd-16 (LSP)',
      status: 'ok',
      version: null,
      path: null,
      available: true,
      required: true,
      checks: ['version matches /clangd version 16\\./'],
    },
    { name: 'nasm', status: 'MISSING', version: null, path: null, available: false, required: true, checks: [] },
    {
      name: 'pnpm',
      status: 'ok',
      version: '12.3.4',
      path: '/root/node-v24.20.0-linux-x64/bin/pnpm',
      available: true,
      required: false,
      checks: [],
    },
  ],
  exitCode: 1,
  missing: ['nasm'],
  root: '/root/PrincessIDE',
  command: "bash -c 'source /root/PrincessIDE/scripts/env.sh && exec bash /root/PrincessIDE/scripts/doctor.sh'",
  rawStdout: 'cargo                  ok         cargo 1.98.1\n',
  rawStderr: 'doctor: 4 tool(s) present, MISSING REQUIRED: nasm\n',
};

let host: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  host = document.createElement('div');
  document.body.appendChild(host);
});

describe('tool table', () => {
  it('maps engine rows to render rows without rewriting values', () => {
    const rows = toRows(data);
    expect(rows.map((r) => r.status)).toEqual(['ok', 'ok', 'ok', 'MISSING', 'ok']);
    expect(rows[3]).toEqual({
      name: 'nasm',
      status: 'MISSING',
      available: false,
      version: '(no version printed)',
      path: '(not resolved)',
      required: true,
      checks: [],
    });
    // An available tool whose path doctor did not print must not read as missing.
    expect(rows[2]?.path).toBe('(path not printed by doctor)');
    expect(rows[2]?.checks).toEqual(['version matches /clangd version 16\\./']);
  });

  it('renders one table row per tool with path and version columns', () => {
    renderToolTable(host, { data, error: null, loading: false });
    const rows = host.querySelectorAll('tbody tr');
    expect(rows).toHaveLength(5);
    expect(host.querySelectorAll('thead th')).toHaveLength(5);

    const first = rows[0] as HTMLTableRowElement;
    expect(first.dataset['tool']).toBe('cargo');
    expect(first.dataset['status']).toBe('ok');
    expect(first.cells[1]?.textContent).toBe('ok');
    expect(first.cells[2]?.textContent).toContain('cargo 1.98.1');
    expect(first.cells[3]?.textContent).toBe('/root/PrincessIDE/.toolchain/cargo/bin/cargo');
    expect(first.cells[4]?.textContent).toBe('');

    const nasm = rows[3] as HTMLTableRowElement;
    expect(nasm.dataset['status']).toBe('MISSING');
    expect(nasm.cells[1]?.className).toBe('missing');

    // Verification notes are shown verbatim in their own column.
    const clangd16 = rows[2] as HTMLTableRowElement;
    expect(clangd16.cells[4]?.textContent).toBe('version matches /clangd version 16\\./');
    expect(clangd16.dataset['available']).toBe('true');
  });

  it('keeps the raw WRONG VER status visible instead of rounding it to "missing"', () => {
    const wrongVersion: ToolsDetectData = {
      ...data,
      tools: [
        {
          name: 'clangd-16 (LSP)',
          status: 'WRONG VER',
          version: null,
          path: '/root/PrincessIDE/.toolchain/prefix/usr/bin/clangd-16',
          available: false,
          required: true,
          checks: ['Debian clangd version 14.0.6 (wanted /clangd version 16\\./)'],
        },
      ],
      missing: ['clangd-16 (LSP)'],
    };
    renderToolTable(host, { data: wrongVersion, error: null, loading: false });
    const row = host.querySelector('tbody tr') as HTMLTableRowElement;
    expect(row.dataset['status']).toBe('WRONG VER');
    expect(row.cells[1]?.textContent).toBe('WRONG VER');
    expect(row.cells[1]?.className).toBe('missing');
    expect(row.cells[4]?.textContent).toContain('wanted /clangd version 16');
  });

  it('summarises the doctor exit code and the missing tools', () => {
    renderToolTable(host, { data, error: null, loading: false });
    const summary = host.querySelector('[data-testid="tool-table-summary"]');
    expect(summary?.textContent).toBe('doctor.sh exit 1 · 5 tools · missing: nasm');
  });

  it('marks optional tools and keeps the raw doctor output available', () => {
    renderToolTable(host, { data, error: null, loading: false });
    const optionalRow = [...host.querySelectorAll('tbody tr')][4] as HTMLTableRowElement;
    expect(optionalRow.cells[0]?.textContent).toBe('pnpm (optional)');
    const raw = host.querySelector('[data-testid="tool-raw"]');
    expect(raw?.textContent).toContain("bash -c 'source");
    expect(raw?.textContent).toContain('MISSING REQUIRED: nasm');
  });

  it('renders the loading state', () => {
    renderToolTable(host, { data: null, error: null, loading: true });
    expect(host.querySelector('[data-testid="tool-table-loading"]')?.textContent).toContain('princess:tools:detect');
    expect(host.querySelector('table')).toBeNull();
  });

  it('renders an engine failure explicitly, with code and raw detail', () => {
    renderToolTable(host, {
      data: null,
      error: { code: 'E_TIMEOUT', message: 'toolchain detection timed out', detail: 'doctor.sh exceeded 60s and was killed' },
      loading: false,
    });
    const box = host.querySelector('[data-testid="tool-table-error"]');
    expect(box?.textContent).toContain('E_TIMEOUT');
    expect(box?.textContent).toContain('doctor.sh exceeded 60s and was killed');
    expect(host.querySelector('table')).toBeNull();
  });

  it('renders the empty state before any detection', () => {
    renderToolTable(host, createToolTableState());
    expect(host.textContent).toContain('not detected yet');
  });

  it('never injects engine output as markup', () => {
    const hostile: ToolsDetectData = {
      ...data,
      tools: [
        {
          name: '<img src=x onerror=alert(1)>',
          status: 'ok',
          version: '<script>bad()</script>',
          path: '/tmp/<b>',
          available: true,
          required: true,
          checks: [],
        },
      ],
      missing: [],
    };
    renderToolTable(host, { data: hostile, error: null, loading: false });
    expect(host.querySelector('script')).toBeNull();
    expect(host.querySelector('img')).toBeNull();
    expect(host.querySelector('tbody td')?.textContent).toBe('<img src=x onerror=alert(1)>');
  });
});
