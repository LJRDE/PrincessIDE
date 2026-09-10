/**
 * IPC client tests — the wire shape must be exactly the contract's §3 envelope,
 * and failures must arrive as `{ ok:false, error:{ code, message, detail } }`
 * rather than as thrown exceptions (contract §0.4).
 */

import { describe, expect, it, vi } from 'vitest';
import {
  NATIVE_DISPATCH_COMMAND,
  call,
  isTauri,
  opCancel,
  opReplay,
  toIpcError,
  toolsDetect,
} from '../src/ipc/client.js';
import type { ToolsDetectData } from '../src/contract/ipc.js';

const detectData: ToolsDetectData = {
  tools: [
    {
      name: 'cargo',
      status: 'ok',
      version: 'cargo 1.98.1',
      path: '/root/.toolchain/cargo/bin/cargo',
      available: true,
      required: true,
      checks: [],
    },
  ],
  exitCode: 0,
  missing: [],
  root: '/root/PrincessIDE',
  command: "bash -c 'source …/env.sh && exec bash …/doctor.sh'",
  rawStdout: 'cargo                  ok         cargo 1.98.1\n',
  rawStderr: '',
};

describe('ipc client', () => {
  it('routes every contract command through the single dispatcher with the name verbatim', async () => {
    const invoke = vi.fn().mockResolvedValue({ ok: true, data: detectData });
    const res = await toolsDetect(invoke);

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith(NATIVE_DISPATCH_COMMAND, {
      cmd: 'princess:tools:detect',
      args: {},
    });
    expect(res.ok).toBe(true);
    if (res.ok) expect(res.data.tools[0]?.name).toBe('cargo');
  });

  it('passes opId and fromSeq arguments under their contract names', async () => {
    const invoke = vi.fn().mockResolvedValue({ ok: true, data: { opId: 'op-7f3a', cancelled: true, found: true } });
    await opCancel('op-7f3a', invoke);
    expect(invoke).toHaveBeenLastCalledWith(NATIVE_DISPATCH_COMMAND, {
      cmd: 'princess:op:cancel',
      args: { opId: 'op-7f3a' },
    });

    await opReplay(42, invoke);
    expect(invoke).toHaveBeenLastCalledWith(NATIVE_DISPATCH_COMMAND, {
      cmd: 'princess:op:replay',
      args: { fromSeq: 42 },
    });
  });

  it('returns engine-reported failures unchanged', async () => {
    const failure = {
      ok: false as const,
      error: { code: 'E_TOOLCHAIN_MISSING' as const, message: 'nasm missing', detail: 'doctor: 1 missing' },
    };
    const res = await call('princess:tools:detect', undefined, vi.fn().mockResolvedValue(failure));
    expect(res).toEqual(failure);
  });

  it('maps a thrown transport error to an explicit E_INTERNAL result', async () => {
    const res = await toolsDetect(vi.fn().mockRejectedValue(new Error('ipc not available')));
    expect(res.ok).toBe(false);
    if (!res.ok) {
      expect(res.error.code).toBe('E_INTERNAL');
      expect(res.error.message).toBe('ipc not available');
      expect(res.error.detail.length).toBeGreaterThan(0);
    }
  });

  it('keeps a structured error thrown by the engine', () => {
    expect(toIpcError({ code: 'E_TIMEOUT', message: 'doctor timed out', detail: 'killed after 60s' })).toEqual({
      code: 'E_TIMEOUT',
      message: 'doctor timed out',
      detail: 'killed after 60s',
    });
  });

  it('never leaves detail undefined', () => {
    expect(toIpcError({ code: 'E_CANCELLED', message: 'gone' }).detail).toBe('');
    expect(toIpcError('plain string').code).toBe('E_INTERNAL');
    expect(toIpcError(undefined).message).toBe('undefined');
  });

  it('detects a non-Tauri environment instead of pretending IPC works', () => {
    expect(isTauri()).toBe(false); // node/jsdom test env has no __TAURI_INTERNALS__
  });
});
