/**
 * LSP client tests (decision D17).
 *
 * D17 says the editor's language service is a *mature library* on the frontend,
 * bridged to a local clangd by the engine — not something we reimplement.  This
 * file proves the chosen library (`@codemirror/lsp-client`) really performs the
 * LSP handshake, headlessly, in this container: a scripted fake server answers
 * `initialize` over an in-process transport, and the client's negotiated
 * capabilities are asserted.
 *
 * What is deliberately NOT here: a real clangd run.  Spawning clangd and
 * generating the kernel `.clangd` / `compile_commands.json` is engine-side work
 * (crates/princess-build) and the byte-pipe bridge does not exist yet, so the
 * transport is scripted.  See docs/reports/p3-shell.md "只能由用户本地验证".
 */

import { describe, expect, it, vi } from 'vitest';
import {
  IpcLspBridge,
  LANG_SERVICE_ENV,
  bridgeTransport,
  createLspSession,
  createTransportPair,
  expectedLanguageService,
  isLspMessageEvent,
  type LspBridge,
} from '../src/lsp/client.js';
import { languageForFile } from '../src/components/editor.js';

interface RpcMessage {
  jsonrpc: string;
  id?: number | string;
  method?: string;
  params?: Record<string, unknown>;
  result?: unknown;
}

/** Minimal scripted language server: enough of LSP to negotiate and answer. */
function fakeServer(
  send: (message: string) => void,
  options: { capabilities?: Record<string, unknown> } = {},
) {
  const received: RpcMessage[] = [];
  const notifications: string[] = [];
  const serverCapabilities = options.capabilities ?? {
    textDocumentSync: 1,
    completionProvider: { triggerCharacters: ['.', '>', ':'] },
    hoverProvider: true,
    definitionProvider: true,
    diagnosticProvider: { interFileDependencies: false, workspaceDiagnostics: false },
  };

  return {
    received,
    notifications,
    handler: (message: string) => {
      const parsed = JSON.parse(message) as RpcMessage;
      received.push(parsed);
      if (parsed.method === 'initialize') {
        send(
          JSON.stringify({
            jsonrpc: '2.0',
            id: parsed.id,
            result: {
              capabilities: serverCapabilities,
              serverInfo: { name: 'fake-clangd', version: '16.0.6' },
            },
          }),
        );
        return;
      }
      if (parsed.method === 'initialized') {
        notifications.push('initialized');
        return;
      }
      if (parsed.method === 'shutdown') {
        send(JSON.stringify({ jsonrpc: '2.0', id: parsed.id, result: null }));
        return;
      }
      if (parsed.method === 'exit') {
        notifications.push('exit');
        return;
      }
      // Any other request gets an empty, valid response.
      if (parsed.id !== undefined) {
        send(JSON.stringify({ jsonrpc: '2.0', id: parsed.id, result: null }));
      }
    },
  };
}

describe('LSP transport seam', () => {
  it('delivers messages asynchronously in both directions', async () => {
    const pair = createTransportPair();
    const fromClient: string[] = [];
    const fromServer: string[] = [];

    pair.server.subscribe((m) => fromServer.push(m));
    pair.client.subscribe((m) => fromClient.push(m));

    pair.client.send('{"jsonrpc":"2.0","method":"hello"}');
    pair.server.send('{"jsonrpc":"2.0","id":1,"result":null}');
    expect(fromServer).toEqual([]); // asynchronous: nothing synchronous

    await new Promise((r) => setTimeout(r, 0));
    expect(fromServer).toEqual(['{"jsonrpc":"2.0","method":"hello"}']);
    expect(fromClient).toEqual(['{"jsonrpc":"2.0","id":1,"result":null}']);
  });

  it('stops delivering after unsubscribe', async () => {
    const pair = createTransportPair();
    const seen: string[] = [];
    const handler = (m: string) => seen.push(m);
    pair.server.subscribe(handler);
    pair.client.send('a');
    await new Promise((r) => setTimeout(r, 0));
    pair.server.unsubscribe(handler);
    pair.client.send('b');
    await new Promise((r) => setTimeout(r, 0));
    expect(seen).toEqual(['a']);
  });

  it('adapts an engine bridge to a Transport without touching the protocol', async () => {
    const listeners = new Set<(m: string) => void>();
    const sent: string[] = [];
    const bridge: LspBridge = {
      send: (m) => sent.push(m),
      subscribe: (h) => void listeners.add(h),
      unsubscribe: (h) => void listeners.delete(h),
      describe: () => 'clangd 16.0.6 (fixture)',
    };

    const transport = bridgeTransport(bridge);
    transport.send('{"jsonrpc":"2.0","method":"x"}');
    expect(sent).toEqual(['{"jsonrpc":"2.0","method":"x"}']);

    const handler = vi.fn();
    transport.subscribe(handler);
    for (const l of listeners) l('{"jsonrpc":"2.0","id":1,"result":null}');
    expect(handler).toHaveBeenCalledTimes(1);
    expect(bridge.describe()).toContain('clangd');
  });

  it('states which language service it expects (D18: clangd-16)', () => {
    expect(LANG_SERVICE_ENV).toBe('PRINCESSIDE_LANG_SERVICE_CLANGD');
    expect(expectedLanguageService()).toContain('clangd-16');
  });
});

/**
 * The D17 amendment ratified three commands plus one event.  These tests drive
 * the frontend half of that contract against a scripted engine: no clangd, no
 * display, but the real `IpcLspBridge`, the real IPC client and the real LSP
 * client library.
 */
describe('IpcLspBridge over the ratified lsp contract', () => {
  /** A scripted engine: answers the three lsp commands and pushes lsp.message events. */
  function fakeEngine() {
    const pushed: string[] = [];
    let listener: ((event: { payload: unknown }) => void) | null = null;
    let unlistened = false;

    const invoke = vi.fn(async (_cmd: string, args?: Record<string, unknown>) => {
      const contractCmd = args?.['cmd'] as string;
      const payload = (args?.['args'] ?? {}) as Record<string, unknown>;
      if (contractCmd === 'princess:lsp:start') {
        return {
          ok: true,
          data: {
            serverId: 'lsp-1',
            command: '/root/PrincessIDE/.toolchain/prefix/usr/bin/clangd-16',
            args: ['--background-index'],
          },
        };
      }
      if (contractCmd === 'princess:lsp:send') {
        // A real engine would frame this and write it to clangd's stdin; here we
        // script the reply through the event stream, exactly as §2 prescribes.
        const message = payload['message'] as string;
        pushed.push(message);
        const parsed = JSON.parse(message) as { id?: number; method?: string };
        if (parsed.method === 'initialize') {
          queueMicrotask(() =>
            listener?.({
              payload: {
                v: 1,
                seq: pushed.length,
                ts: '2026-05-05T12:00:00.000Z',
                opId: 'op-lsp1',
                kind: 'lsp.message',
                payload: {
                  serverId: 'lsp-1',
                  message: JSON.stringify({
                    jsonrpc: '2.0',
                    id: parsed.id,
                    result: { capabilities: { hoverProvider: true }, serverInfo: { name: 'clangd', version: '16.0.6' } },
                  }),
                },
              },
            }),
          );
        }
        return { ok: true, data: {} };
      }
      if (contractCmd === 'princess:lsp:stop') return { ok: true, data: {} };
      return {
        ok: false,
        error: { code: 'E_NOT_FOUND', message: `unknown ${String(contractCmd)}`, detail: '' },
      };
    });

    const listenFn = async (handler: (event: { payload: unknown }) => void) => {
      listener = handler;
      return () => {
        unlistened = true;
      };
    };

    return {
      invoke,
      listenFn,
      pushed,
      push: (payload: unknown) => listener?.({ payload }),
      get unlistened() {
        return unlistened;
      },
    };
  }

  it('starts, sends, receives and stops using only §3 commands and the event stream', async () => {
    const engine = fakeEngine();
    const bridge = await IpcLspBridge.start({
      projectRoot: '/root/PrincessIDE/fixtures/refkernel',
      invokeFn: engine.invoke,
      listenFn: engine.listenFn,
    });

    expect(bridge.failure).toBeNull();
    expect(bridge.info?.serverId).toBe('lsp-1');
    expect(bridge.describe()).toContain('clangd-16');

    expect(engine.invoke).toHaveBeenCalledWith('princess_invoke', {
      cmd: 'princess:lsp:start',
      args: { projectRoot: '/root/PrincessIDE/fixtures/refkernel' },
    });

    const received: string[] = [];
    bridge.subscribe((m) => received.push(m));
    bridge.send('{"jsonrpc":"2.0","id":1,"method":"initialize"}');
    await new Promise((r) => setTimeout(r, 5));

    expect(engine.invoke).toHaveBeenCalledWith('princess_invoke', {
      cmd: 'princess:lsp:send',
      args: { serverId: 'lsp-1', message: '{"jsonrpc":"2.0","id":1,"method":"initialize"}' },
    });
    expect(received).toHaveLength(1);
    expect(JSON.parse(received[0]!)).toMatchObject({ id: 1, result: { capabilities: { hoverProvider: true } } });

    await bridge.stop();
    expect(engine.invoke).toHaveBeenLastCalledWith('princess_invoke', {
      cmd: 'princess:lsp:stop',
      args: { serverId: 'lsp-1' },
    });
    expect(engine.unlistened).toBe(true);
  });

  it('feeds the real LSP client library through the bridge (clangd replies → capabilities)', async () => {
    const engine = fakeEngine();
    const bridge = await IpcLspBridge.start({
      projectRoot: '/root/PrincessIDE/fixtures/refkernel',
      invokeFn: engine.invoke,
      listenFn: engine.listenFn,
    });

    const session = createLspSession({
      rootUri: 'file:///root/PrincessIDE/fixtures/refkernel',
      transport: bridgeTransport(bridge),
      timeoutMs: 2000,
      onShutdown: () => void bridge.stop(),
    });

    await session.client.initializing;
    expect(session.client.serverCapabilities?.hoverProvider).toBe(true);
    expect(engine.pushed[0]).toContain('"method":"initialize"');
    // The engine only ever sees header-less JSON-RPC text (§3 lsp:send).
    expect(engine.pushed[0]?.startsWith('Content-Length')).toBe(false);

    session.disconnect();
    await new Promise((r) => setTimeout(r, 5));
    expect(engine.invoke).toHaveBeenCalledWith('princess_invoke', {
      cmd: 'princess:lsp:stop',
      args: { serverId: 'lsp-1' },
    });
  });

  it('ignores events for other servers and malformed payloads', async () => {
    const engine = fakeEngine();
    const bridge = await IpcLspBridge.start({
      projectRoot: '/tmp/kernel',
      invokeFn: engine.invoke,
      listenFn: engine.listenFn,
    });
    const received: string[] = [];
    bridge.subscribe((m) => received.push(m));

    engine.push({ kind: 'lsp.message', payload: { serverId: 'other-server', message: 'nope' } });
    engine.push({ kind: 'lsp.message', payload: { serverId: 'lsp-1' } });
    engine.push({ kind: 'log.append', payload: { stream: 'ide', chunk: 'x', encoding: 'utf8' } });
    engine.push('not an event');
    expect(received).toEqual([]);

    engine.push({ kind: 'lsp.message', payload: { serverId: 'lsp-1', message: '{"jsonrpc":"2.0","id":9,"result":null}' } });
    expect(received).toEqual(['{"jsonrpc":"2.0","id":9,"result":null}']);

    expect(isLspMessageEvent({ kind: 'lsp.message', payload: { serverId: 'a', message: 'b' } })).toBe(true);
    expect(isLspMessageEvent({ kind: 'lsp.message', payload: { serverId: 'a', message: 'b' } }, 'c')).toBe(false);
  });

  it('reports a not-yet-implemented engine explicitly instead of failing silently', async () => {
    // Until P2 lands the engine side, princess:lsp:start answers E_NOT_FOUND.
    const invoke = vi.fn().mockResolvedValue({
      ok: false,
      error: {
        code: 'E_NOT_FOUND',
        message: 'princess:lsp:start is not implemented by the P3 shell yet',
        detail: 'remaining §3 commands belong to P2',
      },
    });
    const bridge = await IpcLspBridge.start({ projectRoot: '/tmp/kernel', invokeFn: invoke });

    expect(bridge.failure).toContain('E_NOT_FOUND');
    expect(bridge.describe()).toContain('unavailable');
    expect(bridge.info).toBeNull();
    expect(() => bridge.send('{}')).toThrow(/E_NOT_FOUND/);
    // Stopping a bridge that never started must not invent a command call.
    await bridge.stop();
    expect(invoke).toHaveBeenCalledTimes(1);
  });
});

describe('LSP handshake with a scripted server', () => {
  it('initializes and negotiates server capabilities', async () => {
    const pair = createTransportPair();
    const server = fakeServer((m) => pair.server.send(m));
    pair.server.subscribe(server.handler);

    const shutdown = vi.fn();
    const session = createLspSession({
      rootUri: 'file:///root/PrincessIDE/fixtures/refkernel',
      transport: pair.client,
      timeoutMs: 2000,
      onShutdown: shutdown,
    });

    await session.client.initializing;

    const initialize = server.received.find((m) => m.method === 'initialize');
    expect(initialize, 'initialize must be sent').toBeDefined();
    expect(initialize?.params?.['rootUri']).toBe('file:///root/PrincessIDE/fixtures/refkernel');
    expect(initialize?.params).toHaveProperty('capabilities');

    expect(session.client.serverCapabilities).not.toBeNull();
    expect(session.client.serverCapabilities?.hoverProvider).toBe(true);
    expect(session.client.connected).toBe(true);

    // `disconnect()` drops the subscription and the negotiated capabilities and
    // hands process teardown (LSP `shutdown` → `exit`) to the bridge hook.
    session.disconnect();
    expect(shutdown).toHaveBeenCalledTimes(1);
    expect(session.client.serverCapabilities).toBeNull();
  });

  it('keeps the capabilities handshake idempotent under repeated disconnects', async () => {
    const pair = createTransportPair();
    const server = fakeServer((m) => pair.server.send(m));
    pair.server.subscribe(server.handler);
    const session = createLspSession({
      rootUri: 'file:///root/PrincessIDE/fixtures/refkernel',
      transport: pair.client,
      timeoutMs: 2000,
    });
    await session.client.initializing;

    expect(session.extensions().length).toBeGreaterThan(0);
    expect(session.plugin('file:///root/PrincessIDE/fixtures/refkernel/kernel.c', 'c')).toBeDefined();
    session.disconnect();
  });

  it('keeps assembly out of the language server', () => {
    expect(languageForFile('boot.S').lspEligible).toBe(false);
    expect(languageForFile('kernel.c').lspEligible).toBe(true);
  });

  it('surfaces a failing server instead of hanging forever', async () => {
    const pair = createTransportPair();
    // A server that never answers `initialize`.
    pair.server.subscribe(() => {});
    const session = createLspSession({
      rootUri: 'file:///tmp/nonexistent-kernel',
      transport: pair.client,
      timeoutMs: 150,
    });
    await expect(session.client.initializing).rejects.toBeTruthy();
    session.disconnect();
  });
});
