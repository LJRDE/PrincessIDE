// @vitest-environment jsdom
/**
 * Event panels rendered from the replayed state machine: status chips, the
 * append-only log, diagnostics and the fault card.  These are the panels a user
 * looks at when a kernel triple-faults, so the alarm states (gap, version
 * mismatch, missing symbolication) are asserted as well as the happy path.
 */

import { beforeEach, describe, expect, it } from 'vitest';
import {
  renderDiagnostics,
  renderEventLog,
  renderEventStatus,
  renderFaultCard,
} from '../../src/components/eventLog.js';
import refkernelRaw from '../../fixtures/events/refkernel-session.ndjson?raw';
import { parseNdjson } from '../../src/contract/parse.js';
import { createInitialState, replayEvents, type IdeState } from '../../src/state/eventStore.js';

const parsed = parseNdjson(refkernelRaw, 'refkernel-session');
const state = replayEvents(parsed.events);

let host: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  host = document.createElement('div');
  document.body.appendChild(host);
});

describe('event status bar', () => {
  it('shows counters derived from the stream health', () => {
    renderEventStatus(host, state);
    const text = host.textContent ?? '';
    expect(text).toContain('events: 17');
    expect(text).toContain('lastSeq: 17');
    expect(text).toContain('gaps: 0');
    expect(text).toContain('build: ok');
    expect(text).toContain('run: triple-fault');
    expect(text).toContain('artifact: fixtures/refkernel/build/refkernel.elf');
    expect(text).toContain('symbols: 47');
    expect(host.querySelector('[data-testid="request-replay"]')).toBeNull();
  });

  it('offers a replay action and warns when a seq gap was seen', () => {
    const gapped: IdeState = { ...state, gaps: [{ expected: 3, received: 5, size: 2 }], needsReplay: true };
    renderEventStatus(host, gapped);
    const btn = host.querySelector('[data-testid="request-replay"]');
    expect(btn?.textContent).toBe('request replay from seq 18');
    expect(host.textContent).toContain('gaps: 1');
  });

  it('warns loudly on an event-model version mismatch (contract §7)', () => {
    renderEventStatus(host, { ...state, versionMismatches: [2] });
    const warn = [...host.querySelectorAll('.chip.bad')].map((c) => c.textContent);
    expect(warn.join(' ')).toContain('event model v2 != UI v1');
  });
});

describe('log panel', () => {
  it('appends every chunk with its stream tag and seq', () => {
    renderEventLog(host, state);
    const lines = host.querySelectorAll('.log-line');
    expect(lines).toHaveLength(6);
    expect(lines[0]?.textContent).toContain('[ide]');
    expect(lines[0]?.getAttribute('data-seq')).toBe('1');
    expect(lines[3]?.textContent).toContain('[serial.com1]');
    expect(lines[3]?.textContent).toContain('PrincessIDE reference kernel booted');
    expect(lines[4]?.textContent).toContain('[qemu.monitor]');
  });

  it('renders multi-byte chunks unchanged', () => {
    renderEventLog(host, state);
    expect(host.textContent).toContain('内核构建中…');
  });

  it('marks utf8-lossy chunks instead of dropping or fixing them', () => {
    const lossy = replayEvents([
      {
        v: 1,
        seq: 1,
        ts: '2026-05-05T12:00:00.000Z',
        opId: null,
        kind: 'log.append',
        payload: { stream: 'serial.com1', chunk: 'bad \ufffd byte', encoding: 'utf8-lossy' },
      },
    ]);
    renderEventLog(host, lossy);
    expect(host.querySelector('[data-testid="lossy-marker"]')).not.toBeNull();
    expect(host.textContent).toContain('bad \ufffd byte');
  });

  it('filters by stream on request', () => {
    renderEventLog(host, state, 'build');
    expect(host.querySelectorAll('.log-line')).toHaveLength(2);
  });
});

describe('fault card and diagnostics', () => {
  it('shows the symbolicated fault exactly as the engine reported it', () => {
    renderFaultCard(host, state);
    expect(host.querySelector('[data-testid="fault"]')?.textContent).toBe(
      '#PF rip=0x0000000000100abc errorCode=0x2',
    );
    expect(host.querySelector('[data-testid="fault-symbol"]')?.textContent).toBe(
      'refkernel_fault_probe (fixtures/refkernel/kernel.c:42)',
    );
    expect(host.querySelector('[data-testid="run-exit"]')?.textContent).toBe(
      'exit 1 · triple-fault · 1260ms',
    );
  });

  it('says so plainly when symbolication was not available', () => {
    const noSymbols = replayEvents([
      {
        v: 1,
        seq: 1,
        ts: '2026-05-05T12:00:00.000Z',
        opId: null,
        kind: 'run.fault',
        payload: { vector: '#UD', rip: '0x1000', errorCode: '0x0', regs: {} },
      },
    ]);
    renderFaultCard(host, noSymbols);
    expect(host.textContent).toContain('not symbolicated');
    expect(host.querySelector('[data-testid="fault-symbol"]')).toBeNull();
  });

  it('shows the empty state before any run', () => {
    renderFaultCard(host, createInitialState());
    expect(host.textContent).toContain('no run yet');
  });

  it('lists diagnostics with file:line:col and source', () => {
    renderDiagnostics(host, state);
    expect(host.textContent).toContain('Diagnostics (1)');
    expect(host.querySelector('.diag-warning')?.textContent).toBe(
      'fixtures/refkernel/serial.c:27:5 warning [clang] unused parameter \'port\'',
    );
  });

  /**
   * BUG-007: Prove the diagnostic line is NOT confused with the `serial.com1`
   * log stream.  The fixlist claimed the diagnostic panel rendered
   * `/serial.com1:5:27 warning [clang] unused parameter port` — this test
   * constructs a state with both a `serial.com1` log line AND a
   * `build.diagnostic`, then asserts:
   *   1. The diagnostic text is exactly `file:line:col` (not truncated, not
   *      col:line).
   *   2. The log panel and the diagnostic panel are independent DOM
   *      elements — the diagnostic row never contains the stream name
   *      `serial.com1`.
   */
  it('BUG-007: diagnostic rendering does not cross-contaminate with serial.com1 log lines', () => {
    const mixed = replayEvents([
      {
        v: 1, seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null,
        kind: 'log.append',
        payload: { stream: 'serial.com1', chunk: 'PrincessIDE reference kernel booted\n', encoding: 'utf8' },
      },
      {
        v: 1, seq: 2, ts: '2026-05-05T12:00:00.120Z', opId: 'op-7f3a',
        kind: 'build.started',
        payload: { backend: 'make', toolchainId: 'host-clang14-nasm', argv: ['make'], cwd: '/root' },
      },
      {
        v: 1, seq: 3, ts: '2026-05-05T12:00:01.100Z', opId: 'op-7f3a',
        kind: 'build.diagnostic',
        payload: { severity: 'warning', file: 'fixtures/refkernel/serial.c', line: 27, col: 5, message: "unused parameter 'port'", source: 'clang' },
      },
      {
        v: 1, seq: 4, ts: '2026-05-05T12:00:01.900Z', opId: 'op-7f3a',
        kind: 'build.finished',
        payload: { status: 'ok', exitCode: 0, durationMs: 1842, artifacts: [] },
      },
    ]);

    // Diagnostic panel
    const diagHost = document.createElement('div');
    document.body.appendChild(diagHost);
    renderDiagnostics(diagHost, mixed);
    const diagRows = diagHost.querySelectorAll('.diag');
    expect(diagRows).toHaveLength(1);
    // Exact text — NOT `serial.com1:5:27`, NOT truncated path
    expect(diagRows[0]?.textContent).toBe(
      "fixtures/refkernel/serial.c:27:5 warning [clang] unused parameter 'port'",
    );
    // The diagnostic row must not contain the stream name from the log line
    expect(diagRows[0]?.textContent).not.toContain('serial.com1');

    // Log panel — verify the serial.com1 line rendered correctly there
    const logHost = document.createElement('div');
    document.body.appendChild(logHost);
    renderEventLog(logHost, mixed);
    const logRows = logHost.querySelectorAll('.log-line');
    expect(logRows).toHaveLength(1);
    expect(logRows[0]?.textContent).toContain('[serial.com1]');
    expect(logRows[0]?.textContent).toContain('PrincessIDE reference kernel booted');
  });
});
