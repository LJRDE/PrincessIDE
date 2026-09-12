// @vitest-environment jsdom
/**
 * BUG-003 tests: native directory selector (tauri-plugin-dialog) via injectable
 * DirSelectorFn (D17 pattern).
 *
 * Two branches:
 *   1. Browser preview (no DirSelectorFn): only text input, no "Browse…" button.
 *   2. Tauri + injected selector: clicking "Browse…" calls the selector, fills
 *      the input, and triggers `onProjectOpen` with the selected path.
 *   3. Cancellation (selector returns null): no `onProjectOpen` call, no error.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  renderActionPanel,
  type DirSelectorFn,
} from '../../src/components/actionPanel.js';
import { createInitialState } from '../../src/state/eventStore.js';

let host: HTMLElement;

beforeEach(() => {
  document.body.textContent = '';
  host = document.createElement('div');
  document.body.appendChild(host);
});

const state = createInitialState();

describe('BUG-003: project path input (browser preview)', () => {
  it('renders the text input without a Browse button when no dirSelectorFn is given', () => {
    renderActionPanel(host, state);
    expect(host.querySelector('[data-testid="project-path-input"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="browse-btn"]')).toBeNull();
  });

  it('Open Project button triggers onProjectOpen with the input value', () => {
    const onProjectOpen = vi.fn();
    renderActionPanel(host, state, { onProjectOpen });

    const input = host.querySelector<HTMLInputElement>('[data-testid="project-path-input"]')!;
    input.value = '/some/project';

    const btn = host.querySelector<HTMLButtonElement>('[data-testid="project-open-btn"]')!;
    btn.click();

    expect(onProjectOpen).toHaveBeenCalledWith('/some/project');
  });

  it('does not call onProjectOpen when the input is empty', () => {
    const onProjectOpen = vi.fn();
    renderActionPanel(host, state, { onProjectOpen });

    const btn = host.querySelector<HTMLButtonElement>('[data-testid="project-open-btn"]')!;
    btn.click();

    expect(onProjectOpen).not.toHaveBeenCalled();
  });
});

describe('BUG-003: native directory selector (Tauri shell)', () => {
  it('renders the Browse button when dirSelectorFn is provided', () => {
    const selector: DirSelectorFn = vi.fn(async () => null);
    renderActionPanel(host, state, {}, { dirSelectorFn: selector });
    expect(host.querySelector('[data-testid="browse-btn"]')).not.toBeNull();
  });

  it('clicking Browse fills the input and calls onProjectOpen with the selected path', async () => {
    const selectedPath = '/home/user/my-kernel';
    const selector: DirSelectorFn = vi.fn(async () => selectedPath);
    const onProjectOpen = vi.fn();

    renderActionPanel(host, state, { onProjectOpen }, { dirSelectorFn: selector });

    const browseBtn = host.querySelector<HTMLButtonElement>('[data-testid="browse-btn"]')!;
    browseBtn.click();

    // The selector is async, so we need to wait for the microtask.
    await vi.waitFor(() => {
      expect(selector).toHaveBeenCalledTimes(1);
    });

    const input = host.querySelector<HTMLInputElement>('[data-testid="project-path-input"]')!;
    expect(input.value).toBe(selectedPath);
    expect(onProjectOpen).toHaveBeenCalledWith(selectedPath);
  });

  it('cancelling the selector (returns null) keeps the current value and does not call onProjectOpen', async () => {
    const selector: DirSelectorFn = vi.fn(async () => null);
    const onProjectOpen = vi.fn();

    renderActionPanel(host, state, { onProjectOpen }, { dirSelectorFn: selector });

    const input = host.querySelector<HTMLInputElement>('[data-testid="project-path-input"]')!;
    input.value = '/existing/path';

    const browseBtn = host.querySelector<HTMLButtonElement>('[data-testid="browse-btn"]')!;
    browseBtn.click();

    await vi.waitFor(() => {
      expect(selector).toHaveBeenCalledTimes(1);
    });

    // Input value unchanged
    expect(input.value).toBe('/existing/path');
    // onProjectOpen was NOT called
    expect(onProjectOpen).not.toHaveBeenCalled();
  });
});
