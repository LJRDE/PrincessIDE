#!/usr/bin/env node
/**
 * P3-5 — IPC contract alignment checker (docs/spec/10-contracts.md §3).
 *
 * Three sources of truth are compared, and *all three* must agree exactly:
 *
 *   1. the frozen spec   docs/spec/10-contracts.md §3  (commands + error codes)
 *   2. the TS frontend   apps/desktop/src/contract/ipc.ts
 *   3. the Rust engine   apps/desktop/src-tauri/src/contract.rs
 *
 * It also scans the whole frontend for `princess:<domain>:<action>` string
 * literals, so a typo inside a component (which no type would catch) fails too.
 *
 * Run:  node apps/desktop/scripts/check-contract.mjs     (exit 0 = aligned)
 * Or:   pnpm -C apps/desktop check:contract
 *
 * The same functions are imported by tests/contract.test.ts, so the check runs
 * in CI as a test as well as on demand as a script.
 */

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const DESKTOP_DIR = path.resolve(HERE, '..');
export const REPO_ROOT = path.resolve(DESKTOP_DIR, '..', '..');

const SPEC = path.join(REPO_ROOT, 'docs', 'spec', '10-contracts.md');
const TS_CONTRACT = path.join(DESKTOP_DIR, 'src', 'contract', 'ipc.ts');
const RUST_CONTRACT = path.join(DESKTOP_DIR, 'src-tauri', 'src', 'contract.rs');
const SRC_DIR = path.join(DESKTOP_DIR, 'src');

/** Domains listed by §3: `princess:<domain>:<action>`. */
export const CONTRACT_DOMAINS = [
  'project',
  'build',
  'run',
  'debug',
  'symbols',
  'bin',
  'fs',
  'tools',
  'ai',
  'op',
  'lsp', // added by the D17 amendment
];

/**
 * Command names that are deliberately *not* in §3 yet and are waiting for a
 * contract amendment.
 *
 * **The list is empty on purpose.**  It existed for exactly one case —
 * `princess:lsp:bridge` — and the D17 amendment replaced that placeholder with
 * three ratified commands (`princess:lsp:start|send|stop`), which now live in §3
 * and are checked like any other command.  The mechanism is kept so a future
 * amendment has a visible, reviewable home; anything not listed here fails the
 * check immediately instead of being waved through.
 */
export const PENDING_CONTRACT_COMMANDS = [];

/** Text of `## 3. IPC 命令契约` up to the next level-2 heading. */
export function specSection3(specText) {
  const start = specText.search(/^## 3\./m);
  if (start < 0) throw new Error('spec §3 heading not found');
  const rest = specText.slice(start);
  const nextMatch = rest.slice(1).search(/^## \d/m);
  return nextMatch < 0 ? rest : rest.slice(0, nextMatch + 1);
}

/**
 * Extract the §3 command list.
 *
 * §3 writes the debug commands in shorthand:
 *   - `princess:debug:attach` / `setBreakpoints` / `continue` / …
 * i.e. a `/`-separated run where the first part carries the prefix and later
 * parts inherit it.  Bare backticked identifiers are only treated as
 * continuations when they are their own `/`-separated part, so prose such as
 * ``参数 `opId` `` (same part as `princess:op:cancel`) is not mistaken for a
 * command.
 */
export function extractSpecCommands(specText) {
  const body = specSection3(specText);
  const found = [];
  const seen = new Set();

  for (const rawLine of body.split('\n')) {
    const line = rawLine.trim();
    if (!line || line.startsWith('#')) continue;

    let domain = null;
    for (const part of line.split('/')) {
      const explicit = [...part.matchAll(/princess:([a-z]+):([A-Za-z][A-Za-z0-9]*)/g)].map(
        (m) => `princess:${m[1]}:${m[2]}`,
      );
      if (explicit.length > 0) {
        for (const cmd of explicit) {
          if (!seen.has(cmd)) {
            seen.add(cmd);
            found.push(cmd);
          }
        }
        domain = explicit[explicit.length - 1].split(':')[1];
        continue;
      }
      if (!domain) continue;
      const bare = part.match(/`([A-Za-z][A-Za-z0-9]*)`/);
      if (bare) {
        const cmd = `princess:${domain}:${bare[1]}`;
        if (!seen.has(cmd)) {
          seen.add(cmd);
          found.push(cmd);
        }
      }
    }
  }
  return found;
}

/** Extract the §3 error-code list (backticked `E_…` tokens). */
export function extractSpecErrorCodes(specText) {
  const body = specSection3(specText);
  const found = [];
  const seen = new Set();
  for (const m of body.matchAll(/`(E_[A-Z_]+)`/g)) {
    if (!seen.has(m[1])) {
      seen.add(m[1]);
      found.push(m[1]);
    }
  }
  return found;
}

/**
 * Pull the string literals out of a named array in a source file.
 *
 * Two details matter, and both were real bugs before they were handled:
 *
 *   1. **the array is closed by matching brackets**, not by searching for
 *      `"];"` — these arrays end with `] as const;` (TS) or `];` (Rust), and a
 *      naive search for `];` happily lands on `(typeof X)[number];` further down
 *      the file;
 *   2. **only whole-line literals are collected** (`  'princess:…',`), so prose
 *      and comments inside the array cannot contribute entries.  A comment
 *      containing an apostrophe (`the editor's language-service features`) used
 *      to pair up with the next literal's opening quote and silently *swallow*
 *      the command that followed it.
 */
function extractArray(source, constName, quote) {
  const decl = source.indexOf(`const ${constName}`);
  if (decl < 0) throw new Error(`array ${constName} not found`);
  // The array literal starts at the first `[` **after the `=`**, not at the
  // first `[` after the name: Rust declares `pub const X: &[&str] = &[`, whose
  // type annotation would otherwise be mistaken for the array itself.
  const eq = source.indexOf('=', decl);
  if (eq < 0) throw new Error(`array ${constName} has no initializer`);
  const open = source.indexOf('[', eq);
  if (open < 0) throw new Error(`array ${constName} has no opening bracket`);

  let depth = 0;
  let close = -1;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === '[') depth += 1;
    else if (source[i] === ']') {
      depth -= 1;
      if (depth === 0) {
        close = i;
        break;
      }
    }
  }
  if (close < 0) throw new Error(`array ${constName} is not terminated`);

  const body = source.slice(open + 1, close);
  const lineRe = new RegExp(`^\\s*${quote}([^${quote}]+)${quote},?\\s*(?://[^\\n]*)?$`, 'gm');
  const found = [];
  const seen = new Set();
  for (const m of body.matchAll(lineRe)) {
    if (!seen.has(m[1])) {
      seen.add(m[1]);
      found.push(m[1]);
    }
  }
  return found;
}

export const readTsContract = () => readFileSync(TS_CONTRACT, 'utf8');
export const readRustContract = () => readFileSync(RUST_CONTRACT, 'utf8');
export const readSpec = () => readFileSync(SPEC, 'utf8');

export const extractTsCommands = (src = readTsContract()) => extractArray(src, 'IPC_COMMANDS', "'");
export const extractTsErrorCodes = (src = readTsContract()) => extractArray(src, 'ERROR_CODES', "'");
export const extractRustCommands = (src = readRustContract()) =>
  extractArray(src, 'CONTRACT_COMMANDS', '"');
export const extractRustErrorCodes = (src = readRustContract()) =>
  extractArray(src, 'ERROR_CODES', '"');

/** Every `princess:<domain>:<action>` literal in the frontend source tree. */
export function scanSourceLiterals(dir = SRC_DIR, out = new Map()) {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      scanSourceLiterals(full, out);
      continue;
    }
    if (!/\.(ts|tsx|js|mjs)$/.test(entry)) continue;
    if (entry.endsWith('.d.ts')) continue;
    const text = readFileSync(full, 'utf8');
    for (const [i, line] of text.split('\n').entries()) {
      for (const m of line.matchAll(/['"`](princess:[a-z]+:[A-Za-z][A-Za-z0-9]*)['"`]/g)) {
        const where = `${path.relative(REPO_ROOT, full)}:${i + 1}`;
        if (!out.has(m[1])) out.set(m[1], []);
        out.get(m[1]).push(where);
      }
    }
  }
  return out;
}

const sameSet = (a, b) => a.length === b.length && a.every((x) => b.includes(x));
const diff = (a, b) => a.filter((x) => !b.includes(x));

/** Run every check.  Returns a structured report (no printing, no exiting). */
export function checkContract() {
  const spec = readSpec();
  const specCommands = extractSpecCommands(spec);
  const specCodes = extractSpecErrorCodes(spec);
  const tsCommands = extractTsCommands();
  const tsCodes = extractTsErrorCodes();
  const rustCommands = extractRustCommands();
  const rustCodes = extractRustErrorCodes();
  const literals = scanSourceLiterals();

  const badDomain = tsCommands.filter((c) => {
    const domain = c.split(':')[1];
    return !CONTRACT_DOMAINS.includes(domain);
  });

  const checks = [
    {
      name: 'spec §3 command list is non-empty and well-formed',
      ok: specCommands.length > 0 && specCommands.every((c) => /^princess:[a-z]+:[A-Za-z]/.test(c)),
      detail: `${specCommands.length} commands extracted from docs/spec/10-contracts.md §3`,
    },
    {
      name: 'extraction yields only well-formed names (no junk from comments)',
      ok:
        [...tsCommands, ...rustCommands].every((c) => /^princess:[a-z]+:[A-Za-z][A-Za-z0-9]*$/.test(c)) &&
        [...tsCodes, ...rustCodes].every((c) => /^E_[A-Z_]+$/.test(c)),
      detail: `ts=${tsCommands.length} rust=${rustCommands.length} codes ts=${tsCodes.length} rust=${rustCodes.length}`,
    },
    {
      name: 'TS IPC_COMMANDS == spec §3 commands',
      ok: sameSet(tsCommands, specCommands),
      detail:
        diff(specCommands, tsCommands).length || diff(tsCommands, specCommands).length
          ? `missing in TS: [${diff(specCommands, tsCommands)}] · extra in TS: [${diff(tsCommands, specCommands)}]`
          : `${tsCommands.length} commands, exact match`,
    },
    {
      name: 'Rust CONTRACT_COMMANDS == spec §3 commands',
      ok: sameSet(rustCommands, specCommands),
      detail:
        diff(specCommands, rustCommands).length || diff(rustCommands, specCommands).length
          ? `missing in Rust: [${diff(specCommands, rustCommands)}] · extra in Rust: [${diff(rustCommands, specCommands)}]`
          : `${rustCommands.length} commands, exact match`,
    },
    {
      name: 'TS ERROR_CODES == Rust ERROR_CODES == spec §3 error codes',
      ok: sameSet(tsCodes, specCodes) && sameSet(rustCodes, specCodes),
      detail:
        sameSet(tsCodes, specCodes) && sameSet(rustCodes, specCodes)
          ? `${specCodes.length} codes, exact match in all three`
          : `spec=${specCodes.length} ts=[${diff(tsCodes, specCodes)}]/[${diff(specCodes, tsCodes)}] rust=[${diff(rustCodes, specCodes)}]`,
    },
    {
      name: 'the pending-amendment allowlist is empty (nothing waved through)',
      ok: PENDING_CONTRACT_COMMANDS.every((c) => !tsCommands.includes(c)),
      detail: PENDING_CONTRACT_COMMANDS.length
        ? `pending: ${PENDING_CONTRACT_COMMANDS.join(', ')}`
        : 'no command is waiting for a contract amendment',
    },
    {
      name: 'every command uses a §3 domain',
      ok: badDomain.length === 0,
      detail: badDomain.length ? `bad domains: ${badDomain}` : `domains ⊂ {${CONTRACT_DOMAINS.join(', ')}}`,
    },
    {
      name: 'frontend source literals are all in the contract',
      ok: [...literals.keys()].every(
        (c) => tsCommands.includes(c) || PENDING_CONTRACT_COMMANDS.includes(c),
      ),
      detail: [...literals.keys()]
        .map((c) => {
          const state = tsCommands.includes(c)
            ? 'in contract'
            : PENDING_CONTRACT_COMMANDS.includes(c)
              ? 'PENDING amendment'
              : '*** NOT IN CONTRACT ***';
          return `${c} [${state}] @ ${literals.get(c).join(', ')}`;
        })
        .join('\n    '),
    },
  ];

  return {
    ok: checks.every((c) => c.ok),
    checks,
    specCommands,
    tsCommands,
    rustCommands,
    specCodes,
    tsCodes,
    rustCodes,
    literals,
  };
}

export function formatReport(report) {
  const lines = ['PrincessIDE P3-5 — IPC contract alignment (docs/spec/10-contracts.md §3)', ''];
  for (const check of report.checks) {
    lines.push(`  [${check.ok ? 'PASS' : 'FAIL'}] ${check.name}`);
    lines.push(`         ${check.detail}`);
  }
  lines.push('');
  lines.push(`  result: ${report.ok ? 'ALIGNED' : 'MISALIGNED'}`);
  return lines.join('\n');
}

// CLI entry point (only when executed directly, not when imported by tests).
if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  const report = checkContract();
  console.log(formatReport(report));
  process.exit(report.ok ? 0 : 1);
}
