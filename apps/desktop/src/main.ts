import './styles.css';
import { mountApp } from './app.js';

const root = document.getElementById('app');
if (!root) {
  throw new Error('#app container missing from index.html');
}

const app = mountApp(root);

// Handy for manual poking in the webview devtools; not part of any contract.
(window as unknown as { __princesside?: unknown }).__princesside = app;
