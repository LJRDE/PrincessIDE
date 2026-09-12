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
});

// Handy for manual poking in the webview devtools; not part of any contract.
(window as unknown as { __princesside?: unknown }).__princesside = app;
