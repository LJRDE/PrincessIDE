/**
 * Debug panel — BUG-008 minimal subset.
 *
 * Renders five sub-panels for the debug workflow:
 *   1. Attach form (host/port/symbols) → `princess:debug:attach`
 *   2. Breakpoint list (add/remove/toggle) → `princess:debug:setBreakpoints`
 *   3. Continue button → `princess:debug:continue`
 *   4. Register view → `princess:debug:registers`
 *   5. Stack trace view → `princess:debug:stackTrace`
 *
 * All IPC calls are injectable (D17 pattern from `lsp/client.ts` and
 * `liveStream.ts`): the component receives callbacks, not raw IPC functions,
 * so vitest can test without a real QEMU/gdb.
 *
 * Error display follows contract §0.4: failures are explicit, never blank,
 * never silent.  Engine errors (E_QEMU_FAILED, E_NOT_FOUND, etc.) are shown
 * verbatim.
 */

import type { IdeState } from '../state/eventStore.js';
import type { IpcError, IpcResult } from '../contract/ipc.js';
import type {
  DebugAttachData,
  DebugContinueData,
  DebugRegistersData,
  DebugSetBreakpointsData,
  DebugStackTraceData,
  DebugStackFrame,
  DebugRegisterValue,
} from '../contract/ipc.js';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

function showError(container: HTMLElement, label: string, err: IpcError): void {
  const box = el('div', 'error-box debug-error');
  box.dataset['testid'] = 'debug-error';
  box.textContent = `${label} → ${err.code}: ${err.message}`;
  container.appendChild(box);
}

// ---------------------------------------------------------------------------
// Injectable IPC function signatures (D17 pattern)
// ---------------------------------------------------------------------------

export type AttachFn = (args: { host: string; port: number; symbols: string }) => Promise<IpcResult<DebugAttachData>>;
export type SetBreakpointsFn = (
  breakpoints: { file: string; line: number; enabled?: boolean }[],
) => Promise<IpcResult<DebugSetBreakpointsData>>;
export type ContinueFn = () => Promise<IpcResult<DebugContinueData>>;
export type StackTraceFn = () => Promise<IpcResult<DebugStackTraceData>>;
export type RegistersFn = () => Promise<IpcResult<DebugRegistersData>>;

export interface DebugPanelCallbacks {
  attach?: AttachFn;
  setBreakpoints?: SetBreakpointsFn;
  continue?: ContinueFn;
  stackTrace?: StackTraceFn;
  registers?: RegistersFn;
}

// ---------------------------------------------------------------------------
// Breakpoint list persisted locally (the engine state arrives via events, but
// the UI also needs to track which breakpoints the user wants to set/remove).
// ---------------------------------------------------------------------------

export interface BreakpointEntry {
  id: number;
  file: string;
  line: number;
  enabled: boolean;
  verified: boolean;
  location: string;
}

// ---------------------------------------------------------------------------
// Main render
// ---------------------------------------------------------------------------

/**
 * Render the full debug panel into `root`.
 *
 * `state` provides the current event-stream state (breakpoints from events,
 * active flag, stops).  `callbacks` provide injectable IPC calls.
 */
export function renderDebugPanel(
  root: HTMLElement,
  state: IdeState,
  callbacks: DebugPanelCallbacks = {},
): void {
  root.textContent = '';
  root.dataset['testid'] = 'debug-panel';

  // --- Section 1: Attach form ------------------------------------------------
  const attachSection = el('div', 'debug-section debug-attach');
  const attachTitle = el('h3');
  attachTitle.textContent = 'Attach to QEMU gdbstub';
  attachSection.appendChild(attachTitle);

  const hostInput = el('input', 'debug-input');
  hostInput.dataset['testid'] = 'debug-host-input';
  hostInput.setAttribute('type', 'text');
  hostInput.setAttribute('placeholder', 'Host (default: 127.0.0.1)');
  hostInput.setAttribute('value', '127.0.0.1');

  const portInput = el('input', 'debug-input');
  portInput.dataset['testid'] = 'debug-port-input';
  portInput.setAttribute('type', 'number');
  portInput.setAttribute('placeholder', 'Port (default: 1234)');
  portInput.setAttribute('value', '1234');

  const symbolsInput = el('input', 'debug-input');
  symbolsInput.dataset['testid'] = 'debug-symbols-input';
  symbolsInput.setAttribute('type', 'text');
  symbolsInput.setAttribute('placeholder', 'Symbols path (e.g. build/kernel.elf)');

  const attachBtn = el('button', 'debug-btn attach-btn');
  attachBtn.dataset['testid'] = 'debug-attach-btn';
  attachBtn.textContent = '🔌 Attach';

  const attachStatus = el('div', 'debug-attach-status');
  attachStatus.dataset['testid'] = 'debug-attach-status';

  attachBtn.addEventListener('click', async () => {
    attachStatus.textContent = '';
    const host = hostInput.value.trim() || '127.0.0.1';
    const port = parseInt(portInput.value, 10) || 1234;
    const symbols = symbolsInput.value.trim();

    if (!callbacks.attach) {
      showError(attachStatus, 'Attach', {
        code: 'E_INTERNAL',
        message: 'No attach handler configured',
        detail: '',
      });
      return;
    }

    const res = await callbacks.attach({ host, port, symbols });
    if (res.ok) {
      attachStatus.className = 'debug-attach-status ok';
      attachStatus.dataset['testid'] = 'debug-attach-status';
      attachStatus.textContent = `Attached: ${res.data.backend} / ${res.data.protocol} @ ${res.data.host}:${res.data.port}`;
      if (res.data.note) {
        const noteSpan = el('span', 'muted');
        noteSpan.textContent = ` (${res.data.note})`;
        attachStatus.appendChild(noteSpan);
      }
    } else {
      showError(attachStatus, 'Attach', res.error);
    }
  });

  attachSection.append(hostInput, portInput, symbolsInput, attachBtn, attachStatus);
  root.appendChild(attachSection);

  // --- Section 2: Breakpoint list --------------------------------------------
  const bpSection = el('div', 'debug-section debug-breakpoints');
  const bpTitle = el('h3');
  bpTitle.textContent = 'Breakpoints';
  bpSection.appendChild(bpTitle);

  // Add breakpoint form
  const bpForm = el('div', 'debug-bp-form');
  const bpFileInput = el('input', 'debug-input');
  bpFileInput.dataset['testid'] = 'debug-bp-file-input';
  bpFileInput.setAttribute('type', 'text');
  bpFileInput.setAttribute('placeholder', 'File (e.g. kernel.c)');
  const bpLineInput = el('input', 'debug-input');
  bpLineInput.dataset['testid'] = 'debug-bp-line-input';
  bpLineInput.setAttribute('type', 'number');
  bpLineInput.setAttribute('placeholder', 'Line');
  const bpAddBtn = el('button', 'debug-btn bp-add-btn');
  bpAddBtn.dataset['testid'] = 'debug-bp-add-btn';
  bpAddBtn.textContent = '+ Add';
  bpForm.append(bpFileInput, bpLineInput, bpAddBtn);
  bpSection.appendChild(bpForm);

  const bpError = el('div', 'debug-bp-error');
  bpError.dataset['testid'] = 'debug-bp-error';
  bpSection.appendChild(bpError);

  // Local breakpoint list (from event state)
  const bpList = el('div', 'debug-bp-list');
  bpList.dataset['testid'] = 'debug-bp-list';

  const breakpoints = Object.values(state.debug.breakpoints).sort((a, b) => a.id - b.id);

  for (const bp of breakpoints) {
    const row = el('div', 'debug-bp-item');
    row.dataset['testid'] = 'debug-bp-item';
    row.dataset['id'] = String(bp.id);

    const verified = bp.verified ? '✓' : '?';
    const verifiedCls = bp.verified ? 'verified' : 'unverified';
    const status = el('span', `bp-status ${verifiedCls}`);
    status.textContent = verified;

    const label = el('span', 'bp-label');
    label.textContent = `#${bp.id} ${bp.location}`;

    row.append(status, label);
    bpList.appendChild(row);
  }

  if (breakpoints.length === 0) {
    const empty = el('p', 'muted');
    empty.textContent = 'No breakpoints set';
    bpList.appendChild(empty);
  }

  bpSection.appendChild(bpList);

  bpAddBtn.addEventListener('click', async () => {
    bpError.textContent = '';
    const file = bpFileInput.value.trim();
    const line = parseInt(bpLineInput.value, 10);

    if (!file || isNaN(line) || line <= 0) {
      showError(bpError, 'Set breakpoints', {
        code: 'E_INVALID_CONFIG',
        message: 'Please provide a valid file and line number',
        detail: '',
      });
      return;
    }

    if (!callbacks.setBreakpoints) {
      showError(bpError, 'Set breakpoints', {
        code: 'E_INTERNAL',
        message: 'No setBreakpoints handler configured',
        detail: '',
      });
      return;
    }

    const res = await callbacks.setBreakpoints([{ file, line }]);
    if (res.ok) {
      // Clear inputs on success
      bpFileInput.value = '';
      bpLineInput.value = '';
    } else {
      showError(bpError, 'Set breakpoints', res.error);
    }
  });

  root.appendChild(bpSection);

  // --- Section 3: Continue button --------------------------------------------
  const continueSection = el('div', 'debug-section debug-continue');
  const continueBtn = el('button', 'debug-btn continue-btn');
  continueBtn.dataset['testid'] = 'debug-continue-btn';
  continueBtn.textContent = '▶ Continue';
  const continueStatus = el('div', 'debug-continue-status');
  continueStatus.dataset['testid'] = 'debug-continue-status';

  continueBtn.addEventListener('click', async () => {
    continueStatus.textContent = '';
    if (!callbacks.continue) {
      showError(continueStatus, 'Continue', {
        code: 'E_INTERNAL',
        message: 'No continue handler configured',
        detail: '',
      });
      return;
    }
    const res = await callbacks.continue();
    if (res.ok) {
      continueStatus.className = 'debug-continue-status ok';
      continueStatus.textContent = 'Continued';
    } else {
      showError(continueStatus, 'Continue', res.error);
    }
  });

  continueSection.append(continueBtn, continueStatus);
  root.appendChild(continueSection);

  // --- Section 4: Registers --------------------------------------------------
  const regsSection = el('div', 'debug-section debug-registers');
  const regsTitle = el('h3');
  regsTitle.textContent = 'Registers';
  regsSection.appendChild(regsTitle);

  const regsRefreshBtn = el('button', 'debug-btn regs-refresh-btn');
  regsRefreshBtn.dataset['testid'] = 'debug-regs-refresh-btn';
  regsRefreshBtn.textContent = '🔄 Refresh Registers';
  regsSection.appendChild(regsRefreshBtn);

  const regsContainer = el('div', 'debug-regs-container');
  regsContainer.dataset['testid'] = 'debug-regs-container';
  regsSection.appendChild(regsContainer);

  regsRefreshBtn.addEventListener('click', async () => {
    regsContainer.textContent = '';
    if (!callbacks.registers) {
      showError(regsContainer, 'Registers', {
        code: 'E_INTERNAL',
        message: 'No registers handler configured',
        detail: '',
      });
      return;
    }
    const res = await callbacks.registers();
    if (res.ok) {
      renderRegisters(regsContainer, res.data.registers);
    } else {
      showError(regsContainer, 'Registers', res.error);
    }
  });

  // Show initial registers from the latest debug.stopped event (if any)
  if (state.debug.stops.length > 0) {
    const lastStop = state.debug.stops[state.debug.stops.length - 1];
    const regsFromEvent = Object.entries(lastStop.regs).map(([name, value]) => ({
      name,
      value,
    }));
    if (regsFromEvent.length > 0) {
      renderRegisters(regsContainer, regsFromEvent);
    }
  }

  root.appendChild(regsSection);

  // --- Section 5: Stack trace ------------------------------------------------
  const stackSection = el('div', 'debug-section debug-stacktrace');
  const stackTitle = el('h3');
  stackTitle.textContent = 'Stack Trace';
  stackSection.appendChild(stackTitle);

  const stackRefreshBtn = el('button', 'debug-btn stack-refresh-btn');
  stackRefreshBtn.dataset['testid'] = 'debug-stack-refresh-btn';
  stackRefreshBtn.textContent = '🔄 Refresh Stack';
  stackSection.appendChild(stackRefreshBtn);

  const stackContainer = el('div', 'debug-stack-container');
  stackContainer.dataset['testid'] = 'debug-stack-container';
  stackSection.appendChild(stackContainer);

  stackRefreshBtn.addEventListener('click', async () => {
    stackContainer.textContent = '';
    if (!callbacks.stackTrace) {
      showError(stackContainer, 'Stack trace', {
        code: 'E_INTERNAL',
        message: 'No stackTrace handler configured',
        detail: '',
      });
      return;
    }
    const res = await callbacks.stackTrace();
    if (res.ok) {
      renderStackTrace(stackContainer, res.data.frames);
    } else {
      showError(stackContainer, 'Stack trace', res.error);
    }
  });

  // Show initial stack from the latest debug.stopped event (if any)
  if (state.debug.stops.length > 0) {
    const lastStop = state.debug.stops[state.debug.stops.length - 1];
    renderStackTrace(stackContainer, [{
      id: 0,
      name: lastStop.frame.name,
      file: lastStop.frame.file,
      line: lastStop.frame.line,
      pc: lastStop.frame.pc,
    }]);
  }

  root.appendChild(stackSection);
}

// ---------------------------------------------------------------------------
// Sub-renderers
// ---------------------------------------------------------------------------

function renderRegisters(container: HTMLElement, registers: DebugRegisterValue[]): void {
  const table = el('table', 'debug-regs-table');
  table.dataset['testid'] = 'debug-regs-table';

  const thead = el('thead');
  const headerRow = el('tr');
  const thName = el('th');
  thName.textContent = 'Register';
  const thValue = el('th');
  thValue.textContent = 'Value';
  headerRow.append(thName, thValue);
  thead.appendChild(headerRow);
  table.appendChild(thead);

  const tbody = el('tbody');
  for (const reg of registers) {
    const row = el('tr', 'debug-reg-row');
    row.dataset['testid'] = 'debug-reg-row';
    const nameCell = el('td', 'reg-name');
    nameCell.textContent = reg.name;
    const valueCell = el('td', 'reg-value mono');
    valueCell.textContent = reg.value;
    row.append(nameCell, valueCell);
    tbody.appendChild(row);
  }
  table.appendChild(tbody);
  container.appendChild(table);
}

/**
 * Render a stack trace as `function @ file:line` rows.
 * This is the contract-specified format for the stack trace view.
 */
function renderStackTrace(container: HTMLElement, frames: DebugStackFrame[]): void {
  if (frames.length === 0) {
    const empty = el('p', 'muted');
    empty.textContent = 'No stack frames';
    container.appendChild(empty);
    return;
  }

  for (const frame of frames) {
    const row = el('div', 'debug-stack-frame');
    row.dataset['testid'] = 'debug-stack-frame';

    const nameSpan = el('span', 'stack-fn');
    nameSpan.textContent = frame.name;

    const atSpan = el('span', 'stack-at');
    atSpan.textContent = ' @ ';

    const locSpan = el('span', 'stack-loc mono');
    locSpan.textContent = `${frame.file}:${frame.line}`;

    const pcSpan = el('span', 'stack-pc muted');
    pcSpan.textContent = ` (pc=${frame.pc})`;

    row.append(nameSpan, atSpan, locSpan, pcSpan);
    container.appendChild(row);
  }
}
