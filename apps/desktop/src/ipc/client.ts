/**
 * IPC client — the single place the UI talks to the Rust engine.
 *
 * Command names come from the frozen contract (`src/contract/ipc.ts`), never
 * from string literals scattered through components, so `pnpm check:contract`
 * can prove alignment with docs/spec/10-contracts.md §3.
 *
 * Wire shape:
 *   invoke('princess_invoke', { cmd: 'princess:tools:detect', args: {...} })
 *   → { ok: true, data } | { ok: false, error: { code, message, detail } }
 *
 * Why one Tauri command instead of one per contract name: Tauri derives the IPC
 * name from the Rust function identifier, and an identifier cannot contain `:`.
 * `princess:<domain>:<action>` is the frozen contract, so the shell registers
 * `princess_invoke` as a *dispatcher* that receives the contract name verbatim
 * and routes it to a real handler.  The Rust registry is asserted to contain
 * exactly the same set of names as this file (tests/contract.test.ts).
 */

import { invoke } from '@tauri-apps/api/core';
import type { IpcError, IpcResult } from '../contract/ipc.js';

/** Native Tauri command that carries contract-named IPC calls. */
export const NATIVE_DISPATCH_COMMAND = 'princess_invoke';

export interface InvokeFn {
  (cmd: string, args?: Record<string, unknown>): Promise<unknown>;
}

/** True when running inside the Tauri webview (as opposed to a plain browser). */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/**
 * Normalise anything thrown by the transport into the contract's error shape.
 * Failures are explicit and keep the original text (contract §0.4).
 */
export function toIpcError(err: unknown): IpcError {
  if (typeof err === 'object' && err !== null && 'code' in err && 'message' in err) {
    const e = err as { code: unknown; message: unknown; detail?: unknown };
    return {
      code: typeof e.code === 'string' ? (e.code as IpcError['code']) : 'E_INTERNAL',
      message: String(e.message),
      detail: e.detail === undefined ? '' : String(e.detail),
    };
  }
  return {
    code: 'E_INTERNAL',
    message: err instanceof Error ? err.message : String(err),
    detail: err instanceof Error ? (err.stack ?? '') : '',
  };
}

/**
 * Call a contract command.  Never throws for engine-reported failures — the
 * result carries them — but a broken transport becomes an explicit
 * `{ ok:false, error }` too, so the UI always has something truthful to show.
 */
export async function call<I extends keyof import('../contract/ipc.js').IpcMap>(
  cmd: I,
  args?: import('../contract/ipc.js').IpcMap[I]['args'],
  invokeFn: InvokeFn = invoke as unknown as InvokeFn,
): Promise<IpcResult<import('../contract/ipc.js').IpcMap[I]['data']>> {
  try {
    const raw = await invokeFn(NATIVE_DISPATCH_COMMAND, {
      cmd,
      args: (args ?? {}) as Record<string, unknown>,
    });
    return raw as IpcResult<import('../contract/ipc.js').IpcMap[I]['data']>;
  } catch (err) {
    return { ok: false, error: toIpcError(err) };
  }
}

/** Convenience wrappers for the commands the P3 shell actually drives. */
export const toolsDetect = (invokeFn?: InvokeFn) => call('princess:tools:detect', undefined, invokeFn);
export const opCancel = (opId: string, invokeFn?: InvokeFn) =>
  call('princess:op:cancel', { opId }, invokeFn);
export const opReplay = (fromSeq: number, invokeFn?: InvokeFn) =>
  call('princess:op:replay', { fromSeq }, invokeFn);

// P3-C: build commands
export const buildStart = (args?: { projectRoot?: string; targets?: string[] }, invokeFn?: InvokeFn) =>
  call('princess:build:start', args ?? {}, invokeFn);
export const buildCancel = (opId: string, invokeFn?: InvokeFn) =>
  call('princess:build:cancel', { opId }, invokeFn);

// P3-C: run commands
export const runStart = (args?: { projectRoot?: string; timeoutMs?: number }, invokeFn?: InvokeFn) =>
  call('princess:run:start', args ?? {}, invokeFn);
export const runStop = (opId: string, invokeFn?: InvokeFn) =>
  call('princess:run:stop', { opId }, invokeFn);

// P3-C: project commands
export const projectOpen = (args?: { path?: string }, invokeFn?: InvokeFn) =>
  call('princess:project:open', args ?? {}, invokeFn);
export const projectValidate = (args?: { path?: string }, invokeFn?: InvokeFn) =>
  call('princess:project:validate', args ?? {}, invokeFn);

// P3-C: debug commands
export const debugAttach = (args?: { host?: string; port?: number; symbols?: string }, invokeFn?: InvokeFn) =>
  call('princess:debug:attach', args ?? {}, invokeFn);

// BUG-008: debug convenience functions (minimal subset)
export const debugSetBreakpoints = (
  breakpoints: { file: string; line: number; enabled?: boolean }[],
  invokeFn?: InvokeFn,
) => call('princess:debug:setBreakpoints', { breakpoints }, invokeFn);

export const debugContinue = (threadId?: number, invokeFn?: InvokeFn) =>
  call('princess:debug:continue', { threadId }, invokeFn);

export const debugStackTrace = (threadId?: number, invokeFn?: InvokeFn) =>
  call('princess:debug:stackTrace', { threadId }, invokeFn);

export const debugRegisters = (threadId?: number, invokeFn?: InvokeFn) =>
  call('princess:debug:registers', { threadId }, invokeFn);

// §3 fs: the editor's file I/O.  `projectRoot` is the confinement boundary —
// the engine refuses any path that resolves outside it with E_SANDBOX_DENIED.
export const fsRead = (projectRoot: string, path: string, invokeFn?: InvokeFn) =>
  call('princess:fs:read', { projectRoot, path }, invokeFn);
export const fsWrite = (projectRoot: string, path: string, content: string, invokeFn?: InvokeFn) =>
  call('princess:fs:write', { projectRoot, path, content }, invokeFn);

// P3-C: LSP commands
export const lspStart = (projectRoot: string, invokeFn?: InvokeFn) =>
  call('princess:lsp:start', { projectRoot }, invokeFn);
export const lspSend = (serverId: string, message: string, invokeFn?: InvokeFn) =>
  call('princess:lsp:send', { serverId, message }, invokeFn);
export const lspStop = (serverId: string, invokeFn?: InvokeFn) =>
  call('princess:lsp:stop', { serverId }, invokeFn);
