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
    sourcemap: true,
  },
});
