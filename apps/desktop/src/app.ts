/**
 * Application shell: wires the toolchain table (IPC), the event-stream panels
 * (contract §2) and the CodeMirror editor into the Tauri window.
 *
 * Kept dependency-free (no framework) on purpose: P3 is the shell, and the DOM
 * is small enough that a framework would be more surface than value.  All the
 * interesting logic lives in `state/` and `contract/` so it is testable in Node.
 *
 * P-A: Layout is now driven by the view registry (`views/registry.ts`).
 * Each panel self-registers; `app.ts` iterates the registry and mounts them.
 * This eliminates the BUG-001/002 class of bugs (component exists but is
 * never mounted) — the registry test asserts every registered testid is in the DOM.
 */

import { listViews, type ViewMountResult } from './views/registry.js';
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
import { type ListenFn } from './lsp/client.js';
import { createInitialState, replayEvents, type IdeState } from './state/eventStore.js';
import { loadAllBundledFixtures } from './state/fixtureLoader.js';
import { subscribeToLiveStream, type LiveStreamHandle } from './state/liveStream.js';
import type { ActionPanelCallbacks, DirSelectorFn } from './components/actionPanel.js';
import type { DebugPanelCallbacks } from './components/debugPanel.js';
import type { ToolTableState } from './components/toolTable.js';

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

  // --- Top bar (not a registry view — it's the app shell itself) -----------
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

  // --- Grid layout (left / right columns) ---------------------------------
  const grid = document.createElement('div');
  grid.className = 'grid';
  root.appendChild(grid);

  const left = document.createElement('div');
  left.className = 'col';
  const right = document.createElement('div');
  right.className = 'col';
  grid.append(left, right);

  // --- Error display (IPC failures, contract §0.4) -------------------------
  const errorHost = document.createElement('div');
  errorHost.className = 'ipc-errors';
  errorHost.dataset['testid'] = 'ipc-errors';
  root.appendChild(errorHost);

  // --- Mount all registered views ------------------------------------------
  const views = listViews();
  const mounted: { view: typeof views[number]; result: ViewMountResult & Record<string, unknown> }[] = [];

  // Column assignment: first 3 views go left, rest go right.
  const columnMap = [left, left, left, right, right];

  for (let i = 0; i < views.length; i++) {
    const view = views[i];
    const col = columnMap[i] ?? right;
    const host = section(col, view.title, view.testid);
    const result = view.mount(host) as ViewMountResult & Record<string, unknown>;
    mounted.push({ view, result });
  }

  // --- Wire up action panel callbacks --------------------------------------
  const actionMount = mounted.find((m) => m.view.id === 'action-panel');
  const debugMount = mounted.find((m) => m.view.id === 'debug-panel');
  const toolchainMount = mounted.find((m) => m.view.id === 'toolchain-panel');

  /** Show an IPC error explicitly (contract §0.4: never blank, never silent). */
  const showIpcError = (label: string, err: { code: string; message: string; detail: string }): void => {
    const box = document.createElement('div');
    box.className = 'error-box';
    box.textContent = `${label} → ${err.code}: ${err.message}`;
    errorHost.appendChild(box);
    setTimeout(() => box.remove(), 10_000);
  };

  let state = createInitialState();

  // Live event stream subscription
  const liveStream: LiveStreamHandle = subscribeToLiveStream(state, {
    listenFn: options.listenFn,
    onStateChange: (newState) => {
      state = newState;
      render();
    },
  });

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

  // Wire action panel callbacks
  if (actionMount) {
    const r = actionMount.result as Record<string, unknown>;
    if (typeof r['setCallbacks'] === 'function') {
      (r['setCallbacks'] as (c: ActionPanelCallbacks) => void)(actionCallbacks);
    }
    if (typeof r['setOptions'] === 'function') {
      (r['setOptions'] as (o: { dirSelectorFn?: DirSelectorFn }) => void)({ dirSelectorFn: options.dirSelectorFn });
    }
  }

  // Wire debug panel callbacks
  if (debugMount) {
    const debugCallbacks: DebugPanelCallbacks = {
      attach: async (args) => debugAttach({ host: args.host, port: args.port, symbols: args.symbols }),
      setBreakpoints: async (breakpoints) => debugSetBreakpoints(breakpoints),
      continue: async () => debugContinue(),
      stackTrace: async () => debugStackTrace(),
      registers: async () => debugRegisters(),
    };
    const r = debugMount.result as Record<string, unknown>;
    if (typeof r['setCallbacks'] === 'function') {
      (r['setCallbacks'] as (c: DebugPanelCallbacks) => void)(debugCallbacks);
    }
  }

  // --- Toolchain refresh wiring --------------------------------------------
  const refreshTools = async (): Promise<void> => {
    if (toolchainMount) {
      const ts = (toolchainMount.result as Record<string, unknown>)['toolState'] as ToolTableState | undefined;
      if (ts) {
        ts.loading = true;
        ts.error = null;
        render();
        const res = await toolsDetect();
        ts.loading = false;
        if (res.ok) {
          ts.data = res.data;
          ts.error = null;
        } else {
          ts.error = res.error;
          ts.data = null;
        }
        render();
      }
    }
  };
  refresh.addEventListener('click', () => {
    void refreshTools();
  });

  // --- Render all views ----------------------------------------------------
  const render = (): void => {
    for (const { result } of mounted) {
      result.render(state);
    }
  };

  // Initial render (including toolchain panel).
  render();

  // --- Fixture-based offline replay ----------------------------------------
  // The fixture picker is part of the events-panel view, but the loadFixture
  // logic lives here because it mutates the shared state.
  const fixturePicker = root.querySelector('[data-testid="fixture-picker"]') as HTMLSelectElement | null;
  const fixtures = loadAllBundledFixtures();

  const loadFixture = (name: string): void => {
    const fixture = fixtures.find((f) => f.name === name);
    state = fixture ? replayEvents(fixture.events) : createInitialState();
    render();
  };

  if (fixturePicker) {
    fixturePicker.addEventListener('change', () => loadFixture(fixturePicker.value));
    const preferred = fixtures.find((f) => f.name.startsWith('refkernel-session')) ?? fixtures[0];
    if (preferred) {
      fixturePicker.value = preferred.name;
      loadFixture(preferred.name);
    } else {
      render();
    }
  }

  // `op:replay` is wired to the same UI affordance the live stream will use.
  root.addEventListener('click', (ev) => {
    const target = ev.target as HTMLElement;
    if (target.dataset['testid'] === 'request-replay') {
      void opReplay(state.lastSeq + 1).then((res) => {
        if (!res.ok) {
          const box = document.createElement('div');
          box.className = 'error-box';
          box.textContent = `princess:op:replay → ${res.error.code}: ${res.error.message}`;
          root.appendChild(box);
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
      for (const { result } of mounted) {
        result.destroy?.();
      }
    },
  };
}
