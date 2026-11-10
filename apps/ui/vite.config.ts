/// <reference types="vitest/config" />
import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';

import { assertReleaseDiagOff } from './src/diag/buildGuard.ts';

/** When this bundle was built (local time, YYYYMMDD-HHmm): tells two builds of one version apart. */
function buildStamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
}

// Relative base so the bundle can be embedded by Tauri (custom protocol) as-is.
export default defineConfig(({ command, mode }) => {
  assertReleaseDiagOff({
    command,
    mode,
    diag: loadEnv(mode, process.cwd(), 'VITE_VCA_DIAG').VITE_VCA_DIAG,
    internal: process.env.VCA_INTERNAL_BUILD,
  });
  return {
    base: './',
    define: { __BUILD_STAMP__: JSON.stringify(process.env.VCA_BUILD_STAMP || buildStamp()) },
    plugins: [react()],
    server: { port: 5173, strictPort: false },
    build: { target: 'es2022', sourcemap: true },
    test: {
      environment: 'jsdom',
      globals: true,
      setupFiles: ['./src/test/setup.ts'],
      css: { modules: { classNameStrategy: 'non-scoped' } },
    },
  };
});
