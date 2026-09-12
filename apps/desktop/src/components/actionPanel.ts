/**
 * Action panel — build/run buttons and status display.
 *
 * Renders the project controls (Build, Run, Stop) and their live status
 * from the event stream.  Buttons invoke the IPC commands; the state
 * machine (`eventStore.ts`) handles the event feedback.
 *
 * BUG-003 (method C): The "Browse…" button uses an injectable
 * `DirSelectorFn` (D17 pattern from `lsp/client.ts` and `liveStream.ts`)
 * so that:
 *   - In Tauri: the real `tauri-plugin-dialog` `open({ directory: true })` is injected.
 *   - In browser preview: no selector is rendered (only the text input remains).
 *   - In tests: a fake selector can be injected to exercise both branches.
 */

import type { IdeState } from '../state/eventStore.js';

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  testid?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (testid) node.dataset['testid'] = testid;
  return node;
}

/**
 * A function that opens a native directory picker and returns the selected
 * path, or null if the user cancelled.  Injectable for testability (D17).
 */
export type DirSelectorFn = () => Promise<string | null>;

export interface ActionPanelCallbacks {
  onBuild?: () => void;
  onRun?: () => void;
  onStopBuild?: () => void;
  onStopRun?: () => void;
  onProjectOpen?: (path: string) => void;
}

export interface ActionPanelOptions {
  /**
   * Injected directory selector.  When absent (browser preview), the
   * "Browse…" button is not rendered — only the text input path entry
   * is available.
   */
  dirSelectorFn?: DirSelectorFn;
}

/**
 * Render the action panel with build/run controls and status.
 */
export function renderActionPanel(
  root: HTMLElement,
  state: IdeState,
  callbacks: ActionPanelCallbacks = {},
  options: ActionPanelOptions = {},
): void {
  root.textContent = '';
  root.dataset['testid'] = 'action-panel';

  // Project status — text input (always rendered, usable in browser preview)
  const projectRow = el('div', 'action-row project-status');
  const projectPathInput = el('input', 'project-path-input');
  projectPathInput.dataset['testid'] = 'project-path-input';
  projectPathInput.setAttribute('type', 'text');
  projectPathInput.setAttribute('placeholder', 'Enter project path…');
  projectRow.appendChild(projectPathInput);

  // BUG-003: "Browse…" button — only when a DirSelectorFn is injected (i.e. Tauri).
  if (options.dirSelectorFn) {
    const browseBtn = el('button', 'action-btn browse-btn');
    browseBtn.dataset['testid'] = 'browse-btn';
    browseBtn.textContent = '📂 Browse…';
    browseBtn.addEventListener('click', async () => {
      const selected = await options.dirSelectorFn!();
      if (selected !== null) {
        projectPathInput.value = selected;
        callbacks.onProjectOpen?.(selected);
      }
      // User cancelled → keep current value, no error.
    });
    projectRow.appendChild(browseBtn);
  }

  const projectBtn = el('button', 'action-btn project-btn');
  projectBtn.dataset['testid'] = 'project-open-btn';
  projectBtn.textContent = '📂 Open Project';
  projectBtn.addEventListener('click', () => {
    const path = projectPathInput.value?.trim();
    if (path) callbacks.onProjectOpen?.(path);
  });
  projectRow.appendChild(projectBtn);
  root.appendChild(projectRow);

  // Build controls
  const buildRow = el('div', 'action-row build-controls');
  const buildBtn = el('button', 'action-btn build-btn');
  buildBtn.dataset['testid'] = 'build-btn';

  const isBuilding = state.build?.running;
  if (isBuilding) {
    buildBtn.textContent = '⏹ Cancel Build';
    buildBtn.className = 'action-btn build-btn building';
    buildBtn.addEventListener('click', () => callbacks.onStopBuild?.());
  } else {
    buildBtn.textContent = '🔨 Build';
    buildBtn.className = 'action-btn build-btn';
    buildBtn.addEventListener('click', () => callbacks.onBuild?.());
  }
  buildRow.appendChild(buildBtn);

  // Build status
  if (state.build) {
    const status = el('span', `build-status status-${state.build.status ?? 'running'}`);
    status.dataset['testid'] = 'build-status';
    if (isBuilding) {
      status.textContent = '⟳ building…';
    } else if (state.build.status === 'ok') {
      status.textContent = `✓ built (${state.build.durationMs}ms, ${state.build.artifacts.length} artifact(s))`;
    } else if (state.build.status === 'failed') {
      status.textContent = `✗ failed (exit ${state.build.exitCode})`;
    } else if (state.build.status === 'cancelled') {
      status.textContent = '⊘ cancelled';
    }
    buildRow.appendChild(status);
  }

  // Diagnostic count
  if (state.diagnostics.length > 0) {
    const diagChip = el('span', 'chip warn');
    diagChip.textContent = `${state.diagnostics.length} diagnostic(s)`;
    buildRow.appendChild(diagChip);
  }

  root.appendChild(buildRow);

  // Run controls
  const runRow = el('div', 'action-row run-controls');
  const runBtn = el('button', 'action-btn run-btn');
  runBtn.dataset['testid'] = 'run-btn';

  const isRunning = state.run?.running;
  if (isRunning) {
    runBtn.textContent = '⏹ Stop';
    runBtn.className = 'action-btn run-btn running';
    runBtn.addEventListener('click', () => callbacks.onStopRun?.());
  } else {
    runBtn.textContent = '▶ Run';
    runBtn.className = 'action-btn run-btn';
    runBtn.addEventListener('click', () => callbacks.onRun?.());
  }
  runRow.appendChild(runBtn);

  // Run status
  if (state.run) {
    const status = el('span', `run-status`);
    status.dataset['testid'] = 'run-status';
    if (isRunning) {
      status.textContent = '⟳ running…';
    } else if (state.run.exit) {
      const reason = state.run.exit.reason;
      const cls = reason === 'guest-shutdown' ? 'ok' : 'warn';
      status.className = `run-status ${cls}`;
      status.textContent = `exit · ${reason} · ${state.run.exit.uptimeMs}ms`;
    }
    runRow.appendChild(status);

    // Fault indicator
    if (state.run.fault) {
      const faultChip = el('span', 'chip bad');
      faultChip.dataset['testid'] = 'fault-indicator';
      const sym = state.run.fault.symbolicated?.symbol;
      faultChip.textContent = sym
        ? `⚠ ${state.run.fault.vector} ${sym}`
        : `⚠ ${state.run.fault.vector} @${state.run.fault.rip}`;
      runRow.appendChild(faultChip);
    }
  }

  root.appendChild(runRow);

  // Debug quick-info (if a debug session is active)
  if (state.debug.active) {
    const debugRow = el('div', 'action-row debug-info');
    const debugChip = el('span', 'chip ok');
    debugChip.textContent = `debug: ${state.debug.stops.length} stop(s), ${Object.keys(state.debug.breakpoints).length} bp(s)`;
    debugRow.appendChild(debugChip);
    root.appendChild(debugRow);
  }
}

/**
 * Render build artifacts as a list.
 */
export function renderArtifactList(root: HTMLElement, state: IdeState): void {
  root.textContent = '';
  root.dataset['testid'] = 'artifact-list';

  if (!state.build || state.build.artifacts.length === 0) {
    const p = el('p', 'muted');
    p.textContent = 'no build artifacts';
    root.appendChild(p);
    return;
  }

  const h = el('h3');
  h.textContent = 'Build Artifacts';
  root.appendChild(h);

  const list = el('ul', 'artifact-list');
  for (const artifact of state.build.artifacts) {
    const li = el('li', 'artifact-item');
    li.dataset['testid'] = 'artifact';
    const pathSpan = el('span', 'artifact-path mono');
    pathSpan.textContent = artifact.path;
    const kindSpan = el('span', 'artifact-kind');
    kindSpan.textContent = `[${artifact.kind}]`;
    const sizeSpan = el('span', 'artifact-size muted');
    sizeSpan.textContent = formatSize(artifact.size);
    li.append(kindSpan, pathSpan, sizeSpan);
    list.appendChild(li);
  }
  root.appendChild(list);
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}
