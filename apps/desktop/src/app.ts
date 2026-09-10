/**
 * Application shell: wires the toolchain table (IPC), the event-stream panels
 * (contract §2) and the CodeMirror editor into the Tauri window.
 *
 * Kept dependency-free (no framework) on purpose: P3 is the shell, and the DOM
 * is small enough that a framework would be more surface than value.  All the
 * interesting logic lives in `state/` and `contract/` so it is testable in Node.
 */

import { createToolTableState, renderToolTable, type ToolTableState } from './components/toolTable.js';
import {
  renderDebugPanel,
  renderDiagnostics,
  renderEventLog,
  renderEventStatus,
  renderFaultCard,
} from './components/eventLog.js';
import { createEditor } from './components/editor.js';
import { isTauri, opReplay, toolsDetect } from './ipc/client.js';
import { expectedLanguageService } from './lsp/client.js';
import { createInitialState, replayEvents, type IdeState } from './state/eventStore.js';
import { loadAllBundledFixtures } from './state/fixtureLoader.js';

export interface AppHandles {
  refreshTools(): Promise<void>;
  loadFixture(name: string): void;
  state(): IdeState;
  destroy(): void;
}

function section(root: HTMLElement, title: string, testid: string): HTMLElement {
  const sec = document.createElement('section');
  sec.className = 'panel';
  const h = document.createElement('h1');
  h.textContent = title;
  sec.appendChild(h);
  sec.dataset['testid'] = testid;
  const body = document.createElement('div');
  sec.appendChild(body);
  root.appendChild(sec);
  return body;
}

export function mountApp(root: HTMLElement): AppHandles {
  root.textContent = '';
  root.className = 'app';

  const header = document.createElement('header');
  header.className = 'topbar';
  const brand = document.createElement('strong');
  brand.textContent = 'PrincessIDE';
  const envChip = document.createElement('span');
  envChip.className = 'chip';
  envChip.dataset['testid'] = 'env';
  envChip.textContent = isTauri() ? 'tauri shell' : 'browser preview (no engine IPC)';
  const refresh = document.createElement('button');
  refresh.dataset['testid'] = 'refresh-tools';
  refresh.textContent = 'detect toolchain';
  header.append(brand, envChip, refresh);
  root.appendChild(header);

  const grid = document.createElement('div');
  grid.className = 'grid';
  root.appendChild(grid);

  const left = document.createElement('div');
  left.className = 'col';
  const right = document.createElement('div');
  right.className = 'col';
  grid.append(left, right);

  // --- toolchain panel -----------------------------------------------------
  const toolBody = section(left, 'Toolchain (princess:tools:detect)', 'toolchain-panel');
  const toolHost = document.createElement('div');
  toolBody.appendChild(toolHost);
  const toolState: ToolTableState = createToolTableState();

  const refreshTools = async (): Promise<void> => {
    toolState.loading = true;
    toolState.error = null;
    renderToolTable(toolHost, toolState);
    const res = await toolsDetect();
    toolState.loading = false;
    if (res.ok) {
      toolState.data = res.data;
      toolState.error = null;
    } else {
      toolState.error = res.error;
      toolState.data = null;
    }
    renderToolTable(toolHost, toolState);
  };
  refresh.addEventListener('click', () => {
    void refreshTools();
  });
  renderToolTable(toolHost, toolState);

  // --- editor panel --------------------------------------------------------
  const editorBody = section(left, 'Editor (CodeMirror 6)', 'editor-panel');
  const editorHost = document.createElement('div');
  editorHost.className = 'editor-host';
  editorBody.appendChild(editorHost);
  const lspNote = document.createElement('p');
  lspNote.className = 'muted';
  lspNote.dataset['testid'] = 'lsp-status';
  // Being explicit beats a bare "no LSP": say which server is expected and why
  // it is not connected yet (D17 = the engine owns the bridge, D18 = clangd-16).
  lspNote.textContent = `LSP: not connected · expected ${expectedLanguageService()} · engine-side bridge pending (D17)`;
  lspNote.dataset['server'] = LANG_SERVICE_ENV_MARKER;
  editorBody.appendChild(lspNote);
  const editor = createEditor(editorHost, { filename: 'kernel.c', doc: DEMO_SOURCE });

  // --- event stream panels -------------------------------------------------
  const eventsBody = section(right, 'Event stream (contract §2)', 'events-panel');
  const statusBar = document.createElement('div');
  statusBar.className = 'statusbar';
  eventsBody.appendChild(statusBar);

  const fixturePicker = document.createElement('select');
  fixturePicker.dataset['testid'] = 'fixture-picker';
  const fixtures = loadAllBundledFixtures();
  for (const f of fixtures) {
    const opt = document.createElement('option');
    opt.value = f.name;
    opt.textContent = `${f.name}${f.constructed ? ' (P3-constructed)' : ''}`;
    fixturePicker.appendChild(opt);
  }
  eventsBody.appendChild(fixturePicker);

  const logHost = document.createElement('div');
  logHost.className = 'log';
  eventsBody.appendChild(logHost);

  const faultHost = document.createElement('div');
  faultHost.className = 'fault-card';
  eventsBody.appendChild(faultHost);

  const diagHost = document.createElement('div');
  diagHost.className = 'diagnostics';
  eventsBody.appendChild(diagHost);

  const debugHost = document.createElement('div');
  debugHost.className = 'debug-panel';
  eventsBody.appendChild(debugHost);

  let state = createInitialState();
  const render = (): void => {
    renderEventStatus(statusBar, state);
    renderEventLog(logHost, state);
    renderFaultCard(faultHost, state);
    renderDiagnostics(diagHost, state);
    renderDebugPanel(debugHost, state);
  };

  const loadFixture = (name: string): void => {
    const fixture = fixtures.find((f) => f.name === name);
    state = fixture ? replayEvents(fixture.events) : createInitialState();
    render();
  };
  fixturePicker.addEventListener('change', () => loadFixture(fixturePicker.value));
  // Prefer the reference-kernel session on first paint: it exercises the whole
  // path a user cares about first (build → run → fault → exit).  Falls back to
  // whatever fixture the bundle happens to contain.
  const preferred = fixtures.find((f) => f.name.startsWith('refkernel-session')) ?? fixtures[0];
  if (preferred) {
    fixturePicker.value = preferred.name;
    loadFixture(preferred.name);
  } else {
    render();
  }

  // `op:replay` is wired to the same UI affordance the live stream will use.
  statusBar.addEventListener('click', (ev) => {
    const target = ev.target as HTMLElement;
    if (target.dataset['testid'] === 'request-replay') {
      void opReplay(state.lastSeq + 1).then((res) => {
        if (!res.ok) {
          const box = document.createElement('div');
          box.className = 'error-box';
          box.textContent = `princess:op:replay → ${res.error.code}: ${res.error.message}`;
          statusBar.appendChild(box);
        }
      });
    }
  });

  return {
    refreshTools,
    loadFixture,
    state: () => state,
    destroy: () => editor.destroy(),
  };
}

const LANG_SERVICE_ENV_MARKER = 'PRINCESSIDE_LANG_SERVICE_CLANGD';

const DEMO_SOURCE = `/* PrincessIDE shell demo buffer — fixtures/refkernel/kernel.c shaped.
   Real files are opened by the engine (princess:project:open, later phase). */
#include "serial.h"

void kmain(void) {
    serial_write("PrincessIDE reference kernel booted\\n");
    refkernel_fault_probe();
}
`;
