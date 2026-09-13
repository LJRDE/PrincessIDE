/**
 * Editor component: CodeMirror 6 (chosen in P3, see docs/reports/p3-shell.md §编辑器选型).
 *
 * Language-service split (decision D17):
 *   - the *engine* (Rust, crates/princess-build) owns kernel-specific clangd
 *     configuration (`.clangd`, `compile_commands.json`) — not this file;
 *   - the *UI* owns editor interaction and uses the mature LSP client library
 *     `@codemirror/lsp-client` (`src/lsp/client.ts`) over a transport that an
 *     engine-side bridge supplies.  No LSP protocol stack is reimplemented here.
 *
 * This module deliberately has no dependency on the DOM at import time beyond
 * CodeMirror itself, so `languageForFile` / `editorExtensions` stay unit-testable
 * in a plain Node environment.
 */

import { EditorState, type Extension } from '@codemirror/state';
import {
  EditorView,
  drawSelection,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  rectangularSelection,
} from '@codemirror/view';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import {
  bracketMatching,
  defaultHighlightStyle,
  foldGutter,
  indentOnInput,
  syntaxHighlighting,
} from '@codemirror/language';
import { cpp } from '@codemirror/lang-cpp';
import { StreamLanguage } from '@codemirror/language';
import { gas } from '@codemirror/legacy-modes/mode/gas';

/** LSP language ids we hand to clangd (clangd supports c, cpp, objective-c, ...). */
export type EditorLanguageId = 'c' | 'cpp' | 'asm' | 'plaintext';

const C_EXT = ['.c', '.h'];
const CPP_EXT = ['.cc', '.cpp', '.cxx', '.hpp', '.hh', '.hxx', '.cppm', '.ixx'];
const ASM_EXT = ['.s', '.S'.toLowerCase(), '.asm', '.nasm'];

function hasExt(file: string, exts: readonly string[]): boolean {
  const lower = file.toLowerCase();
  return exts.some((e) => lower.endsWith(e));
}

/**
 * x86_64 kernel projects are C/asm hybrids.  clangd cannot serve assembly
 * (research report §3: "不要拿它当汇编的 IDE 后端"), so `.S`/`.asm` get a
 * syntax-only GAS stream mode and are **not** sent to the language server.
 */
/**
 * Native "pick a file" seam (same D17 injection pattern as `DirSelectorFn`).
 * In the Tauri webview this is `tauri-plugin-dialog`; in a plain browser
 * preview it is absent and the editor falls back to its path text input.
 */
export type FileSelectorFn = () => Promise<string | null>;

export function languageForFile(filename: string): {
  languageId: EditorLanguageId;
  extension: Extension;
  lspEligible: boolean;
} {
  if (hasExt(filename, ASM_EXT)) {
    return { languageId: 'asm', extension: StreamLanguage.define(gas), lspEligible: false };
  }
  if (hasExt(filename, C_EXT)) {
    return { languageId: 'c', extension: cpp(), lspEligible: true };
  }
  if (hasExt(filename, CPP_EXT)) {
    return { languageId: 'cpp', extension: cpp(), lspEligible: true };
  }
  return { languageId: 'plaintext', extension: [], lspEligible: false };
}

/** Baseline extension set (no LSP). */
export function editorExtensions(filename: string, readonly = false): Extension[] {
  const { extension } = languageForFile(filename);
  return [
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightActiveLine(),
    foldGutter(),
    history(),
    drawSelection(),
    rectangularSelection(),
    indentOnInput(),
    bracketMatching(),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
    extension,
    EditorView.lineWrapping,
    EditorState.readOnly.of(readonly),
  ];
}

export interface EditorHandle {
  readonly view: EditorView;
  readonly filename: string;
  getDoc(): string;
  setDoc(text: string): void;
  /** Attach the LSP plugin extension produced by the LSP client (D17 seam). */
  attachLsp(plugin: Extension): void;
  detachLsp(): void;
  destroy(): void;
}

/**
 * Mount an editor.  `lspPlugin` is optional: with no language server the editor
 * is still fully usable (syntax highlighting, editing), which is what keeps the
 * shell functional before the engine-side bridge exists.
 */
export function createEditor(
  parent: HTMLElement,
  options: { doc?: string; filename?: string; readonly?: boolean; lspPlugin?: Extension } = {},
): EditorHandle {
  const filename = options.filename ?? 'untitled.c';
  let lspCompartmentPlugin: Extension | null = options.lspPlugin ?? null;

  const base = editorExtensions(filename, options.readonly ?? false);
  const state = EditorState.create({
    doc: options.doc ?? '',
    extensions: lspCompartmentPlugin ? [...base, lspCompartmentPlugin] : base,
  });

  const view = new EditorView({ state, parent });

  return {
    view,
    filename,
    getDoc: () => view.state.doc.toString(),
    setDoc(text: string) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
    },
    attachLsp(plugin: Extension) {
      lspCompartmentPlugin = plugin;
      view.dispatch({ effects: [] });
      view.setState(
        EditorState.create({
          doc: view.state.doc,
          extensions: [...base, plugin],
        }),
      );
    },
    detachLsp() {
      lspCompartmentPlugin = null;
      view.setState(EditorState.create({ doc: view.state.doc, extensions: base }));
    },
    destroy() {
      view.destroy();
    },
  };
}
