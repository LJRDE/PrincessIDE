import { defineConfig } from 'vite';

// Tauri drives this dev server: it must listen on a fixed port and must not
// clear the screen (the Rust side owns the terminal).
// See https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: '127.0.0.1',
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2022',
    // Tauri *embeds* `dist/` into the release binary (tauri.conf.json
    // `frontendDist`), so a source map here is not a debug convenience — it is
    // ~2 MB of shipped weight inside the executable.  Opt back in with
    // PRINCESSIDE_SOURCEMAP=1 when a build actually needs to be debugged.
    sourcemap: process.env['PRINCESSIDE_SOURCEMAP'] === '1',
  },
});
