/**
 * Event-stream panel: renders the state produced by `src/state/eventStore.ts`.
 *
 * The panel is append-only over the log ring (contract §2: "前端只做追加渲染，
 * 不在事件流上做有损变换") and surfaces contract-level alarms — `seq` gaps,
 * duplicates, and event-model version mismatch — instead of hiding them.
 */

import type { IdeState } from '../state/eventStore.js';
import { deriveSummary } from '../state/eventStore.js';

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

function chip(label: string, value: string, cls = ''): HTMLElement {
  const span = el('span', `chip ${cls}`.trim());
  span.textContent = `${label}: ${value}`;
  return span;
}

/** Header strip: connection/health of the event stream itself. */
export function renderEventStatus(root: HTMLElement, state: IdeState): void {
  root.textContent = '';
  root.dataset['testid'] = 'event-status';
  const s = deriveSummary(state);

  root.appendChild(chip('events', String(s.events)));
  root.appendChild(chip('lastSeq', String(s.lastSeq)));
  root.appendChild(chip('gaps', String(s.gaps), s.gaps > 0 ? 'bad' : 'ok'));
  if (state.duplicates > 0) root.appendChild(chip('duplicates', String(state.duplicates), 'warn'));
  if (state.outOfOrder > 0) root.appendChild(chip('outOfOrder', String(state.outOfOrder), 'warn'));
  if (s.versionWarning) root.appendChild(chip('WARNING', s.versionWarning, 'bad'));
  if (s.needsReplay) {
    const btn = el('button', 'warn');
    btn.dataset['testid'] = 'request-replay';
    btn.textContent = `request replay from seq ${state.lastSeq + 1}`;
    root.appendChild(btn);
  }
  if (s.buildStatus) root.appendChild(chip('build', s.buildStatus, s.buildStatus === 'ok' ? 'ok' : 'warn'));
  if (s.runReason) root.appendChild(chip('run', s.runReason, s.runReason === 'guest-shutdown' ? 'ok' : 'warn'));
  if (state.artifact) root.appendChild(chip('artifact', state.artifact.path));
  if (state.symbols) root.appendChild(chip('symbols', String(state.symbols.symbolCount)));
}

/** Append-only log view, one element per `log.append` chunk. */
export function renderEventLog(root: HTMLElement, state: IdeState, filter?: string): void {
  root.textContent = '';
  root.dataset['testid'] = 'event-log';
  const lines = filter ? state.logs.filter((l) => l.stream === filter) : state.logs;

  for (const line of lines) {
    const row = el('div', `log-line stream-${line.stream.replace(/\./g, '-')}`);
    row.dataset['seq'] = String(line.seq);
    const tag = el('span', 'log-stream');
    tag.textContent = `[${line.stream}]`;
    const text = el('span', 'log-text');
    // Rendered as-is: the engine owns UTF-8 safety and flags lossy chunks.
    text.textContent = line.text;
    row.append(tag, text);
    if (line.lossy) {
      const mark = el('span', 'lossy');
      mark.dataset['testid'] = 'lossy-marker';
      mark.textContent = '(utf8-lossy)';
      row.appendChild(mark);
    }
    root.appendChild(row);
  }
}

/** Diagnostics list from `build.diagnostic`. */
export function renderDiagnostics(root: HTMLElement, state: IdeState): void {
  root.textContent = '';
  root.dataset['testid'] = 'diagnostics';
  const h = el('h3');
  h.textContent = `Diagnostics (${state.diagnostics.length})`;
  root.appendChild(h);
  for (const d of state.diagnostics) {
    const row = el('div', `diag diag-${d.severity}`);
    row.textContent = `${d.file}:${d.line}:${d.col} ${d.severity} [${d.source}] ${d.message}`;
    root.appendChild(row);
  }
}

/** Fault/exit card from `run.fault` + `run.exited` (P2-4/P2-5 evidence in the UI). */
export function renderFaultCard(root: HTMLElement, state: IdeState): void {
  root.textContent = '';
  root.dataset['testid'] = 'fault-card';
  const run = state.run;
  if (!run) {
    const p = el('p', 'muted');
    p.textContent = 'no run yet';
    root.appendChild(p);
    return;
  }
  if (run.fault) {
    const f = run.fault;
    const row = el('div', 'fault');
    row.dataset['testid'] = 'fault';
    row.textContent = `${f.vector} rip=${f.rip} errorCode=${f.errorCode}`;
    root.appendChild(row);
    if (f.symbolicated) {
      const s = el('div', 'symbolicated');
      s.dataset['testid'] = 'fault-symbol';
      s.textContent = `${f.symbolicated.symbol} (${f.symbolicated.file}:${f.symbolicated.line})`;
      root.appendChild(s);
    } else {
      const s = el('div', 'muted');
      s.textContent = 'not symbolicated';
      root.appendChild(s);
    }
  }
  if (run.exit) {
    const row = el('div', `exit reason-${run.exit.reason}`);
    row.dataset['testid'] = 'run-exit';
    row.textContent = `exit ${run.exit.exitCode} · ${run.exit.reason} · ${run.exit.uptimeMs}ms`;
    root.appendChild(row);
  }
}

/**
 * Debug panel: breakpoints, stops and `debug.output` text.
 *
 * The state machine has tracked the `debug.*` kinds since the first replay, but
 * without this panel they were invisible — a state the user cannot see is a bug,
 * not a missing feature.  Kept to the last few stops/outputs because a stepping
 * session can produce thousands of lines.
 */
export function renderDebugPanel(root: HTMLElement, state: IdeState): void {
  root.textContent = '';
  root.dataset['testid'] = 'debug-panel';

  const head = el('h3');
  head.textContent = `Debug (${state.debug.stops.length} stops)`;
  root.appendChild(head);

  const breakpoints = Object.values(state.debug.breakpoints).sort((a, b) => a.id - b.id);
  for (const bp of breakpoints) {
    const row = el('div', 'breakpoint');
    row.dataset['testid'] = 'breakpoint';
    row.dataset['id'] = String(bp.id);
    row.textContent = `${bp.verified ? 'verified' : 'unverified'} #${bp.id} ${bp.location}`;
    root.appendChild(row);
  }

  for (const stop of state.debug.stops.slice(-5)) {
    const row = el('div', `stop reason-${stop.reason}`);
    row.dataset['testid'] = 'debug-stop';
    row.textContent =
      `${stop.reason} · thread ${stop.threadId} · ${stop.frame.name} ` +
      `(${stop.frame.file}:${stop.frame.line}) pc=${stop.frame.pc}`;
    root.appendChild(row);
  }

  for (const out of state.debug.output.slice(-20)) {
    const row = el('div', `debug-out debug-out-${out.category}`);
    row.dataset['testid'] = 'debug-output';
    row.textContent = `[${out.category}] ${out.text}`;
    root.appendChild(row);
  }

  if (breakpoints.length === 0 && state.debug.stops.length === 0 && state.debug.output.length === 0) {
    const p = el('p', 'muted');
    p.textContent = 'no debug session';
    root.appendChild(p);
  }
}
