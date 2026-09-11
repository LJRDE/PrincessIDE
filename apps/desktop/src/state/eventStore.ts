/**
 * Frontend event state machine (contract §2).
 *
 * Pure functions in, pure state out: the same reducer drives the live Tauri
 * event stream and the offline NDJSON replay used by tests (P3-4).  Nothing in
 * here touches the DOM, so the replay test asserts the *state*, not a rendered
 * screenshot.
 *
 * Contract rules enforced here:
 *  - `seq` is monotonic per session; a hole means dropped events → record the
 *    gap and raise `needsReplay` (the UI then asks for `princess:op:replay`).
 *  - a duplicate / out-of-order envelope is counted and ignored, never applied twice.
 *  - `v` != expected model version raises an explicit warning (contract §7).
 *  - `log.append` is append-only, no lossy transformation on the stream (§2).
 */

import { EVENT_MODEL_VERSION, type AnyEvent, type EventKind } from '../contract/events.js';
import type {
  AiUsage,
  LspStopReason,
  ArtifactRef,
  BuildDiagnosticPayload,
  DebugBreakpointChangedPayload,
  DebugOutputPayload,
  DebugStoppedPayload,
  GdbStubRef,
  LogEncoding,
  LogStream,
  RunExitReason,
  RunFaultPayload,
} from '../contract/events.js';

export interface LogLine {
  seq: number;
  ts: string;
  opId: string | null;
  stream: LogStream;
  text: string;
  encoding: LogEncoding;
  /** true when the engine flagged the chunk `utf8-lossy` (rendered with a marker). */
  lossy: boolean;
}

export interface SeqGap {
  expected: number;
  received: number;
  size: number;
}

export interface BuildView {
  backend: string;
  toolchainId: string;
  argv: string[];
  cwd: string;
  startedAt: string;
  running: boolean;
  status: 'ok' | 'failed' | 'cancelled' | null;
  exitCode: number | null;
  durationMs: number | null;
  artifacts: ArtifactRef[];
}

export interface RunView {
  qemuArgv: string[];
  gdbStub: GdbStubRef | null;
  startedAt: string;
  running: boolean;
  fault: RunFaultPayload | null;
  exit: { exitCode: number | null; reason: RunExitReason; uptimeMs: number } | null;
}

export interface DebugStop extends DebugStoppedPayload {
  seq: number;
}

export interface DebugView {
  active: boolean;
  stops: DebugStop[];
  breakpoints: Record<number, DebugBreakpointChangedPayload>;
  output: (DebugOutputPayload & { seq: number })[];
}

export interface SymbolsView {
  artifact: string;
  buildId: string;
  symbolCount: number;
}

export interface ArtifactView {
  path: string;
  kind: string;
  seq: number;
}

/**
 * LSP state (contract §2 `lsp.message` / `lsp.stopped`).
 *
 * `messages` holds the **header-less JSON-RPC text** exactly as the engine sent
 * it: the frontend never re-frames it, it is handed straight to the language
 * client through `src/lsp/client.ts`.
 */
export interface LspServerView {
  serverId: string;
  messages: string[];
  stopped: { reason: LspStopReason; seq: number } | null;
}

export interface LspView {
  servers: Record<string, LspServerView>;
}

export interface AiView {
  requestId: string;
  text: string;
  usage: AiUsage | null;
  finished: boolean;
}

export interface IdeState {
  /** Event-model version this build of the UI speaks. */
  modelVersion: number;
  lastSeq: number;
  appliedEvents: number;
  gaps: SeqGap[];
  duplicates: number;
  outOfOrder: number;
  /** Distinct `v` values seen that differ from `modelVersion` (contract §7). */
  versionMismatches: number[];
  needsReplay: boolean;
  logs: LogLine[];
  logBytes: number;
  build: BuildView | null;
  diagnostics: BuildDiagnosticPayload[];
  run: RunView | null;
  debug: DebugView;
  symbols: SymbolsView | null;
  artifact: ArtifactView | null;
  ai: Record<string, AiView>;
  lsp: LspView;
  /** Envelopes that were dropped on purpose, with the reason (auditable). */
  ignored: { seq: number; kind: EventKind; reason: string }[];
}

/** Cap the in-memory log ring so a chatty kernel can't OOM the UI. */
export const MAX_LOG_LINES = 5000;
/** Cap retained diagnostics / stops for the same reason. */
export const MAX_DIAGNOSTICS = 1000;
/** Cap retained LSP messages per server: a busy language server is chatty. */
export const MAX_LSP_MESSAGES = 500;

export function createInitialState(): IdeState {
  return {
    modelVersion: EVENT_MODEL_VERSION,
    lastSeq: 0,
    appliedEvents: 0,
    gaps: [],
    duplicates: 0,
    outOfOrder: 0,
    versionMismatches: [],
    needsReplay: false,
    logs: [],
    logBytes: 0,
    build: null,
    diagnostics: [],
    run: null,
    debug: { active: false, stops: [], breakpoints: {}, output: [] },
    symbols: null,
    artifact: null,
    ai: {},
    lsp: { servers: {} },
    ignored: [],
  };
}

/** Structural clone of a state slice we are about to mutate. */
function clone(state: IdeState): IdeState {
  return {
    ...state,
    gaps: [...state.gaps],
    versionMismatches: [...state.versionMismatches],
    logs: [...state.logs],
    diagnostics: [...state.diagnostics],
    ignored: [...state.ignored],
    debug: {
      ...state.debug,
      stops: [...state.debug.stops],
      breakpoints: { ...state.debug.breakpoints },
      output: [...state.debug.output],
    },
    ai: { ...state.ai },
    lsp: { servers: { ...state.lsp.servers } },
  };
}

/**
 * Apply one envelope.  Returns a new state; the input state is not mutated.
 * @param strictSeq when true (default), a `seq` hole raises `needsReplay`.
 */
export function applyEvent(state: IdeState, env: AnyEvent): IdeState {
  const next = clone(state);

  if (env.v !== next.modelVersion && !next.versionMismatches.includes(env.v)) {
    // Contract §7: a version mismatch must be visible, not silently tolerated.
    next.versionMismatches.push(env.v);
  }

  if (env.seq <= next.lastSeq) {
    if (env.seq === next.lastSeq) next.duplicates += 1;
    else next.outOfOrder += 1;
    next.ignored.push({ seq: env.seq, kind: env.kind, reason: 'seq not greater than lastSeq' });
    return next;
  }

  if (state.lastSeq !== 0 && env.seq !== state.lastSeq + 1) {
    next.gaps.push({ expected: state.lastSeq + 1, received: env.seq, size: env.seq - state.lastSeq - 1 });
    next.needsReplay = true;
  }

  next.lastSeq = env.seq;
  next.appliedEvents += 1;

  switch (env.kind) {
    case 'log.append': {
      const p = env.payload;
      const line: LogLine = {
        seq: env.seq,
        ts: env.ts,
        opId: env.opId,
        stream: p.stream,
        text: p.chunk,
        encoding: p.encoding,
        lossy: p.encoding === 'utf8-lossy',
      };
      next.logs.push(line);
      next.logBytes += p.chunk.length;
      if (next.logs.length > MAX_LOG_LINES) {
        const dropped = next.logs.length - MAX_LOG_LINES;
        next.logs.splice(0, dropped);
      }
      break;
    }
    case 'build.started': {
      const p = env.payload;
      next.build = {
        backend: p.backend,
        toolchainId: p.toolchainId,
        argv: p.argv,
        cwd: p.cwd,
        startedAt: env.ts,
        running: true,
        status: null,
        exitCode: null,
        durationMs: null,
        artifacts: [],
      };
      break;
    }
    case 'build.diagnostic': {
      next.diagnostics.push(env.payload);
      if (next.diagnostics.length > MAX_DIAGNOSTICS) {
        next.diagnostics.splice(0, next.diagnostics.length - MAX_DIAGNOSTICS);
      }
      break;
    }
    case 'build.finished': {
      const p = env.payload;
      if (!next.build) {
        // A `*.finished` without `*.started` still must be visible (§6.5).
        next.build = {
          backend: '(unknown)',
          toolchainId: '(unknown)',
          argv: [],
          cwd: '',
          startedAt: env.ts,
          running: false,
          status: p.status,
          exitCode: p.exitCode,
          durationMs: p.durationMs,
          artifacts: p.artifacts,
        };
        next.ignored.push({ seq: env.seq, kind: env.kind, reason: 'build.finished without build.started' });
      } else {
        next.build = {
          ...next.build,
          running: false,
          status: p.status,
          exitCode: p.exitCode,
          durationMs: p.durationMs,
          artifacts: p.artifacts,
        };
      }
      break;
    }
    case 'run.started': {
      const p = env.payload;
      next.run = {
        qemuArgv: p.qemuArgv,
        gdbStub: p.gdbStub,
        startedAt: env.ts,
        running: true,
        fault: null,
        exit: null,
      };
      break;
    }
    case 'run.fault': {
      const p = env.payload;
      if (next.run) next.run = { ...next.run, fault: p };
      else next.run = { qemuArgv: [], gdbStub: null, startedAt: env.ts, running: true, fault: p, exit: null };
      break;
    }
    case 'run.exited': {
      const p = env.payload;
      if (next.run) next.run = { ...next.run, running: false, exit: p };
      else {
        next.run = { qemuArgv: [], gdbStub: null, startedAt: env.ts, running: false, fault: null, exit: p };
      }
      break;
    }
    case 'debug.stopped': {
      next.debug.active = true;
      next.debug.stops.push({ ...env.payload, seq: env.seq });
      break;
    }
    case 'debug.breakpoint.changed': {
      next.debug.breakpoints[env.payload.id] = env.payload;
      break;
    }
    case 'debug.output': {
      next.debug.output.push({ ...env.payload, seq: env.seq });
      break;
    }
    case 'symbols.indexed': {
      next.symbols = env.payload;
      break;
    }
    case 'artifact.changed': {
      next.artifact = { ...env.payload, seq: env.seq };
      break;
    }
    case 'ai.chunk': {
      const p = env.payload;
      const prev = next.ai[p.requestId];
      next.ai[p.requestId] = {
        requestId: p.requestId,
        text: (prev?.text ?? '') + p.text,
        usage: prev?.usage ?? null,
        finished: prev?.finished ?? false,
      };
      break;
    }
    case 'lsp.message': {
      const p = env.payload;
      const prev = next.lsp.servers[p.serverId] ?? { serverId: p.serverId, messages: [], stopped: null };
      const messages = [...prev.messages, p.message];
      if (messages.length > MAX_LSP_MESSAGES) {
        messages.splice(0, messages.length - MAX_LSP_MESSAGES);
      }
      next.lsp.servers[p.serverId] = { ...prev, messages };
      break;
    }
    case 'lsp.stopped': {
      const p = env.payload;
      const prev = next.lsp.servers[p.serverId] ?? { serverId: p.serverId, messages: [], stopped: null };
      next.lsp.servers[p.serverId] = { ...prev, stopped: { reason: p.reason, seq: env.seq } };
      break;
    }
    case 'ai.finished': {
      const p = env.payload;
      const prev = next.ai[p.requestId];
      next.ai[p.requestId] = {
        requestId: p.requestId,
        text: prev?.text ?? '',
        usage: p.usage,
        finished: true,
      };
      break;
    }
    default: {
      // Exhaustiveness guard: `env` is `never` here, so adding a kind to the
      // contract without handling it below becomes a compile error.  At runtime
      // (an envelope that skipped validation) the event is recorded as ignored
      // instead of crashing the UI.
      const unknown = env as unknown as { seq: number; kind: string };
      next.ignored.push({
        seq: unknown.seq,
        kind: unknown.kind as EventKind,
        reason: `unhandled kind ${unknown.kind}`,
      });
    }
  }

  return next;
}

/** Fold a whole stream (replay / fixture load). */
export function replayEvents(events: readonly AnyEvent[], initial = createInitialState()): IdeState {
  return events.reduce(applyEvent, initial);
}

/** Derived view state consumed by the UI panels. */
export function deriveSummary(state: IdeState) {
  return {
    lastSeq: state.lastSeq,
    events: state.appliedEvents,
    needsReplay: state.needsReplay,
    gaps: state.gaps.length,
    duplicates: state.duplicates,
    versionWarning:
      state.versionMismatches.length > 0
        ? `event model v${state.versionMismatches.join(', v')} != UI v${state.modelVersion}`
        : null,
    buildStatus: state.build?.status ?? (state.build ? 'running' : null),
    runReason: state.run?.exit?.reason ?? (state.run ? 'running' : null),
    faultSymbol: state.run?.fault?.symbolicated?.symbol ?? null,
    lspServers: Object.keys(state.lsp.servers).length,
    lspMessages: Object.values(state.lsp.servers).reduce((n, s) => n + s.messages.length, 0),
    banner:
      state.logs.find((l) => l.stream === 'serial.com1' && l.text.includes('PrincessIDE reference kernel booted'))
        ?.text ?? null,
  };
}
