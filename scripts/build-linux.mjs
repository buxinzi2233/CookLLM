#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { constants } from 'node:os';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'linux') {
  console.error('build:linux requires Linux; use npm run tauri build on other platforms.');
  process.exitCode = 1;
} else {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const cli = fileURLToPath(new URL('../node_modules/@tauri-apps/cli/tauri.js', import.meta.url));
  const child = spawn(process.execPath, [cli, 'build', ...process.argv.slice(2)], {
    cwd: root,
    shell: false,
    stdio: 'inherit',
    // linuxdeploy's older strip may reject modern system libraries (.relr.dyn).
    // This does not change Cargo's release strip=true setting.
    env: { ...process.env, NO_STRIP: process.env.NO_STRIP ?? '1' },
  });
  const forwardInterrupt = () => child.kill('SIGINT');
  const forwardTerminate = () => child.kill('SIGTERM');
  process.on('SIGINT', forwardInterrupt);
  process.on('SIGTERM', forwardTerminate);
  child.on('error', (error) => {
    console.error(`Failed to start the local Tauri CLI: ${error.message}`);
    process.exitCode = 1;
  });
  child.on('close', (code, signal) => {
    process.removeListener('SIGINT', forwardInterrupt);
    process.removeListener('SIGTERM', forwardTerminate);
    process.exitCode = code ?? (signal ? 128 + (constants.signals[signal] ?? 1) : 1);
  });
}
