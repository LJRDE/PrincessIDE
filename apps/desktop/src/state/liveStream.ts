/**
 * Live event stream bridge — subscribes to the Tauri `princess:event` channel
 * and feeds validated envelopes into the event state machine.
 *
 * Design follows the `ListenFn` injectable pattern from `lsp/client.ts` (D17):
 * the Tauri `listen` function is an injected dependency, so vitest can test
 * this module without a real Tauri runtime.  In a browser preview (non-Tauri),
 * if no `listenFn` is provided, the module degrades silently — no error, no
 * crash, just no live events.
 *
 * This module also tracks the most recent `opId` from `build.started` and
 * `run.started` envelopes, which the action panel needs for cancel/stop
 * operations (the IPC commands `princess:build:start` / `princess:run:start`
 * return synchronously and the opId is only available from the event envelope).
 */

import { parseEventEnvelope, ContractViolation } from '../contract/parse.js';
import { applyEvent, type IdeState } from './eventStore.js';

/** Shape of a Tauri event listener, injectable for testing. */
export type ListenFn = (handler: (event: { payload: unknown }) => void) => Promise<() => void>;

export interface LiveStreamOptions {
  /**
   * Injected Tauri `listen` function.  When absent (browser preview), the
   * subscription degrades silently — no error thrown.
   */
  listenFn?: ListenFn;
  /**
   * Callback invoked after each envelope is successfully applied to state.
   * The UI should re-render its panels here.
   */
  onStateChange: (state: IdeState) => void;
}

export interface LiveStreamHandle {
  /** Current state snapshot. */
  state(): IdeState;
  /** Last opId from a `build.started` event, or null if none received. */
  lastBuildOpId(): string | null;
  /** Last opId from a `run.started` event, or null if none received. */
  lastRunOpId(): string | null;
  /** Unsubscribe from the event channel and reset. */
  unsubscribe(): void;
}

/**
 * Subscribe to the Tauri `princess:event` channel.
 *
 * Returns a handle for reading state / opIds and for cleanup.
 * In non-Tauri environments (no listenFn), returns a no-op handle with
 * initial state.
 */
export function subscribeToLiveStream(
  initialState: IdeState,
  options: LiveStreamOptions,
): LiveStreamHandle {
  let state = initialState;
  let unlisten: (() => void) | null = null;
  let lastBuildOpId: string | null = null;
  let lastRunOpId: string | null = null;

  const listenFn = options.listenFn;
  if (listenFn) {
    void listenFn((event) => {
      const raw = event.payload;
      let envelope;
      try {
        envelope = parseEventEnvelope(raw, 'live-stream');
      } catch (err) {
        // Malformed envelope — log but don't crash the UI.
        if (err instanceof ContractViolation) {
          console.warn(`[liveStream] dropped envelope: ${err.message}`);
        }
        return;
      }

      // Track opId for build/run started events.
      if (envelope.kind === 'build.started' && envelope.opId) {
        lastBuildOpId = envelope.opId;
      }
      if (envelope.kind === 'run.started' && envelope.opId) {
        lastRunOpId = envelope.opId;
      }

      state = applyEvent(state, envelope);
      options.onStateChange(state);
    }).then((fn) => {
      unlisten = fn;
    });
  }

  return {
    state: () => state,
    lastBuildOpId: () => lastBuildOpId,
    lastRunOpId: () => lastRunOpId,
    unsubscribe: () => {
      unlisten?.();
      unlisten = null;
    },
  };
}
