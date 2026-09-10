/**
 * LSP client seam (decision D17, contract §3 `lsp` domain).
 *
 * D17 split: the **engine** owns the clangd process, its kernel-specific
 * configuration (`.clangd` / `compile_commands.json`, per D7/D8) and the
 * `Content-Length` framing; the **frontend** owns editor interaction and uses a
 * mature LSP client library.  So this file contains **no LSP protocol
 * implementation** — framing, capability negotiation and lifecycle live in
 * [`@codemirror/lsp-client`](https://codemirror.net/6/docs/ref/#lsp-client).
 *
 * The bridge maps one-to-one onto the three ratified commands plus one event:
 *
 *   princess:lsp:start { projectRoot } → { serverId, command, args }
 *   princess:lsp:send  { serverId, message } → {}   // message: header-less JSON-RPC
 *   princess:lsp:stop  { serverId } → {}
 *   event  lsp.message { serverId, message }        // replies travel only on the event stream
 *   event  lsp.stopped { serverId, reason }
 *
 * Contract §0.1 is respected: commands plus the single event stream, no second transport.
 */

import {
  LSPClient,
  languageServerExtensions,
  type LSPClientExtension,
  type Transport,
} from '@codemirror/lsp-client';
import type { Extension } from '@codemirror/state';

import { call, type InvokeFn } from '../ipc/client.js';
import type { LspSendArgs, LspStartArgs, LspStartData, LspStopArgs } from '../contract/ipc.js';

export type { Transport };

/**
 * Decision D18: the language service must be `clangd-16`, selected through the
 * environment variable `scripts/env.sh` exports — the unversioned `clangd` on
 * this machine is intentionally still 14 for zero-regression reasons, and using
 * it silently loses capabilities.  The bridge (engine side) is what spawns the
 * process; this constant exists so the frontend can *report* which server it
 * expects instead of leaving the user guessing.
 */
export const LANG_SERVICE_ENV = 'PRINCESSIDE_LANG_SERVICE_CLANGD' as const;

/** Human-readable description of the expected server, shown in the UI. */
export function expectedLanguageService(): string {
  return 'clangd-16 (via $PRINCESSIDE_LANG_SERVICE_CLANGD, D18)';
}

export interface LspBridge {
  /** Send one JSON-RPC message (no headers) to the language server process. */
  send(message: string): void;
  /** Subscribe to messages coming back from the server process. */
  subscribe(handler: (message: string) => void): void;
  unsubscribe(handler: (message: string) => void): void;
  /** Which server this bridge drives; shown in the UI so the user is never guessing. */
  describe(): string;
}

/** Wrap a bridge as the library's `Transport`. */
export function bridgeTransport(bridge: LspBridge): Transport {
  return {
    send: (message: string) => bridge.send(message),
    subscribe: (handler) => bridge.subscribe(handler),
    unsubscribe: (handler) => bridge.unsubscribe(handler),
  };
}

/**
 * Two transports wired to each other, with async delivery.  Used by tests to
 * run the genuine `LSPClient` against a fake server in-process.
 */
export function createTransportPair(): { client: Transport; server: Transport } {
  const clientHandlers = new Set<(value: string) => void>();
  const serverHandlers = new Set<(value: string) => void>();

  const make = (out: Set<(v: string) => void>, into: Set<(v: string) => void>): Transport => ({
    send(message: string) {
      queueMicrotask(() => {
        for (const h of [...into]) h(message);
      });
    },
    subscribe(handler) {
      out.add(handler);
    },
    unsubscribe(handler) {
      out.delete(handler);
    },
  });

  return {
    client: make(clientHandlers, serverHandlers),
    server: make(serverHandlers, clientHandlers),
  };
}

/**
 * Validate a `lsp.message` envelope (contract §2) before it reaches the language
 * client: a malformed event must not be able to corrupt the editor's protocol
 * state, and events for other servers must not leak into this session.
 */
export function isLspMessageEvent(
  event: unknown,
  serverId?: string,
): event is { kind: string; payload: { serverId: string; message: string } } {
  if (typeof event !== 'object' || event === null) return false;
  const e = event as { kind?: unknown; payload?: unknown };
  if (e.kind !== 'lsp.message') return false;
  const p = e.payload as { serverId?: unknown; message?: unknown } | undefined;
  if (typeof p?.serverId !== 'string' || typeof p.message !== 'string') return false;
  return serverId === undefined || p.serverId === serverId;
}

/** Shape of a Tauri event listener, injectable so this is testable without Tauri. */
export type ListenFn = (handler: (event: { payload: unknown }) => void) => Promise<() => void>;

export interface IpcLspBridgeOptions {
  projectRoot: string;
  invokeFn?: InvokeFn;
  listenFn?: ListenFn;
}

/**
 * The real bridge: `lsp:start` on construction, `lsp:send` per message, replies
 * taken from the `lsp.message` event stream, `lsp:stop` on teardown.
 *
 * The engine side of these three commands is P2's work.  Until it lands they
 * answer `E_NOT_FOUND`, and this bridge surfaces that as an explicit failure
 * (see `failure` / `describe()`) instead of a silent no-op.
 */
export class IpcLspBridge implements LspBridge {
  private readonly handlers = new Set<(message: string) => void>();
  private serverId: string | null = null;
  private unlisten: (() => void) | null = null;
  private startInfo: LspStartData | null = null;
  private error: string | null = null;

  private constructor(private readonly options: IpcLspBridgeOptions) {}

  /** Start the server and wire the event stream.  Never throws: failures are remembered. */
  static async start(options: IpcLspBridgeOptions): Promise<IpcLspBridge> {
    const bridge = new IpcLspBridge(options);
    const args: LspStartArgs = { projectRoot: options.projectRoot };
    const result = await call('princess:lsp:start', args, options.invokeFn);
    if (!result.ok) {
      bridge.error = `${result.error.code}: ${result.error.message}`;
      return bridge;
    }
    bridge.startInfo = result.data;
    bridge.serverId = result.data.serverId;

    if (options.listenFn) {
      bridge.unlisten = await options.listenFn((event) => {
        // Tauri wraps the emitted value in `event.payload`, so the *envelope*
        // (contract §2) is `event.payload` and the JSON-RPC text is one level
        // deeper still: event.payload.payload.message.
        const envelope = event.payload;
        if (!isLspMessageEvent(envelope, bridge.serverId ?? undefined)) return;
        for (const handler of [...bridge.handlers]) handler(envelope.payload.message);
      });
    }
    return bridge;
  }

  /** null when the server started; otherwise the engine's error, verbatim. */
  get failure(): string | null {
    return this.error;
  }

  get info(): LspStartData | null {
    return this.startInfo;
  }

  send(message: string): void {
    const serverId = this.serverId;
    if (!serverId) throw new Error(this.error ?? 'LSP bridge is not connected');
    const args: LspSendArgs = { serverId, message };
    // Fire-and-forget per the library's Transport contract; a failed send is
    // recorded so `describe()`/the UI can show it rather than dropping it.
    void call('princess:lsp:send', args, this.options.invokeFn).then((result) => {
      if (!result.ok) this.error = `${result.error.code}: ${result.error.message}`;
    });
  }

  subscribe(handler: (message: string) => void): void {
    this.handlers.add(handler);
  }

  unsubscribe(handler: (message: string) => void): void {
    this.handlers.delete(handler);
  }

  describe(): string {
    if (this.error) return `unavailable (${this.error})`;
    if (!this.startInfo) return 'starting…';
    return `${this.startInfo.command} ${this.startInfo.args.join(' ')}`.trim();
  }

  /** Ask the engine to shut the server down; also drops the event subscription. */
  async stop(): Promise<void> {
    this.unlisten?.();
    this.unlisten = null;
    const serverId = this.serverId;
    this.serverId = null;
    if (!serverId) return;
    const args: LspStopArgs = { serverId };
    const result = await call('princess:lsp:stop', args, this.options.invokeFn);
    if (!result.ok) this.error = `${result.error.code}: ${result.error.message}`;
  }
}

export interface LspSession {
  client: LSPClient;
  /**
   * The extra extensions this client carries (completion/hover/diagnostics
   * wiring).  `LSPPlugin.create` already includes these, so they only need to be
   * attached separately when an editor is configured without a plugin.
   */
  extensions(): readonly (Extension | LSPClientExtension)[];
  /** CodeMirror extension binding one file to the server. */
  plugin(fileUri: string, languageId?: string): Extension;
  /**
   * Tear the session down.
   *
   * The library deliberately does not implement the LSP lifecycle handshake —
   * its README leaves "how to connect and how to handle disconnects" to the
   * transport (research report §4.8: shutdown→exit is exit code 0, bare exit is
   * 1).  The bridge owns that handshake, so it is `onShutdown`'s job.
   */
  disconnect(): void;
}

/** Create + connect a client.  `rootUri` must be a `file://` URI of the project root. */
export function createLspSession(options: {
  rootUri: string;
  transport: Transport;
  timeoutMs?: number;
  /** Called before the transport is dropped — the bridge's `shutdown`/`exit` hook. */
  onShutdown?: () => void;
}): LspSession {
  const client = new LSPClient({
    rootUri: options.rootUri,
    timeout: options.timeoutMs ?? 5000,
    extensions: languageServerExtensions(),
    // Server-provided markdown is untrusted input; keep it as plain text.
    sanitizeHTML: (html: string) => html.replace(/<[^>]*>/g, ''),
  });
  client.connect(options.transport);

  return {
    client,
    extensions: () => languageServerExtensions(),
    plugin: (fileUri: string, languageId?: string) =>
      languageId === undefined ? client.plugin(fileUri) : client.plugin(fileUri, languageId),
    disconnect: () => {
      options.onShutdown?.();
      client.disconnect();
    },
  };
}

/** True when the client finished `initialize`/`initialized` (capabilities known). */
export function isInitialized(client: LSPClient): boolean {
  return client.serverCapabilities !== null;
}
