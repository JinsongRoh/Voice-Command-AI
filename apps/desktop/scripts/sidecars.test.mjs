// node --test scripts/sidecars.test.mjs (pnpm test)
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { SIDECARS, sidecarPlan } from './sidecars.mjs';

const here = dirname(fileURLToPath(import.meta.url));

test('builds vcad and the vca CLI in one cargo run', () => {
  const plan = sidecarPlan({
    triple: 'x86_64-pc-windows-msvc',
    debug: false,
    explicitTarget: undefined,
    targetDir: join('repo', 'target'),
    binaries: 'bin',
  });
  assert.deepEqual(plan.cargoArgs, [
    'build',
    '-p',
    'vca-server',
    '-p',
    'vca-cli',
    '--bin',
    'vcad',
    '--bin',
    'vca',
    '--release',
  ]);
  assert.deepEqual(plan.copies, [
    {
      from: join('repo', 'target', 'release', 'vcad.exe'),
      to: join('bin', 'vcad-x86_64-pc-windows-msvc.exe'),
    },
    {
      from: join('repo', 'target', 'release', 'vca.exe'),
      to: join('bin', 'vca-x86_64-pc-windows-msvc.exe'),
    },
  ]);
  assert.equal(plan.chmod, false);
});

test('cross builds and debug builds', () => {
  const plan = sidecarPlan({
    triple: 'aarch64-apple-darwin',
    debug: true,
    explicitTarget: 'aarch64-apple-darwin',
    targetDir: 't',
    binaries: 'b',
  });
  assert.ok(!plan.cargoArgs.includes('--release'));
  assert.deepEqual(plan.cargoArgs.slice(-2), ['--target', 'aarch64-apple-darwin']);
  assert.deepEqual(
    plan.copies.map((c) => [c.from, c.to]),
    [
      [join('t', 'aarch64-apple-darwin', 'debug', 'vcad'), join('b', 'vcad-aarch64-apple-darwin')],
      [join('t', 'aarch64-apple-darwin', 'debug', 'vca'), join('b', 'vca-aarch64-apple-darwin')],
    ],
  );
  assert.equal(plan.chmod, true);
});

test('every sidecar is in externalBin', () => {
  const conf = JSON.parse(
    readFileSync(resolve(here, '..', 'src-tauri', 'tauri.vcad.conf.json'), 'utf8'),
  );
  assert.deepEqual(
    conf.bundle.externalBin,
    SIDECARS.map((s) => `binaries/${s.bin}`),
  );
});
