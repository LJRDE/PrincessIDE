/**
 * Runtime validation for the frozen event contract (docs/spec/10-contracts.md §2).
 *
 * The TypeScript types in `events.ts` are erased at runtime, so the NDJSON
 * loader validates every envelope before it reaches the state machine.  A
 * malformed line is *rejected and reported*, never silently dropped or coerced
 * (contract §0.4: failures are explicit).
 *
 * This validator is also the P3-4 / P3-5 evidence: replaying a fixture through
 * it proves the fixture really is contract-shaped, not just JSON-shaped.
 */

import {
  EVENT_KINDS,
  type AnyEvent,
  type EventKind,
  type EventPayloadMap,
} from './events.js';

export class ContractViolation extends Error {
  readonly path: string;
  constructor(path: string, message: string) {
    super(`${path}: ${message}`);
    this.name = 'ContractViolation';
    this.path = path;
  }
}

const ISO_TS = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

type FieldCheck = (value: unknown) => boolean;

const isString: FieldCheck = (v) => typeof v === 'string';
const isNumber: FieldCheck = (v) => typeof v === 'number' && Number.isFinite(v);
const isBoolean: FieldCheck = (v) => typeof v === 'boolean';
const isObject: FieldCheck = (v) => typeof v === 'object' && v !== null && !Array.isArray(v);
const isStringArray: FieldCheck = (v) => Array.isArray(v) && v.every(isString);
const isObjectOrNull: FieldCheck = (v) => v === null || isObject(v);

function isEnum(allowed: readonly string[]): FieldCheck {
  return (v) => typeof v === 'string' && allowed.includes(v);
}

function isArrayOf(check: FieldCheck): FieldCheck {
  return (v) => Array.isArray(v) && v.every(check);
}

interface PayloadSchema {
  /** key → check.  keys are required verbatim names from the contract. */
  required: Record<string, FieldCheck>;
  /** key → check; present-or-absent (contract marks these with `?`). */
  optional?: Record<string, FieldCheck>;
}

const ARTIFACT_REF: FieldCheck = (v) =>
  isObject(v) &&
  isString((v as Record<string, unknown>).path) &&
  isString((v as Record<string, unknown>).kind) &&
  isNumber((v as Record<string, unknown>).size) &&
  isString((v as Record<string, unknown>).sha256);

const REGS: FieldCheck = (v) =>
  isObject(v) && Object.values(v as Record<string, unknown>).every(isString);

const FRAME: FieldCheck = (v) =>
  isObject(v) &&
  isNumber((v as Record<string, unknown>).id) &&
  isString((v as Record<string, unknown>).name) &&
  isString((v as Record<string, unknown>).file) &&
  isNumber((v as Record<string, unknown>).line) &&
  isString((v as Record<string, unknown>).pc);

const SYMBOLICATED: FieldCheck = (v) =>
  isObject(v) &&
  isString((v as Record<string, unknown>).symbol) &&
  isString((v as Record<string, unknown>).file) &&
  isNumber((v as Record<string, unknown>).line);

const USAGE: FieldCheck = (v) =>
  isObject(v) &&
  isNumber((v as Record<string, unknown>).promptTokens) &&
  isNumber((v as Record<string, unknown>).completionTokens) &&
  isNumber((v as Record<string, unknown>).totalTokens);

/**
 * Payload field names below are copied verbatim from the contract table in §2.
 * `severity`, `vector` and `rip` deliberately use loose checks where the
 * contract does not freeze the value set.
 */
export const PAYLOAD_SCHEMAS: Record<EventKind, PayloadSchema> = {
  'log.append': {
    required: {
      stream: isEnum(['build', 'serial.com1', 'qemu.monitor', 'gdb.console', 'ide']),
      chunk: isString,
      encoding: isEnum(['utf8', 'utf8-lossy']),
    },
  },
  'build.started': {
    required: { backend: isString, toolchainId: isString, argv: isStringArray, cwd: isString },
  },
  'build.diagnostic': {
    required: {
      severity: isString,
      file: isString,
      line: isNumber,
      col: isNumber,
      message: isString,
      source: isEnum(['clang', 'ld', 'nasm']),
    },
  },
  'build.finished': {
    required: {
      status: isEnum(['ok', 'failed', 'cancelled']),
      exitCode: isNumber,
      durationMs: isNumber,
      artifacts: isArrayOf(ARTIFACT_REF),
    },
  },
  'run.started': {
    required: { qemuArgv: isStringArray, gdbStub: isObjectOrNull },
  },
  'run.fault': {
    required: { vector: isString, rip: isString, errorCode: isString, regs: REGS },
    optional: { symbolicated: SYMBOLICATED },
  },
  'run.exited': {
    required: {
      exitCode: isNumber,
      reason: isEnum(['guest-shutdown', 'triple-fault', 'timeout', 'killed']),
      uptimeMs: isNumber,
    },
  },
  'debug.stopped': {
    required: {
      reason: isEnum(['breakpoint', 'step', 'signal', 'entry']),
      threadId: isNumber,
      frame: FRAME,
      regs: REGS,
    },
  },
  'debug.breakpoint.changed': {
    required: { id: isNumber, verified: isBoolean, location: isString },
  },
  'debug.output': {
    required: { category: isEnum(['console', 'stdout', 'stderr']), text: isString },
  },
  'symbols.indexed': {
    required: { artifact: isString, buildId: isString, symbolCount: isNumber },
  },
  'artifact.changed': {
    required: { path: isString, kind: isString },
  },
  'ai.chunk': {
    required: { requestId: isString, text: isString },
  },
  'ai.finished': {
    required: { requestId: isString, usage: USAGE },
  },
  'lsp.message': {
    required: { serverId: isString, message: isString },
  },
  'lsp.stopped': {
    required: { serverId: isString, reason: isEnum(['shutdown', 'crashed', 'killed']) },
  },
};

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Validate one envelope.  Throws ContractViolation with a JSON-path-ish prefix.
 * @param where human-readable origin (line number in an NDJSON file, etc.)
 */
export function parseEventEnvelope(value: unknown, where = 'event'): AnyEvent {
  if (!isPlainObject(value)) throw new ContractViolation(where, 'envelope is not a JSON object');

  for (const key of ['v', 'seq', 'ts', 'opId', 'kind', 'payload'] as const) {
    if (!(key in value)) throw new ContractViolation(`${where}.${key}`, 'missing required envelope key');
  }

  if (!isNumber(value.v)) throw new ContractViolation(`${where}.v`, 'must be a number');
  if (!Number.isInteger(value.seq) || (value.seq as number) < 1) {
    throw new ContractViolation(`${where}.seq`, 'must be an integer >= 1');
  }
  if (!isString(value.ts) || !ISO_TS.test(value.ts as string)) {
    throw new ContractViolation(`${where}.ts`, 'must be an ISO-8601 UTC timestamp string');
  }
  if (!(value.opId === null || isString(value.opId))) {
    throw new ContractViolation(`${where}.opId`, 'must be a string or null');
  }
  if (!isString(value.kind) || !EVENT_KINDS.includes(value.kind as EventKind)) {
    throw new ContractViolation(`${where}.kind`, `unknown event kind ${JSON.stringify(value.kind)}`);
  }
  if (!isPlainObject(value.payload)) {
    throw new ContractViolation(`${where}.payload`, 'must be an object');
  }

  const kind = value.kind as EventKind;
  const schema = PAYLOAD_SCHEMAS[kind];
  const payload = value.payload;

  for (const [key, check] of Object.entries(schema.required)) {
    if (!(key in payload)) {
      throw new ContractViolation(`${where}.payload.${key}`, `missing required field for kind ${kind}`);
    }
    if (!check(payload[key])) {
      throw new ContractViolation(
        `${where}.payload.${key}`,
        `wrong type for kind ${kind}: ${JSON.stringify(payload[key])}`,
      );
    }
  }
  for (const [key, check] of Object.entries(schema.optional ?? {})) {
    if (key in payload && payload[key] !== undefined && !check(payload[key])) {
      throw new ContractViolation(`${where}.payload.${key}`, `wrong type for kind ${kind}`);
    }
  }

  return value as unknown as AnyEvent;
}

export interface NdjsonParseResult {
  events: AnyEvent[];
  /** One entry per rejected line; empty means the whole stream is contract-clean. */
  violations: { line: number; message: string }[];
}

/** Parse an NDJSON event stream (one JSON envelope per line, `#` comments allowed). */
export function parseNdjson(text: string, origin = 'ndjson'): NdjsonParseResult {
  const events: AnyEvent[] = [];
  const violations: { line: number; message: string }[] = [];

  text.split(/\r?\n/).forEach((raw, index) => {
    const line = raw.trim();
    if (line === '' || line.startsWith('#')) return;
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch (err) {
      violations.push({ line: index + 1, message: `${origin}:${index + 1}: invalid JSON (${String(err)})` });
      return;
    }
    try {
      events.push(parseEventEnvelope(parsed, `${origin}:${index + 1}`));
    } catch (err) {
      violations.push({ line: index + 1, message: err instanceof Error ? err.message : String(err) });
    }
  });

  return { events, violations };
}

/** Type-level helper: the payload type for a kind, used by the store's switch. */
export type PayloadOf<K extends EventKind> = EventPayloadMap[K];
