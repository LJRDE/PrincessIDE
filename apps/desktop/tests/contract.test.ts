/**
 * P3-5 — the frontend's command names must line up with
 * docs/spec/10-contracts.md §3, item by item.
 *
 * The real comparison lives in apps/desktop/scripts/check-contract.mjs (also
 * runnable standalone as `pnpm check:contract`); this test makes it part of
 * `vitest run` so a contract drift fails the test suite too.
 */

import { describe, expect, it } from 'vitest';
import {
  CONTRACT_DOMAINS,
  PENDING_CONTRACT_COMMANDS,
  checkContract,
  extractSpecCommands,
  formatReport,
  readSpec,
} from '../scripts/check-contract.mjs';

const report = checkContract();

describe('IPC contract alignment (docs/spec/10-contracts.md §3)', () => {
  it('passes every alignment check', () => {
    if (!report.ok) {
      // Print the full report on failure so the diff is visible in CI logs.
      throw new Error(`contract misaligned:\n${formatReport(report)}`);
    }
    expect(report.ok).toBe(true);
  });

  it('extracts the v1 minimal set from the spec itself (not from a copy)', () => {
    expect(report.specCommands.length).toBeGreaterThanOrEqual(20);
    expect(report.specCommands).toContain('princess:tools:detect');
    expect(report.specCommands).toContain('princess:op:cancel');
    expect(report.specCommands).toContain('princess:op:replay');
    // The spec's shorthand line must expand to one command per action.
    for (const action of [
      'attach',
      'setBreakpoints',
      'continue',
      'stepOver',
      'stepInto',
      'stackTrace',
      'scopes',
      'variables',
      'readMemory',
      'writeMemory',
      'disassemble',
      'registers',
    ]) {
      expect(report.specCommands).toContain(`princess:debug:${action}`);
    }
  });

  it('does not mistake prose identifiers for commands', () => {
    // §3 says: `princess:op:cancel`（统一取消入口，参数 `opId`）
    expect(report.specCommands).not.toContain('princess:op:opId');
    expect(report.specCommands).not.toContain('princess:op:domain');
  });

  it('expands the shorthand rule deterministically', () => {
    const synthetic = [
      '## 3. IPC 命令契约',
      '- `princess:debug:attach` / `stepOver` / `continue`',
      '- `princess:op:cancel`（参数 `opId`）',
      '',
      '## 4. next',
    ].join('\n');
    expect(extractSpecCommands(synthetic)).toEqual([
      'princess:debug:attach',
      'princess:debug:stepOver',
      'princess:debug:continue',
      'princess:op:cancel',
    ]);
  });

  it('agrees with the TypeScript mirror', () => {
    expect(report.tsCommands).toEqual(report.specCommands);
  });

  it('agrees with the Rust engine registry', () => {
    expect(report.rustCommands).toEqual(report.specCommands);
  });

  it('uses a closed set of domains and error codes', () => {
    for (const cmd of report.tsCommands) {
      const [, domain] = cmd.split(':');
      expect(CONTRACT_DOMAINS).toContain(domain);
    }
    expect(report.tsCodes).toEqual(report.specCodes);
    expect(report.rustCodes).toEqual(report.specCodes);
    expect(report.specCodes).toHaveLength(10);
  });

  it('only uses contract commands in the frontend', () => {
    for (const [cmd, where] of report.literals) {
      const allowed =
        report.tsCommands.includes(cmd) || PENDING_CONTRACT_COMMANDS.includes(cmd);
      expect(allowed, `${cmd} used at ${where.join(', ')} is not in §3`).toBe(true);
    }
    // The pending-amendment mechanism is kept but empty: the D17 amendment
    // ratified the lsp commands, so nothing is waved through any more.
    expect(PENDING_CONTRACT_COMMANDS).toEqual([]);
    expect(report.tsCommands).not.toContain('princess:lsp:bridge');
  });

  it('covers the lsp domain added by the D17 amendment', () => {
    for (const cmd of ['princess:lsp:start', 'princess:lsp:send', 'princess:lsp:stop']) {
      expect(report.specCommands).toContain(cmd);
      expect(report.tsCommands).toContain(cmd);
      expect(report.rustCommands).toContain(cmd);
    }
    // 24 at the D17 amendment, +2 for the §3 fs commands that made the editor
    // able to open and save real files.  Pinned so a command can never appear
    // in the spec without this test noticing.
    expect(report.specCommands).toHaveLength(26);
  });

  it('covers the fs domain the editor needs to open and save files', () => {
    for (const cmd of ['princess:fs:read', 'princess:fs:write']) {
      expect(report.specCommands).toContain(cmd);
      expect(report.tsCommands).toContain(cmd);
      expect(report.rustCommands).toContain(cmd);
    }
  });

  it('is not confused by prose, apostrophes or Rust type annotations', () => {
    // Regression: a comment containing an apostrophe ("the editor's …") used to
    // pair with the next literal's opening quote and swallow the command after
    // it, and Rust's `const X: &[&str] = &[` used to fool bracket detection.
    expect(report.tsCommands).toHaveLength(report.specCommands.length);
    expect(report.rustCommands).toHaveLength(report.specCommands.length);
    for (const cmd of [...report.tsCommands, ...report.rustCommands]) {
      expect(cmd).toMatch(/^princess:[a-z]+:[A-Za-z][A-Za-z0-9]*$/);
    }
  });

  it('reads the spec from disk (guards against a stale embedded list)', () => {
    const spec = readSpec();
    expect(spec).toContain('## 3. IPC 命令契约');
    expect(extractSpecCommands(spec)).toHaveLength(report.specCommands.length);
  });
});
