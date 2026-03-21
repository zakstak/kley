import { spawn } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';

const repoRoot = process.cwd();
const port = process.env.PLAYWRIGHT_WEB_PORT || '3211';
const homeDir = resolve(repoRoot, '.playwright', 'home');
const systemHome = process.env.HOME || homeDir;
const cargoHome = process.env.CARGO_HOME || resolve(systemHome, '.cargo');
const rustupHome = process.env.RUSTUP_HOME || resolve(systemHome, '.rustup');

rmSync(homeDir, { force: true, recursive: true });
mkdirSync(homeDir, { recursive: true });

const child = spawn('cargo', ['run', '--bin', 'kley', '--', 'web', '--bind', `127.0.0.1:${port}`], {
  cwd: repoRoot,
  stdio: 'inherit',
  env: {
    ...process.env,
    HOME: homeDir,
    CARGO_HOME: cargoHome,
    RUSTUP_HOME: rustupHome,
  },
});

const forwardSignal = (signal) => {
  if (!child.killed) {
    child.kill(signal);
  }
};

process.on('SIGINT', () => forwardSignal('SIGINT'));
process.on('SIGTERM', () => forwardSignal('SIGTERM'));

child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
