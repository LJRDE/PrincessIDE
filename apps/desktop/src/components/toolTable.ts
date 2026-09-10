/**
 * Toolchain table panel — renders `princess:tools:detect` (§3) output.
 *
 * Security/hygiene rule: engine output is untrusted text.  Everything is
 * written with `textContent`, never `innerHTML`, so a weird PATH entry cannot
 * inject markup into the IDE shell.
 *
 * Fidelity rule: the raw `status` and `checks` columns are rendered verbatim.
 * `scripts/doctor.sh` can report `MISSING` **and** `WRONG VER`, and it can print a
 * verification note (`version matches /clangd version 16\./`) instead of a
 * version — the UI shows what the doctor said rather than flattening it into a
 * single green/red dot.
 */

import type { IpcError, ToolInfo, ToolsDetectData } from '../contract/ipc.js';

export interface ToolTableState {
  data: ToolsDetectData | null;
  error: IpcError | null;
  loading: boolean;
}

export function createToolTableState(): ToolTableState {
  return { data: null, error: null, loading: false };
}

/** The render model, kept separate from the DOM so it is trivially testable. */
export interface ToolRow {
  name: string;
  /** Raw status from doctor.sh, verbatim (`ok`, `MISSING`, `WRONG VER`, …). */
  status: string;
  available: boolean;
  version: string;
  path: string;
  required: boolean;
  checks: string[];
}

export function toRows(data: ToolsDetectData | null): ToolRow[] {
  if (!data) return [];
  return data.tools.map((t: ToolInfo) => ({
    name: t.name,
    status: t.status,
    available: t.available,
    version: t.version ?? '(no version printed)',
    // `path: null` on an available tool means doctor verified it without
    // printing a path — which is not the same as "not resolved".
    path: t.path ?? (t.available ? '(path not printed by doctor)' : '(not resolved)'),
    required: t.required,
    checks: t.checks ?? [],
  }));
}

export function renderToolTable(root: HTMLElement, state: ToolTableState): void {
  root.textContent = '';
  root.dataset['testid'] = 'tool-table';

  const heading = document.createElement('h2');
  heading.textContent = 'Toolchain';
  root.appendChild(heading);

  if (state.loading) {
    const p = document.createElement('p');
    p.className = 'muted';
    p.dataset['testid'] = 'tool-table-loading';
    p.textContent = 'princess:tools:detect …';
    root.appendChild(p);
    return;
  }

  if (state.error) {
    const box = document.createElement('div');
    box.className = 'error-box';
    box.dataset['testid'] = 'tool-table-error';
    box.textContent = `${state.error.code}: ${state.error.message}\n${state.error.detail}`;
    root.appendChild(box);
    return;
  }

  if (!state.data) {
    const p = document.createElement('p');
    p.className = 'muted';
    p.textContent = 'not detected yet';
    root.appendChild(p);
    return;
  }

  const summary = document.createElement('p');
  summary.className = 'muted';
  summary.dataset['testid'] = 'tool-table-summary';
  summary.textContent =
    `doctor.sh exit ${state.data.exitCode} · ${state.data.tools.length} tools · ` +
    (state.data.missing.length ? `missing: ${state.data.missing.join(', ')}` : 'no missing required tools');
  root.appendChild(summary);

  const table = document.createElement('table');
  table.className = 'tool-table';
  const thead = document.createElement('thead');
  const headRow = document.createElement('tr');
  for (const label of ['tool', 'status', 'version', 'path', 'checks']) {
    const th = document.createElement('th');
    th.textContent = label;
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = document.createElement('tbody');
  for (const row of toRows(state.data)) {
    const tr = document.createElement('tr');
    tr.dataset['tool'] = row.name;
    tr.dataset['status'] = row.status;
    tr.dataset['available'] = String(row.available);

    const name = document.createElement('td');
    name.textContent = row.name + (row.required ? '' : ' (optional)');

    const status = document.createElement('td');
    status.textContent = row.status;
    status.className = row.available ? 'ok' : 'missing';

    const version = document.createElement('td');
    version.textContent = row.version;

    const path = document.createElement('td');
    path.className = 'mono';
    path.textContent = row.path;

    const checks = document.createElement('td');
    checks.className = 'mono';
    checks.textContent = row.checks.join('; ');

    tr.append(name, status, version, path, checks);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  root.appendChild(table);

  const cmd = document.createElement('details');
  const cmdSummary = document.createElement('summary');
  cmdSummary.textContent = 'engine command + raw doctor.sh output';
  const pre = document.createElement('pre');
  pre.dataset['testid'] = 'tool-raw';
  pre.textContent = `$ ${state.data.command}\n${state.data.rawStdout}${state.data.rawStderr}`;
  cmd.append(cmdSummary, pre);
  root.appendChild(cmd);
}
