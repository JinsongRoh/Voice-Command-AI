import { describe, expect, it } from 'vitest';

import { assertReleaseDiagOff } from './buildGuard';

describe('release build guard', () => {
  const build = { command: 'build', mode: 'production' };

  it('refuses a production build with verbose diagnostics', () => {
    expect(() => assertReleaseDiagOff({ ...build, diag: '1' })).toThrow(/VITE_VCA_DIAG/);
    expect(() => assertReleaseDiagOff({ ...build, diag: '0' })).toThrow(/VITE_VCA_DIAG/);
  });

  it('allows a production build without the flag', () => {
    expect(() => assertReleaseDiagOff({ ...build })).not.toThrow();
    expect(() => assertReleaseDiagOff({ ...build, diag: '' })).not.toThrow();
  });

  it('allows a build explicitly marked as internal', () => {
    expect(() => assertReleaseDiagOff({ ...build, diag: '1', internal: '1' })).not.toThrow();
    expect(() => assertReleaseDiagOff({ ...build, diag: '1', internal: '0' })).toThrow();
  });

  it('never stops the dev server, tests or other modes', () => {
    expect(() =>
      assertReleaseDiagOff({ command: 'serve', mode: 'production', diag: '1' }),
    ).not.toThrow();
    expect(() => assertReleaseDiagOff({ command: 'build', mode: 'test', diag: '1' })).not.toThrow();
    expect(() =>
      assertReleaseDiagOff({ command: 'build', mode: 'development', diag: '1' }),
    ).not.toThrow();
  });
});
