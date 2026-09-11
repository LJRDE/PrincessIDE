/**
 * Frozen event-stream contract — frontend mirror of `docs/spec/10-contracts.md` §2.
 *
 * RULE FOR EVERYONE EDITING THIS FILE
 * ----------------------------------
 * Field names here are copied **verbatim** from the contract.  Do not rename,
 * do not "improve" casing, do not add a translation layer.  If the contract
 * changes, this file changes in the same commit, and `pnpm check:contract`
 * (tests/contract.test.ts) must still pass.
 *
 * Envelope (contract §2):
 *   { "v": 1, "seq": 12345, "ts": "...", "opId": "op-7f3a", "kind": "...", "payload": {} }
 */

/** Event-model version carried in the envelope's `v` field (contract §2, §7). */
export const EVENT_MODEL_VERSION = 1;

/** `log.append.payload.stream` (contract §2) — closed set. */
export type LogStream = 'build' | 'serial.com1' | 'qemu.monitor' | 'gdb.console' | 'ide';
export const LOG_STREAMS: readonly LogStream[] = [
  'build',
  'serial.com1',
  'qemu.monitor',
  'gdb.console',
  'ide',
];

/**
 * `log.append.payload.encoding` (contract §2).  The engine guarantees it never
 * splits a UTF-8 code point; illegal bytes are replaced and flagged
 * `utf8-lossy`.  The UI renders such chunks verbatim and marks them.
 */
export type LogEncoding = 'utf8' | 'utf8-lossy';

export interface LogAppendPayload {
  stream: LogStream;
  chunk: string;
  encoding: LogEncoding;
}

export interface BuildStartedPayload {
  backend: string;
  toolchainId: string;
  argv: string[];
  cwd: string;
}

/**
 * The contract fixes the payload *key* (`severity`), not the enum.  The union
 * below documents the values we know about while still accepting anything the
 * engine emits (so an unknown severity degrades to "render as-is", never to a
 * type error or a silent drop).
 */
export type DiagnosticSeverity = 'error' | 'warning' | 'note' | 'info' | (string & {});

/** `build.diagnostic.payload.source` (contract §2) — closed set. */
export type DiagnosticSource = 'clang' | 'gcc' | 'ld' | 'nasm';

export interface BuildDiagnosticPayload {
  severity: DiagnosticSeverity;
  file: string;
  line: number;
  col: number;
  message: string;
  source: DiagnosticSource;
}

export interface ArtifactRef {
  path: string;
  kind: string;
  size: number;
  sha256: string;
}

/** `build.finished.payload.status` (contract §2) — closed set. */
export type BuildStatus = 'ok' | 'failed' | 'cancelled';

export interface BuildFinishedPayload {
  status: BuildStatus;
  exitCode: number;
  durationMs: number;
  artifacts: ArtifactRef[];
}

export interface GdbStubRef {
  host: string;
  port: number;
  mode: string;
}

export interface RunStartedPayload {
  qemuArgv: string[];
  gdbStub: GdbStubRef | null;
}

/** x86_64 exception vectors, e.g. `#DE` `#UD` `#PF` (contract §2). */
export type FaultVector = '#DE' | '#UD' | '#PF' | (string & {});

export interface SymbolicatedLocation {
  symbol: string;
  file: string;
  line: number;
}

export interface RunFaultPayload {
  vector: FaultVector;
  rip: string;
  errorCode: string;
  regs: Record<string, string>;
  /** Optional: absent when symbolization failed (contract §2 marks it `?`). */
  symbolicated?: SymbolicatedLocation;
}

/** `run.exited.payload.reason` (contract §2) — closed set; the engine attributes it, the UI never guesses. */
export type RunExitReason = 'guest-shutdown' | 'triple-fault' | 'timeout' | 'killed';

export interface RunExitedPayload {
  exitCode: number | null;
  reason: RunExitReason;
  uptimeMs: number;
}

/** `debug.stopped.payload.reason` (contract §2) — closed set. */
export type DebugStopReason = 'breakpoint' | 'step' | 'signal' | 'entry';

export interface DebugFrame {
  id: number;
  name: string;
  file: string;
  line: number;
  pc: string;
}

export interface DebugStoppedPayload {
  reason: DebugStopReason;
  threadId: number;
  frame: DebugFrame;
  regs: Record<string, string>;
}

export interface DebugBreakpointChangedPayload {
  id: number;
  verified: boolean;
  location: string;
}

/** `debug.output.payload.category` (contract §2) — closed set. */
export type DebugOutputCategory = 'console' | 'stdout' | 'stderr';

export interface DebugOutputPayload {
  category: DebugOutputCategory;
  text: string;
}

export interface SymbolsIndexedPayload {
  artifact: string;
  buildId: string;
  symbolCount: number;
}

export interface ArtifactChangedPayload {
  path: string;
  kind: string;
}

export interface AiChunkPayload {
  requestId: string;
  text: string;
}

export interface AiUsage {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
}

export interface AiFinishedPayload {
  requestId: string;
  usage: AiUsage;
}

/**
 * `lsp.message` (contract §2, added by the D17 amendment): the language server's
 * reply, as **JSON-RPC text with the frame headers already stripped** by the
 * engine.  This is the only channel LSP replies travel on — no second transport.
 */
export interface LspMessagePayload {
  serverId: string;
  message: string;
}

/** `lsp.stopped.payload.reason` (contract §2) — closed set. */
export type LspStopReason = 'shutdown' | 'crashed' | 'killed';

export interface LspStoppedPayload {
  serverId: string;
  reason: LspStopReason;
}

/** kind → payload map. Keys are the contract's `kind` strings, verbatim. */
export interface EventPayloadMap {
  'log.append': LogAppendPayload;
  'build.started': BuildStartedPayload;
  'build.diagnostic': BuildDiagnosticPayload;
  'build.finished': BuildFinishedPayload;
  'run.started': RunStartedPayload;
  'run.fault': RunFaultPayload;
  'run.exited': RunExitedPayload;
  'debug.stopped': DebugStoppedPayload;
  'debug.breakpoint.changed': DebugBreakpointChangedPayload;
  'debug.output': DebugOutputPayload;
  'symbols.indexed': SymbolsIndexedPayload;
  'artifact.changed': ArtifactChangedPayload;
  'ai.chunk': AiChunkPayload;
  'ai.finished': AiFinishedPayload;
  'lsp.message': LspMessagePayload;
  'lsp.stopped': LspStoppedPayload;
}

export type EventKind = keyof EventPayloadMap;

export const EVENT_KINDS: readonly EventKind[] = [
  'log.append',
  'build.started',
  'build.diagnostic',
  'build.finished',
  'run.started',
  'run.fault',
  'run.exited',
  'debug.stopped',
  'debug.breakpoint.changed',
  'debug.output',
  'symbols.indexed',
  'artifact.changed',
  'ai.chunk',
  'ai.finished',
  'lsp.message',
  'lsp.stopped',
];

/** The single envelope shape; `kind` discriminates `payload` (contract §2). */
export interface EventEnvelope<K extends EventKind = EventKind> {
  v: number;
  seq: number;
  ts: string;
  /** null when the event is not attached to a long-running operation. */
  opId: string | null;
  kind: K;
  payload: EventPayloadMap[K];
}

/** A well-typed union of every concrete envelope. */
export type AnyEvent = { [K in EventKind]: EventEnvelope<K> }[EventKind];

/** Distributive narrowing helper: `narrowEvent(env, 'run.fault')`. */
export function isEvent<K extends EventKind>(env: EventEnvelope, kind: K): env is EventEnvelope<K> {
  return env.kind === kind;
}
