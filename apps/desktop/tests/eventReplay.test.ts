/**
 * P3-4 — event replay: fixture (NDJSON) → frontend state machine → snapshot.
 *
 * Two fixtures are replayed:
 *   - `refkernel-session.ndjson` (build → symbols → run → fault → exit → ai)
 *   - `debug-session.ndjson`     (debug.* kinds)
 * both constructed by P3 (see the file headers) because P2 has not landed the
 * real recordings yet.  The same test consumes `fixtures/events/*.ndjson`
 * unchanged the moment they exist.
 *
 * Every positive assertion is paired with a negative one (docs/spec/20-acceptance.md §0):
 * seq gaps, duplicates, out-of-order delivery, model-version mismatch and
 * malformed envelopes must all be surfaced, never swallowed.
 */

import { describe, expect, it } from 'vitest';
import refkernelRaw from '../fixtures/events/refkernel-session.ndjson?raw';
import debugRaw from '../fixtures/events/debug-session.ndjson?raw';
import lspRaw from '../fixtures/events/lsp-session.ndjson?raw';
import { parseNdjson, parseEventEnvelope, ContractViolation } from '../src/contract/parse.js';
import type { AnyEvent } from '../src/contract/events.js';
import {
  applyEvent,
  createInitialState,
  deriveSummary,
  MAX_LOG_LINES,
  MAX_LSP_MESSAGES,
  replayEvents,
} from '../src/state/eventStore.js';
import { listBundledFixtures, loadBundledFixture } from '../src/state/fixtureLoader.js';

const refkernel = parseNdjson(refkernelRaw, 'refkernel-session');
const debugSession = parseNdjson(debugRaw, 'debug-session');
const lspSession = parseNdjson(lspRaw, 'lsp-session');
const refkernelState = replayEvents(refkernel.events);
const debugState = replayEvents(debugSession.events);
const lspState = replayEvents(lspSession.events);

/**
 * Build a minimal valid envelope for the negative-path tests.  Defaults to a
 * `log.append` on the `ide` stream so a test only has to state what it varies.
 */
function env(partial: { seq: number } & Partial<AnyEvent>): AnyEvent {
  return {
    v: 1,
    ts: '2026-05-05T12:00:00.000Z',
    opId: null,
    kind: 'log.append',
    payload: { stream: 'ide', chunk: 'x\n', encoding: 'utf8' },
    ...partial,
  } as AnyEvent;
}

describe('fixtures are contract-legal', () => {
  it('parses every bundled fixture with zero violations', () => {
    expect(refkernel.violations).toEqual([]);
    expect(debugSession.violations).toEqual([]);
    expect(lspSession.violations).toEqual([]);
    expect(refkernel.events).toHaveLength(17);
    expect(debugSession.events).toHaveLength(9);
    expect(lspSession.events).toHaveLength(6);
  });

  it('every envelope carries the frozen envelope fields', () => {
    for (const e of [...refkernel.events, ...debugSession.events, ...lspSession.events]) {
      expect(Object.keys(e).sort()).toEqual(['kind', 'opId', 'payload', 'seq', 'ts', 'v']);
      expect(e.v).toBe(1);
      expect(typeof e.seq).toBe('number');
      expect(e.ts).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
      expect(e.opId === null || typeof e.opId === 'string').toBe(true);
    }
  });

  it('seq is strictly monotonic within each session', () => {
    for (const events of [refkernel.events, debugSession.events, lspSession.events]) {
      const seqs = events.map((e) => e.seq);
      expect(seqs).toEqual([...seqs].sort((a, b) => a - b));
      expect(new Set(seqs).size).toBe(seqs.length);
    }
  });

  it('is discoverable through the fixture loader and marked constructed', () => {
    expect(listBundledFixtures()).toEqual([
      'debug-session.ndjson',
      'lsp-session.ndjson',
      'refkernel-session.ndjson',
    ]);
    const loaded = loadBundledFixture('refkernel-session.ndjson');
    expect(loaded.violations).toEqual([]);
    // Honesty marker: this file is a P3 demo, not a P2 recording.
    expect(loaded.constructed).toBe(true);
  });
});

describe('replay: refkernel session (build → run → fault → exit)', () => {
  it('folds the whole stream without gaps', () => {
    expect(refkernelState.lastSeq).toBe(17);
    expect(refkernelState.appliedEvents).toBe(17);
    expect(refkernelState.gaps).toEqual([]);
    expect(refkernelState.needsReplay).toBe(false);
    expect(refkernelState.duplicates).toBe(0);
    expect(refkernelState.ignored).toEqual([]);
    expect(refkernelState.versionMismatches).toEqual([]);
  });

  it('produces the expected build view', () => {
    expect(refkernelState.build).toMatchObject({
      backend: 'make',
      toolchainId: 'host-clang14-nasm',
      running: false,
      status: 'ok',
      exitCode: 0,
      durationMs: 1842,
    });
    expect(refkernelState.build?.argv).toEqual(['make', '-C', 'fixtures/refkernel', 'all']);
    expect(refkernelState.build?.artifacts.map((a) => a.path)).toEqual([
      'fixtures/refkernel/build/refkernel.elf',
      'fixtures/refkernel/build/refkernel.iso',
    ]);
    expect(refkernelState.build?.artifacts[0]?.kind).toBe('elf');
  });

  it('keeps the banner and fault symbol from the acceptance spec constants', () => {
    const summary = deriveSummary(refkernelState);
    expect(summary.banner).toBe('PrincessIDE reference kernel booted\n');
    expect(summary.faultSymbol).toBe('refkernel_fault_probe');
    expect(refkernelState.run?.fault?.symbolicated).toEqual({
      symbol: 'refkernel_fault_probe',
      file: 'fixtures/refkernel/kernel.c',
      line: 42,
    });
    expect(refkernelState.run?.fault?.rip).toBe('0x0000000000100abc');
    expect(refkernelState.run?.fault?.vector).toBe('#PF');
  });

  it('attributes the exit reason without guessing', () => {
    expect(refkernelState.run?.running).toBe(false);
    expect(refkernelState.run?.exit).toEqual({ exitCode: 1, reason: 'triple-fault', uptimeMs: 1260 });
    expect(deriveSummary(refkernelState).runReason).toBe('triple-fault');
  });

  it('routes each log chunk to its own stream in order', () => {
    expect(refkernelState.logs.map((l) => l.stream)).toEqual([
      'ide',
      'build',
      'build',
      'serial.com1',
      'qemu.monitor',
      'serial.com1',
    ]);
    expect(refkernelState.logs.map((l) => l.seq)).toEqual([1, 3, 4, 10, 11, 13]);
    const serial = refkernelState.logs.filter((l) => l.stream === 'serial.com1');
    expect(serial.map((l) => l.text).join('')).toContain('PrincessIDE reference kernel booted');
    expect(serial.map((l) => l.text).join('')).toContain('PANIC: unhandled page fault');
  });

  it('preserves multi-byte UTF-8 chunks verbatim (contract §2)', () => {
    const cjk = refkernelState.logs.find((l) => l.text.includes('内核构建中'));
    expect(cjk?.encoding).toBe('utf8');
    expect(cjk?.text).toContain('内核构建中…');
    expect(cjk?.lossy).toBe(false);
  });

  it('collects diagnostics and symbol/artifact metadata', () => {
    expect(refkernelState.diagnostics).toHaveLength(1);
    expect(refkernelState.diagnostics[0]).toMatchObject({
      severity: 'warning',
      file: 'fixtures/refkernel/serial.c',
      line: 27,
      col: 5,
      source: 'clang',
    });
    expect(refkernelState.symbols).toEqual({
      artifact: 'fixtures/refkernel/build/refkernel.elf',
      buildId: 'a3f91c0d5e7b2468',
      symbolCount: 47,
    });
    expect(refkernelState.artifact?.path).toBe('fixtures/refkernel/build/refkernel.elf');
  });

  it('assembles streamed ai.chunk text and takes usage from ai.finished', () => {
    expect(refkernelState.ai['ai-1']).toEqual({
      requestId: 'ai-1',
      text:
        'The guest faulted at rip=0x100abc, which symbolication maps to refkernel_fault_probe (kernel.c:42).',
      usage: { promptTokens: 412, completionTokens: 38, totalTokens: 450 },
      finished: true,
    });
  });

  it('matches the stored snapshot (P3-4 snapshot assertion)', () => {
    expect(refkernelState).toMatchSnapshot();
  });
});

describe('replay: debug session', () => {
  it('records stops, breakpoints and debug output', () => {
    expect(debugState.debug.active).toBe(true);
    expect(debugState.debug.stops.map((s) => s.reason)).toEqual(['entry', 'breakpoint']);
    expect(debugState.debug.stops[1]?.frame).toMatchObject({
      name: 'refkernel_fault_probe',
      file: 'fixtures/refkernel/kernel.c',
      line: 42,
      pc: '0x0000000000100abc',
    });
    expect(debugState.debug.breakpoints[1]).toEqual({
      id: 1,
      verified: true,
      location: 'refkernel_fault_probe',
    });
    expect(debugState.debug.output.map((o) => o.category)).toEqual(['console', 'stdout']);
    expect(debugState.debug.output[0]?.text).toContain('line 42.');
    expect(debugState.gaps).toEqual([]);
  });

  it('matches the stored snapshot', () => {
    expect(debugState).toMatchSnapshot();
  });
});

describe('replay: lsp session (D17 amendment events)', () => {
  it('keeps header-less JSON-RPC messages per server, in order', () => {
    expect(lspState.gaps).toEqual([]);
    expect(lspState.ignored).toEqual([]);
    expect(Object.keys(lspState.lsp.servers)).toEqual(['lsp-1']);
    const server = lspState.lsp.servers['lsp-1'];
    expect(server?.messages).toHaveLength(2);

    // Messages are stored verbatim: the engine strips the frame headers, the
    // frontend must not add, reorder or paraphrase anything.
    const first = JSON.parse(server!.messages[0]!) as {
      result: { serverInfo: { name: string; version: string } };
    };
    expect(first.result.serverInfo).toEqual({ name: 'clangd', version: '16.0.6' });
    const second = JSON.parse(server!.messages[1]!) as {
      method: string;
      params: { diagnostics: { message: string }[] };
    };
    expect(second.method).toBe('textDocument/publishDiagnostics');
    expect(second.params.diagnostics[0]?.message).toBe("unused parameter 'port'");
  });

  it('records the stop reason from lsp.stopped', () => {
    expect(lspState.lsp.servers['lsp-1']?.stopped).toEqual({ reason: 'shutdown', seq: 5 });
    expect(deriveSummary(lspState).lspServers).toBe(1);
    expect(deriveSummary(lspState).lspMessages).toBe(2);
  });

  it('matches the stored snapshot', () => {
    expect(lspState).toMatchSnapshot();
  });

  it('bounds the retained messages per server', () => {
    const many = Array.from({ length: MAX_LSP_MESSAGES + 5 }, (_, i) =>
      env({ seq: i + 1, kind: 'lsp.message', payload: { serverId: 'lsp-x', message: `m${i}` } }),
    );
    const state = many.reduce(applyEvent, createInitialState());
    expect(state.lsp.servers['lsp-x']?.messages).toHaveLength(MAX_LSP_MESSAGES);
    expect(state.lsp.servers['lsp-x']?.messages.at(-1)).toBe(`m${MAX_LSP_MESSAGES + 4}`);
  });

  it('rejects a malformed lsp event instead of feeding it to the language client', () => {
    const cases: [unknown, RegExp][] = [
      [
        { v: 1, seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'lsp.message', payload: { serverId: 'lsp-1' } },
        /payload\.message: missing required field/,
      ],
      [
        { v: 1, seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'lsp.stopped', payload: { serverId: 'lsp-1', reason: 'exploded' } },
        /payload\.reason/,
      ],
    ];
    for (const [value, pattern] of cases) {
      expect(() => parseEventEnvelope(value, 'case')).toThrow(pattern);
    }
  });
});

describe('negative paths (failures must be explicit)', () => {
  it('records a seq hole and asks for a replay', () => {
    const hole = replayEvents([
      env({ seq: 1 }),
      env({ seq: 2 }),
      env({ seq: 5 }),
    ]);
    expect(hole.gaps).toEqual([{ expected: 3, received: 5, size: 2 }]);
    expect(hole.needsReplay).toBe(true);
    expect(hole.appliedEvents).toBe(3);
    expect(hole.lastSeq).toBe(5);
  });

  it('counts a duplicate instead of applying it twice', () => {
    const dup = replayEvents([env({ seq: 1 }), env({ seq: 1 })]);
    expect(dup.duplicates).toBe(1);
    expect(dup.appliedEvents).toBe(1);
    expect(dup.logs).toHaveLength(1);
    expect(dup.ignored).toEqual([{ seq: 1, kind: 'log.append', reason: 'seq not greater than lastSeq' }]);
  });

  it('counts out-of-order delivery and keeps the stream coherent', () => {
    const ooo = replayEvents([env({ seq: 2 }), env({ seq: 1 }), env({ seq: 3 })]);
    expect(ooo.outOfOrder).toBe(1);
    expect(ooo.gaps).toEqual([]);
    expect(ooo.appliedEvents).toBe(2);
    expect(ooo.lastSeq).toBe(3);
  });

  it('raises an explicit warning on an event-model version mismatch', () => {
    const mismatched = replayEvents([env({ seq: 1, v: 2 })]);
    expect(mismatched.versionMismatches).toEqual([2]);
    expect(deriveSummary(mismatched).versionWarning).toBe('event model v2 != UI v1');
  });

  it('surfaces a build.finished that never had a build.started', () => {
    const orphan = replayEvents([
      env({
        seq: 1,
        kind: 'build.finished',
        payload: { status: 'failed', exitCode: 2, durationMs: 10, artifacts: [] },
      }),
    ]);
    expect(orphan.build?.status).toBe('failed');
    expect(orphan.ignored[0]?.reason).toBe('build.finished without build.started');
  });

  it('marks utf8-lossy chunks instead of hiding them', () => {
    const lossy = replayEvents([
      env({
        seq: 1,
        kind: 'log.append',
        payload: { stream: 'serial.com1', chunk: 'bad \ufffd byte', encoding: 'utf8-lossy' },
      }),
    ]);
    expect(lossy.logs[0]?.lossy).toBe(true);
    expect(lossy.logs[0]?.encoding).toBe('utf8-lossy');
  });

  it('rejects malformed envelopes with the offending path', () => {
    const cases: [unknown, RegExp][] = [
      [{ seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'log.append', payload: {} }, /missing required envelope key/],
      [{ v: 1, seq: 0, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'log.append', payload: {} }, /seq: must be an integer/],
      [{ v: 1, seq: 1, ts: 'yesterday', opId: null, kind: 'log.append', payload: {} }, /ts: must be an ISO-8601/],
      [{ v: 1, seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'build.nope', payload: {} }, /unknown event kind/],
      [
        { v: 1, seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'log.append', payload: { stream: 'serial.com9', chunk: 'x', encoding: 'utf8' } },
        /payload\.stream: wrong type/,
      ],
      [
        { v: 1, seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'run.exited', payload: { exitCode: 1, reason: 'exploded', uptimeMs: 1 } },
        /payload\.reason/,
      ],
      [
        { v: 1, seq: 1, ts: '2026-05-05T12:00:00.000Z', opId: null, kind: 'build.finished', payload: { status: 'ok', exitCode: 0, durationMs: 1 } },
        /payload\.artifacts: missing required field/,
      ],
    ];
    for (const [value, pattern] of cases) {
      expect(() => parseEventEnvelope(value, 'case')).toThrow(ContractViolation);
      expect(() => parseEventEnvelope(value, 'case')).toThrow(pattern);
    }
  });

  it('reports invalid JSON lines instead of dropping them silently', () => {
    const broken = parseNdjson('{"v":1,"seq":1}\nnot json\n{"v":1,"seq":2,"ts":"2026-05-05T12:00:00.000Z","opId":null,"kind":"log.append","payload":{"stream":"ide","chunk":"ok","encoding":"utf8"}}\n');
    expect(broken.events).toHaveLength(1);
    expect(broken.violations).toHaveLength(2);
    expect(broken.violations[0]?.message).toMatch(/missing required envelope key/);
    expect(broken.violations[1]?.message).toMatch(/invalid JSON/);
  });

  it('tolerates comment and blank lines in NDJSON fixtures', () => {
    const withComments = parseNdjson('# a comment\n\n{"v":1,"seq":1,"ts":"2026-05-05T12:00:00.000Z","opId":null,"kind":"log.append","payload":{"stream":"ide","chunk":"x","encoding":"utf8"}}\n');
    expect(withComments.violations).toEqual([]);
    expect(withComments.events).toHaveLength(1);
  });

  it('bounds the log ring so a chatty kernel cannot exhaust memory', () => {
    const many: AnyEvent[] = [];
    for (let i = 1; i <= MAX_LOG_LINES + 10; i += 1) {
      many.push(env({ seq: i, kind: 'log.append' }));
    }
    const state = many.reduce(applyEvent, createInitialState());
    expect(state.logs).toHaveLength(MAX_LOG_LINES);
    expect(state.lastSeq).toBe(MAX_LOG_LINES + 10);
    expect(state.appliedEvents).toBe(MAX_LOG_LINES + 10);
  });
});
