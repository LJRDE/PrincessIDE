/**
 * View registry — each IDE panel self-registers so that `app.ts` can mount
 * all views without hand-maintaining a per-panel list.
 *
 * This module is the structural fix for BUG-001 / BUG-002: a component that
 * exists but is never mounted is now impossible — the registry test asserts
 * that every registered `testid` appears in the DOM.
 *
 * Design constraints:
 *  - No new dependencies (task requirement).
 *  - Each view receives a `host` element (its container) and a `state` object;
 *    it renders into the host.  The host is created by `app.ts` (layout).
 *  - `destroy?()` is optional — most views are stateless re-renders, but the
 *    editor owns a CodeMirror instance that must be torn down.
 *  - The registry is a plain array; ordering is the mount order (top-to-bottom
 *    in the left/right columns).
 */

import type { IdeState } from '../state/eventStore.js';

// ---------------------------------------------------------------------------
// View descriptor — what each panel self-reports
// ---------------------------------------------------------------------------

export interface ViewDescriptor {
  /** Unique id (kebab-case, e.g. "action-panel"). */
  id: string;

  /** Human-readable title shown in the section header. */
  title: string;

  /** `data-testid` value on the view's section element. */
  testid: string;

  /**
   * Mount the view into a host element.
   *
   * `app.ts` creates a `<section>` with the right title/testid and passes
   * the inner body element as `host`.  The view appends its own DOM there.
   *
   * Returns an optional render function that `app.ts` calls on state changes.
   */
  mount(host: HTMLElement): ViewMountResult;
}

export interface ViewMountResult {
  /** Called on every state change to re-render. */
  render(state: IdeState): void;
  /** Optional teardown (e.g. CodeMirror editor). */
  destroy?(): void;
}

// ---------------------------------------------------------------------------
// Registry — populated by each view's registration
// ---------------------------------------------------------------------------

const registry: ViewDescriptor[] = [];

/**
 * Register a view.  Called once per view at module-evaluation time (static
 * import in `app.ts` or in the view file itself).
 */
export function registerView(desc: ViewDescriptor): void {
  registry.push(desc);
}

/**
 * Return a snapshot of all registered views.
 * Order is the registration (mount) order.
 */
export function listViews(): readonly ViewDescriptor[] {
  return registry;
}

// ---------------------------------------------------------------------------
// Built-in views — imported and registered below
// ---------------------------------------------------------------------------

import { renderActionPanel, type ActionPanelCallbacks, type DirSelectorFn } from '../components/actionPanel.js';
import { renderToolTable, type ToolTableState } from '../components/toolTable.js';
import { createEditor } from '../components/editor.js';
import {
  renderDebugPanel as renderEventDebugPanel,
  renderDiagnostics,
  renderEventLog,
  renderEventStatus,
  renderFaultCard,
} from '../components/eventLog.js';
import { renderDebugPanel, type DebugPanelCallbacks } from '../components/debugPanel.js';
import { loadAllBundledFixtures } from '../state/fixtureLoader.js';
import { expectedLanguageService } from '../lsp/client.js';

// --- Action panel -----------------------------------------------------------

registerView({
  id: 'action-panel',
  title: 'Actions',
  testid: 'action-panel-section',
  mount(host) {
    const actionHost = document.createElement('div');
    host.appendChild(actionHost);

    // The action panel requires callbacks + options that are wired at the app
    // level.  We store them here; `app.ts` calls `setActionConfig()`.
    let callbacks: ActionPanelCallbacks = {};
    let options: { dirSelectorFn?: DirSelectorFn } = {};

    return {
      render(state) {
        renderActionPanel(actionHost, state, callbacks, options);
      },
      // Expose for app.ts to wire callbacks after mount.
      get _actionHost() { return actionHost; },
      setCallbacks(c: ActionPanelCallbacks) { callbacks = c; },
      setOptions(o: { dirSelectorFn?: DirSelectorFn }) { options = o; },
    } as ReturnType<ViewDescriptor['mount']> & {
      setCallbacks(c: ActionPanelCallbacks): void;
      setOptions(o: { dirSelectorFn?: DirSelectorFn }): void;
    };
  },
});

// --- Toolchain panel --------------------------------------------------------

registerView({
  id: 'toolchain-panel',
  title: 'Toolchain (princess:tools:detect)',
  testid: 'toolchain-panel',
  mount(host) {
    const toolHost = document.createElement('div');
    host.appendChild(toolHost);
    const toolState: ToolTableState = { data: null, error: null, loading: false };

    return {
      render() {
        renderToolTable(toolHost, toolState);
      },
      get toolState() { return toolState; },
    } as ReturnType<ViewDescriptor['mount']> & { toolState: ToolTableState };
  },
});

// --- Editor panel -----------------------------------------------------------

registerView({
  id: 'editor-panel',
  title: 'Editor (CodeMirror 6)',
  testid: 'editor-panel',
  mount(host) {
    const editorHost = document.createElement('div');
    editorHost.className = 'editor-host';
    host.appendChild(editorHost);

    // LSP status note
    const lspNote = document.createElement('p');
    lspNote.className = 'muted';
    lspNote.dataset['testid'] = 'lsp-status';
    lspNote.textContent = `LSP: not connected · expected ${expectedLanguageService()} · engine-side bridge pending (D17)`;
    lspNote.dataset['server'] = 'PRINCESSIDE_LANG_SERVICE_CLANGD';
    host.appendChild(lspNote);

    const DEMO_SOURCE = `/* PrincessIDE shell demo buffer — fixtures/refkernel/kernel.c shaped.
   Real files are opened by the engine (princess:project:open, later phase). */
#include "serial.h"

void kmain(void) {
    serial_write("PrincessIDE reference kernel booted\\n");
    refkernel_fault_probe();
}
`;
    const editor = createEditor(editorHost, { filename: 'kernel.c', doc: DEMO_SOURCE });

    return {
      render() { /* CodeMirror manages its own state */ },
      destroy() { editor.destroy(); },
    };
  },
});

// --- Event stream panel (status + fixture picker + log + fault + diagnostics + event debug) ---

registerView({
  id: 'events-panel',
  title: 'Event stream (contract §2)',
  testid: 'events-panel',
  mount(host) {
    const statusBar = document.createElement('div');
    statusBar.className = 'statusbar';
    host.appendChild(statusBar);

    // Fixture picker (was in app.ts, moved here as it's part of the events panel)
    const fixturePicker = document.createElement('select');
    fixturePicker.dataset['testid'] = 'fixture-picker';
    const fixtures = loadAllBundledFixtures();
    for (const f of fixtures) {
      const opt = document.createElement('option');
      opt.value = f.name;
      opt.textContent = `${f.name}${f.constructed ? ' (P3-constructed)' : ''}`;
      fixturePicker.appendChild(opt);
    }
    host.appendChild(fixturePicker);

    const logHost = document.createElement('div');
    logHost.className = 'log';
    host.appendChild(logHost);

    const faultHost = document.createElement('div');
    faultHost.className = 'fault-card';
    host.appendChild(faultHost);

    const diagHost = document.createElement('div');
    diagHost.className = 'diagnostics';
    host.appendChild(diagHost);

    const debugHost = document.createElement('div');
    debugHost.className = 'debug-panel';
    host.appendChild(debugHost);

    return {
      render(state) {
        renderEventStatus(statusBar, state);
        renderEventLog(logHost, state);
        renderFaultCard(faultHost, state);
        renderDiagnostics(diagHost, state);
        renderEventDebugPanel(debugHost, state);
      },
    };
  },
});

// --- Debug panel (BUG-008) --------------------------------------------------

registerView({
  id: 'debug-panel',
  title: 'Debug (BUG-008)',
  testid: 'debug-panel-section',
  mount(host) {
    const debugPanelHost = document.createElement('div');
    host.appendChild(debugPanelHost);

    let callbacks: DebugPanelCallbacks = {
      attach: async () => ({ ok: false as const, error: { code: 'E_INTERNAL', message: 'not wired', detail: '' } }),
      setBreakpoints: async () => ({ ok: false as const, error: { code: 'E_INTERNAL', message: 'not wired', detail: '' } }),
      continue: async () => ({ ok: false as const, error: { code: 'E_INTERNAL', message: 'not wired', detail: '' } }),
      stackTrace: async () => ({ ok: false as const, error: { code: 'E_INTERNAL', message: 'not wired', detail: '' } }),
      registers: async () => ({ ok: false as const, error: { code: 'E_INTERNAL', message: 'not wired', detail: '' } }),
    };

    return {
      render(state) {
        renderDebugPanel(debugPanelHost, state, callbacks);
      },
      setCallbacks(c: DebugPanelCallbacks) { callbacks = c; },
    } as ReturnType<ViewDescriptor['mount']> & { setCallbacks(c: DebugPanelCallbacks): void };
  },
});
