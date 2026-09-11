/**
 * Action panel — build/run buttons and status display.
 *
 * Renders the project controls (Build, Run, Stop) and their live status
 * from the event stream.  Buttons invoke the IPC commands; the state
 * machine (`eventStore.ts`) handles the event feedback.
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

export interface ActionPanelCallbacks {
  onBuild?: () => void;
  onRun?: () => void;
  onStopBuild?: () => void;
  onStopRun?: () => void;
  onProjectOpen?: () => void;
}

/**
 * Render the action panel with build/run controls and status.
 */
export function renderActionPanel(
  root: HTMLElement,
  state: IdeState,
  callbacks: ActionPanelCallbacks = {},
): void {
  root.textContent = '';
  root.dataset['testid'] = 'action-panel';

  // Project status
  const projectRow = el('div', 'action-row project-status');
  const projectBtn = el('button', 'action-btn project-btn');
  projectBtn.dataset['testid'] = 'project-open-btn';
  projectBtn.textContent = '📂 Open Project';
  projectBtn.addEventListener('click', () => callbacks.onProjectOpen?.());
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
