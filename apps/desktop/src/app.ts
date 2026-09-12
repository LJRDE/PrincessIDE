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
  renderDebugPanel as renderEventDebugPanel,
  renderDiagnostics,
  renderEventLog,
  renderEventStatus,
  renderFaultCard,
} from './components/eventLog.js';
import { renderActionPanel, type ActionPanelCallbacks, type DirSelectorFn } from './components/actionPanel.js';
import { renderDebugPanel, type DebugPanelCallbacks } from './components/debugPanel.js';
import { createEditor } from './components/editor.js';
import {
  isTauri,
  opReplay,
  toolsDetect,
  buildStart,
  buildCancel,
  runStart,
  runStop,
  projectOpen,
  debugAttach,
  debugSetBreakpoints,
  debugContinue,
  debugStackTrace,
  debugRegisters,
} from './ipc/client.js';
import { expectedLanguageService, type ListenFn } from './lsp/client.js';
import { createInitialState, replayEvents, type IdeState } from './state/eventStore.js';
import { loadAllBundledFixtures } from './state/fixtureLoader.js';
import { subscribeToLiveStream, type LiveStreamHandle } from './state/liveStream.js';

export interface AppHandles {
  refreshTools(): Promise<void>;
  loadFixture(name: string): void;
  state(): IdeState;
  /** Last build opId from the live event stream, or null. */
  lastBuildOpId(): string | null;
  /** Last run opId from the live event stream, or null. */
  lastRunOpId(): string | null;
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

export interface MountAppOptions {
  /** Injected Tauri `listen` for the event stream.  Absent = browser preview (degrades silently). */
  listenFn?: ListenFn;
  /**
   * Injected directory selector (BUG-003, D17 pattern).  Absent = browser
   * preview: only the text input path entry is rendered (no "Browse…" button).
   */
  dirSelectorFn?: DirSelectorFn;
}

export function mountApp(root: HTMLElement, options: MountAppOptions = {}): AppHandles {
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

  // --- action panel (BUG-001 fix) -------------------------------------------
  const actionBody = section(left, 'Actions', 'action-panel-section');
  const actionHost = document.createElement('div');
  actionBody.appendChild(actionHost);

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

  // --- BUG-008: Debug panel (attach/breakpoints/registers/stacktrace) -------
  const debugPanelBody = section(right, 'Debug (BUG-008)', 'debug-panel-section');
  const debugPanelHost = document.createElement('div');
  debugPanelBody.appendChild(debugPanelHost);

  // --- error display (IPC failures, contract §0.4) -------------------------
  const errorHost = document.createElement('div');
  errorHost.className = 'ipc-errors';
  errorHost.dataset['testid'] = 'ipc-errors';
  root.appendChild(errorHost);

  let state = createInitialState();

  // --- BUG-008: Debug panel callbacks wired to real IPC --------------------
  const debugCallbacks: DebugPanelCallbacks = {
    attach: async (args) => {
      const res = await debugAttach({ host: args.host, port: args.port, symbols: args.symbols });
      return res;
    },
    setBreakpoints: async (breakpoints) => {
      const res = await debugSetBreakpoints(breakpoints);
      return res;
    },
    continue: async () => {
      const res = await debugContinue();
      return res;
    },
    stackTrace: async () => {
      const res = await debugStackTrace();
      return res;
    },
    registers: async () => {
      const res = await debugRegisters();
      return res;
    },
  };

  /** Re-render all panels including the action panel. */
  const render = (): void => {
    renderEventStatus(statusBar, state);
    renderEventLog(logHost, state);
    renderFaultCard(faultHost, state);
    renderDiagnostics(diagHost, state);
    renderEventDebugPanel(debugHost, state);
    renderDebugPanel(debugPanelHost, state, debugCallbacks);
    renderActionPanel(actionHost, state, actionCallbacks, actionPanelOptions);
  };

  // --- Action panel callbacks wired to real IPC ----------------------------

  /** Show an IPC error explicitly (contract §0.4: never blank, never silent). */
  const showIpcError = (label: string, err: { code: string; message: string; detail: string }): void => {
    const box = document.createElement('div');
    box.className = 'error-box';
    box.textContent = `${label} → ${err.code}: ${err.message}`;
    errorHost.appendChild(box);
    // Auto-remove after 10 seconds to avoid flooding.
    setTimeout(() => box.remove(), 10_000);
  };

  const actionCallbacks: ActionPanelCallbacks = {
    onBuild: () => {
      void buildStart().then((res) => {
        if (!res.ok) showIpcError('princess:build:start', res.error);
      });
    },
    onRun: () => {
      void runStart().then((res) => {
        if (!res.ok) showIpcError('princess:run:start', res.error);
      });
    },
    onStopBuild: () => {
      const opId = liveStream.lastBuildOpId();
      if (!opId) {
        showIpcError('princess:build:cancel', {
          code: 'E_INTERNAL',
          message: 'No active build operation (no opId received from event stream)',
          detail: '',
        });
        return;
      }
      void buildCancel(opId).then((res) => {
        if (!res.ok) showIpcError('princess:build:cancel', res.error);
      });
    },
    onStopRun: () => {
      const opId = liveStream.lastRunOpId();
      if (!opId) {
        showIpcError('princess:run:stop', {
          code: 'E_INTERNAL',
          message: 'No active run operation (no opId received from event stream)',
          detail: '',
        });
        return;
      }
      void runStop(opId).then((res) => {
        if (!res.ok) showIpcError('princess:run:stop', res.error);
      });
    },
    onProjectOpen: (path: string) => {
      // BUG-003: path comes from either the text input or the native dir selector.
      if (!path) {
        showIpcError('princess:project:open', {
          code: 'E_INTERNAL',
          message: 'Please enter a project path',
          detail: '',
        });
        return;
      }
      void projectOpen({ path }).then((res) => {
        if (!res.ok) showIpcError('princess:project:open', res.error);
      });
    },
  };

  // BUG-003: pass dirSelectorFn to the action panel so the "Browse…" button
  // is only rendered inside the Tauri shell.
  const actionPanelOptions = { dirSelectorFn: options.dirSelectorFn };

  // Initial render of the action panel.
  renderActionPanel(actionHost, state, actionCallbacks, actionPanelOptions);

  // --- Live event stream subscription (Task 2) -----------------------------

  // In the Tauri shell, `listen` is available as `window.__TAURI_INTERNALS__`.
  // We accept an injected listenFn for testability (D17 pattern from lsp/client.ts).
  const liveStream: LiveStreamHandle = subscribeToLiveStream(state, {
    listenFn: options.listenFn,
    onStateChange: (newState) => {
      state = newState;
      render();
    },
  });

  // --- Fixture-based offline replay ----------------------------------------

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
    lastBuildOpId: () => liveStream.lastBuildOpId(),
    lastRunOpId: () => liveStream.lastRunOpId(),
    destroy: () => {
      liveStream.unsubscribe();
      editor.destroy();
    },
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
