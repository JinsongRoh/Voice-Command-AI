/**
 * Build-time guard of the verbose diagnostics. `VITE_VCA_DIAG` turns off the
 * release rules of `log.ts` (every event, raw error text, the iOS audio
 * route), so a release bundle must never be made with it.
 *
 * An internal test build opts in on purpose with `VCA_INTERNAL_BUILD=1`.
 * Lives in `src` and takes plain values so the tests can reach it; used by
 * `vite.config.ts`.
 */
export function assertReleaseDiagOff(build: {
  /** Vite's `command`: `build` or `serve`. */
  command: string;
  mode: string;
  /** The value of `VITE_VCA_DIAG`. */
  diag?: string;
  /** The value of `VCA_INTERNAL_BUILD`. */
  internal?: string;
}): void {
  if (build.command !== 'build' || build.mode !== 'production') return;
  if (!build.diag || build.internal === '1') return;
  throw new Error(
    'VITE_VCA_DIAG is set for a production build. Unset it, or set VCA_INTERNAL_BUILD=1 if this is an internal test build that must never be released.',
  );
}
