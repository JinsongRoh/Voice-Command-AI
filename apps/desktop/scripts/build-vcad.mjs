#!/usr/bin/env node
// Builds the desktop app's sidecars, the VCA server (vcad) and the `vca`
// CLI (which agent hooks run), and copies them to
// src-tauri/binaries/<name>-<target-triple>[.exe], where Tauri's
// `externalBin` (tauri.vcad.conf.json) picks them up. See sidecars.mjs.
//
//   node scripts/build-vcad.mjs                  # release build for this machine
//   node scripts/build-vcad.mjs --debug          # faster debug build (tauri dev)
//   node scripts/build-vcad.mjs --target aarch64-apple-darwin
//   node scripts/build-vcad.mjs --dry-run        # print the plan, build nothing
//
// The binaries folder is ignored by git; never commit its contents.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, chmodSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { sidecarPlan } from './sidecars.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..', '..', '..');
const binaries = resolve(here, '..', 'src-tauri', 'binaries');

const args = process.argv.slice(2);
const debug = args.includes('--debug');
const dryRun = args.includes('--dry-run');
const targetIndex = args.indexOf('--target');
const explicitTarget = targetIndex >= 0 ? args[targetIndex + 1] : undefined;
if (targetIndex >= 0 && !explicitTarget) {
  console.error('build-vcad: --target needs a value');
  process.exit(2);
}

function hostTriple() {
  const out = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
  const line = out.split(/\r?\n/).find((l) => l.startsWith('host: '));
  if (!line) throw new Error('cannot read the host target triple from `rustc -vV`');
  return line.slice('host: '.length).trim();
}

const plan = sidecarPlan({
  triple: explicitTarget ?? hostTriple(),
  debug,
  explicitTarget,
  targetDir: process.env.CARGO_TARGET_DIR
    ? resolve(process.env.CARGO_TARGET_DIR)
    : join(repo, 'target'),
  binaries,
});

console.log(`build-vcad: cargo ${plan.cargoArgs.join(' ')}`);
if (!dryRun) execFileSync('cargo', plan.cargoArgs, { cwd: repo, stdio: 'inherit' });

if (!dryRun) mkdirSync(binaries, { recursive: true });
for (const { from, to } of plan.copies) {
  if (!dryRun) {
    copyFileSync(from, to);
    if (plan.chmod) chmodSync(to, 0o755);
  }
  console.log(`build-vcad: ${from} -> ${to}${dryRun ? ' (dry run)' : ''}`);
}
