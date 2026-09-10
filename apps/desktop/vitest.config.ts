import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    // Default to Node; the DOM tests opt in per file with a
    // `// @vitest-environment jsdom` docblock (vitest 3 deprecates
    // `environmentMatchGlobs` in favour of projects, and a per-file docblock
    // keeps the split visible at the top of the file it applies to).
    environment: 'node',
    include: ['tests/**/*.test.ts'],
    setupFiles: ['tests/setup.ts'],
  },
});
