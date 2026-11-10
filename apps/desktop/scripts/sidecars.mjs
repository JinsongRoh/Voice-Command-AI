// The executables bundled with the desktop app as Tauri sidecars
// (`externalBin` in src-tauri/tauri.vcad.conf.json), and how
// build-vcad.mjs builds and copies them. Pure functions, so they can be
// tested without running cargo (sidecars.test.mjs).
import { join } from 'node:path';

/**
 * Every sidecar: the VCA server and the `vca` CLI. Agent hooks run the
 * bundled CLI (`<app dir>/vca[.exe]`, on macOS `Contents/MacOS/vca`), so
 * its installed path stays stable across updates.
 */
export const SIDECARS = [
  { pkg: 'vca-server', bin: 'vcad' },
  { pkg: 'vca-cli', bin: 'vca' },
];

/**
 * The cargo arguments and copies for one build.
 *
 * @param {object} opts
 * @param {string} opts.triple Target triple of the binaries.
 * @param {boolean} opts.debug Debug build (tauri dev).
 * @param {string | undefined} opts.explicitTarget `--target` given on the command line.
 * @param {string} opts.targetDir Cargo's target directory.
 * @param {string} opts.binaries src-tauri/binaries.
 */
export function sidecarPlan({ triple, debug, explicitTarget, targetDir, binaries }) {
  const exe = triple.includes('windows') ? '.exe' : '';
  const cargoArgs = ['build'];
  for (const { pkg } of SIDECARS) cargoArgs.push('-p', pkg);
  for (const { bin } of SIDECARS) cargoArgs.push('--bin', bin);
  if (!debug) cargoArgs.push('--release');
  if (explicitTarget) cargoArgs.push('--target', explicitTarget);
  const outDir = join(targetDir, ...(explicitTarget ? [explicitTarget] : []), debug ? 'debug' : 'release');
  const copies = SIDECARS.map(({ bin }) => ({
    from: join(outDir, `${bin}${exe}`),
    to: join(binaries, `${bin}-${triple}${exe}`),
  }));
  return { cargoArgs, copies, chmod: !exe };
}
