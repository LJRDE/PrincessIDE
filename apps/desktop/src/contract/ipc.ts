/**
 * Frozen IPC contract — frontend mirror of `docs/spec/10-contracts.md` §3.
 *
 * Naming: `princess:<domain>:<action>`, domain ∈
 * project | build | run | debug | symbols | bin | fs | tools | ai | op.
 *
 * Unified return (contract §3):
 *   { "ok": true,  "data": { } }
 *   { "ok": false, "error": { "code": "E_BUILD_FAILED", "message": "…", "detail": "原始 stderr" } }
 *
 * `IPC_COMMANDS` is the *whole* v1 minimal set from §3 — including commands whose
 * engine-side handler lands in a later phase.  Keeping the full set here is what
 * makes P3-5 (contract alignment) a machine check instead of an opinion:
 * scripts/check-contract.mjs diffs this list against the spec and against the
 * Rust registry, and fails on any drift in either direction.
 */

export const IPC_COMMANDS = [
  // §3 project
  'princess:project:open',
  'princess:project:validate',
  // §3 tools
  'princess:tools:detect',
  // §3 build
  'princess:build:start',
  'princess:build:cancel',
  // §3 run
  'princess:run:start',
  'princess:run:stop',
  // §3 debug (the spec writes `princess:debug:attach` / `setBreakpoints` / … —
  // the shorthand expands to one `princess:debug:<action>` command each)
  'princess:debug:attach',
  'princess:debug:setBreakpoints',
  'princess:debug:continue',
  'princess:debug:stepOver',
  'princess:debug:stepInto',
  'princess:debug:stackTrace',
  'princess:debug:scopes',
  'princess:debug:variables',
  'princess:debug:readMemory',
  'princess:debug:writeMemory',
  'princess:debug:disassemble',
  'princess:debug:registers',
  // §3 op
  'princess:op:cancel',
  'princess:op:replay',
  // §3 lsp (D17 amendment: engine owns the clangd process and the frame
  // headers; the frontend only drives the editor's language-service features)
  'princess:lsp:start',
  'princess:lsp:send',
  'princess:lsp:stop',
] as const;

export type IpcCommand = (typeof IPC_COMMANDS)[number];

/**
 * Error-code closed set (contract §3).  Adding a code requires changing the
 * contract first — `tests/contract.test.ts` asserts this array equals the set
 * printed in the spec.
 */
export const ERROR_CODES = [
  'E_TOOLCHAIN_MISSING',
  'E_BUILD_FAILED',
  'E_QEMU_FAILED',
  'E_TIMEOUT',
  'E_NOT_FOUND',
  'E_INVALID_CONFIG',
  'E_SANDBOX_DENIED',
  'E_CANCELLED',
  'E_AI_UNAVAILABLE',
  'E_INTERNAL',
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export interface IpcError {
  code: ErrorCode;
  message: string;
  /** Raw stderr / underlying cause, verbatim (contract §0.4: failures are explicit). */
  detail: string;
}

export type IpcResult<T> = { ok: true; data: T } | { ok: false; error: IpcError };

/** Narrowing helper usable in tests and UI code. */
export function isOk<T>(r: IpcResult<T>): r is { ok: true; data: T } {
  return r.ok === true;
}

// --------------------------------------------------------------------------
// Payloads for the commands the P3 shell actually calls.
// --------------------------------------------------------------------------

/**
 * One row of `princess:tools:detect` (contract §3: "工具链表（路径 + 版本 + 是否可用）").
 *
 * `scripts/doctor.sh` prints more than path/version/availability, and P3 keeps
 * it rather than flattening it:
 *   - `status` is the raw status column (`ok` | `MISSING` | `WRONG VER`),
 *   - `checks` carries verification notes printed instead of a version
 *     (`version matches /clangd version 16\./`).
 * An empty `checks` array is the normal case.
 */
export interface ToolInfo {
  /** Tool name as printed by scripts/doctor.sh, e.g. `qemu-system-x86_64` or `clangd-16 (LSP)`. */
  name: string;
  /** Raw status column, verbatim. */
  status: string;
  /** First line of the tool's version output; null when doctor printed none. */
  version: string | null;
  /** Absolute path doctor.sh printed; null when it printed none (not always "missing"). */
  path: string | null;
  /** true only when `status === 'ok'`. */
  available: boolean;
  /** true when doctor.sh treats the tool as required. */
  required: boolean;
  /** Extra verification notes for this tool, verbatim from doctor.sh. */
  checks: string[];
}

export interface ToolsDetectData {
  tools: ToolInfo[];
  /** Exit code of scripts/doctor.sh (0 = all required tools present). */
  exitCode: number;
  /** Names doctor.sh reported as missing. */
  missing: string[];
  /** Workspace root the detection ran against. */
  root: string;
  /** The exact command line that was executed (reproducibility, contract §0.3). */
  command: string;
  /** Raw doctor.sh stdout, verbatim — never paraphrase the engine's evidence. */
  rawStdout: string;
  /** Raw doctor.sh stderr, verbatim (empty string when there was none). */
  rawStderr: string;
}

export interface OpCancelArgs {
  opId: string;
}

export interface OpCancelData {
  opId: string;
  /** true when a live operation was actually asked to die. */
  cancelled: boolean;
  /** Whether this call found a live operation registered under `opId`. */
  found: boolean;
}

export interface OpReplayArgs {
  fromSeq: number;
}

export interface OpReplayData {
  /** Replayed envelopes, `seq` ascending, starting at the first event with seq >= fromSeq. */
  events: unknown[];
  fromSeq: number;
  /** Highest seq currently held by the engine's ring buffer. */
  lastSeq: number;
}

export interface LspStartArgs {
  projectRoot: string;
}

export interface LspStartData {
  serverId: string;
  /** The exact server command line the engine launched (D18: clangd-16). */
  command: string;
  args: string[];
}

export interface LspSendArgs {
  serverId: string;
  /** JSON-RPC text WITHOUT frame headers — the engine frames it (`Content-Length`). */
  message: string;
}

export interface LspStopArgs {
  serverId: string;
}

/** Per-command argument/result typing for the commands P3 exposes end-to-end. */
export interface IpcMap {
  'princess:tools:detect': { args: Record<string, never>; data: ToolsDetectData };
  'princess:op:cancel': { args: OpCancelArgs; data: OpCancelData };
  'princess:op:replay': { args: OpReplayArgs; data: OpReplayData };
  'princess:lsp:start': { args: LspStartArgs; data: LspStartData };
  'princess:lsp:send': { args: LspSendArgs; data: Record<string, never> };
  'princess:lsp:stop': { args: LspStopArgs; data: Record<string, never> };
}
