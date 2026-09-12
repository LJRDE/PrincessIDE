import { listen } from '@tauri-apps/api/event';
import './styles.css';
import { mountApp } from './app.js';
import { EVENT_CHANNEL } from './contract/events.js';
import { isTauri } from './ipc/client.js';

const root = document.getElementById('app');
if (!root) {
  throw new Error('#app container missing from index.html');
}

/**
 * BUG-003: native directory selector via tauri-plugin-dialog.
 *
 * Only imported and wired when running inside the Tauri webview; in a plain
 * browser preview the action panel degrades to text-input-only (no "Browse…"
 * button rendered).  The `open()` call is wrapped as a `DirSelectorFn` so the
 * action panel stays testable via injection (D17 pattern).
 */
const dirSelectorFn = isTauri()
  ? async (): Promise<string | null> => {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const result = await open({ directory: true, multiple: false });
      // tauri-plugin-dialog returns `string | string[] | null`.
      if (result === null) return null;
      if (Array.isArray(result)) return result[0] ?? null;
      return result;
    }
  : undefined;

/**
 * Subscribe to the engine's event stream.
 *
 * Only the Tauri webview has `listen`; a plain browser preview passes nothing
 * and the shell degrades silently (the panels then show the replayed fixtures,
 * which is what a preview should show).  The channel name comes from the
 * contract mirror, never from a string literal here.
 */
const app = mountApp(root, {
  listenFn: isTauri()
    ? (handler) => listen<unknown>(EVENT_CHANNEL, (event) => handler({ payload: event.payload }))
    : undefined,
  dirSelectorFn,
});

// Handy for manual poking in the webview devtools; not part of any contract.
(window as unknown as { __princesside?: unknown }).__princesside = app;
