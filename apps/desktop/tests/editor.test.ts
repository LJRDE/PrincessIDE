/**
 * Editor tests — CodeMirror 6 language wiring.
 *
 * x86_64 kernel projects mix C and assembly; the research report (§3) is blunt
 * that clangd cannot serve assembly, so `.S`/`.asm` must get a syntax-only GAS
 * mode and must NOT be advertised to the language server.  That routing is pure
 * logic and is asserted here; the LSP client itself is exercised in lsp.test.ts.
 */

import { describe, expect, it } from 'vitest';
import { EditorState } from '@codemirror/state';
import { editorExtensions, languageForFile } from '../src/components/editor.js';

describe('editor language routing', () => {
  it('routes the reference kernel fixture sources to C + clangd', () => {
    for (const file of ['kernel.c', 'serial.h', 'idt.c', 'fixtures/refkernel/kernel.c']) {
      const lang = languageForFile(file);
      expect(lang.languageId, file).toBe('c');
      expect(lang.lspEligible, file).toBe(true);
    }
  });

  it('routes C++ sources to cpp + clangd', () => {
    for (const file of ['klass.cpp', 'thing.cc', 'header.hpp']) {
      const lang = languageForFile(file);
      expect(lang.languageId, file).toBe('cpp');
      expect(lang.lspEligible, file).toBe(true);
    }
  });

  it('routes assembly to the GAS stream mode and keeps it away from the LSP', () => {
    for (const file of ['boot.S', 'isr.S', 'probe32.S', 'legacy.s', 'kernel.asm']) {
      const lang = languageForFile(file);
      expect(lang.languageId, file).toBe('asm');
      expect(lang.lspEligible, file).toBe(false);
    }
  });

  it('falls back to plaintext for unknown extensions', () => {
    const lang = languageForFile('linker.ld');
    expect(lang.languageId).toBe('plaintext');
    expect(lang.lspEligible).toBe(false);
    expect(lang.extension).toEqual([]);
  });

  it('builds a non-empty extension set for a real source file', () => {
    expect(editorExtensions('kernel.c').length).toBeGreaterThan(5);
  });

  it('honours the read-only flag through the real state facet', () => {
    const editable = EditorState.create({ extensions: editorExtensions('kernel.c') });
    const readonly = EditorState.create({ extensions: editorExtensions('kernel.c', true) });
    expect(editable.readOnly).toBe(false);
    expect(readonly.readOnly).toBe(true);
  });

  it('is case-insensitive about extensions', () => {
    expect(languageForFile('BOOT.S').languageId).toBe('asm');
    expect(languageForFile('Kernel.C').languageId).toBe('c');
  });
});
