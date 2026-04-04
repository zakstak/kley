import { spawn } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
import http from 'node:http';
import { resolve } from 'node:path';

const repoRoot = process.cwd();
const port = process.env.PLAYWRIGHT_WEB_PORT || '3211';
const homeDir = resolve(repoRoot, '.playwright', 'home');
const xdgConfigHome = resolve(homeDir, '.config');
const xdgDataHome = resolve(homeDir, '.local', 'share');
const systemHome = process.env.HOME || homeDir;
const cargoHome = process.env.CARGO_HOME || resolve(systemHome, '.cargo');
const rustupHome = process.env.RUSTUP_HOME || resolve(systemHome, '.rustup');

function healthCheck(timeoutMs = 500) {
  return new Promise((resolveHealth) => {
    const request = http.request(
      {
        host: '127.0.0.1',
        port,
        path: '/healthz',
        method: 'GET',
        timeout: timeoutMs,
      },
      (response) => {
        response.resume();
        resolveHealth(response.statusCode === 200);
      },
    );

    request.on('timeout', () => {
      request.destroy();
      resolveHealth(false);
    });
    request.on('error', () => resolveHealth(false));
    request.end();
  });
}

async function waitForHealthy(maxAttempts = 8, delayMs = 250) {
  for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
    if (await healthCheck()) {
      return true;
    }
    if (attempt < maxAttempts - 1) {
      await new Promise((resolveDelay) => setTimeout(resolveDelay, delayMs));
    }
  }
  return false;
}

function runReuseMode() {
    const timer = setInterval(() => {}, 1_000);

  const stop = () => {
    clearInterval(timer);
    process.exit(0);
  };

  process.on('SIGINT', stop);
    process.on('SIGTERM', stop);
}

function keepProcessAlive() {
  const timer = setInterval(() => {}, 1_000);
  return () => clearInterval(timer);
}

function processExists(pid) {
  if (typeof pid !== 'number') {
    return false;
  }

  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code !== 'ESRCH';
  }
}

async function waitForProcessExit(pid, maxAttempts = 50, delayMs = 100) {
  for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
    if (!processExists(pid)) {
      return true;
    }
    if (attempt < maxAttempts - 1) {
      await new Promise((resolveDelay) => setTimeout(resolveDelay, delayMs));
    }
  }
  return !processExists(pid);
}

function killChildProcessTree(child, signal) {
  if (!child || child.killed) {
    return;
  }

  if (typeof child.pid === 'number') {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch {
      // Fall through to direct child kill.
    }
  }

  try {
    child.kill(signal);
  } catch {
    // Ignore failures when process is already dead.
  }
}

async function startManagedServer() {
  if (await waitForHealthy()) {
    console.log(`playwright-web-server: reusing healthy server on 127.0.0.1:${port}`);
    runReuseMode();
    return;
  }

  rmSync(homeDir, { force: true, recursive: true });
  mkdirSync(xdgConfigHome, { recursive: true });
  mkdirSync(xdgDataHome, { recursive: true });

  const child = spawn(
    'cargo',
    ['run', '--bin', 'kley', '--', 'web', '--bind', `127.0.0.1:${port}`],
    {
      cwd: repoRoot,
      stdio: 'ignore',
      env: {
        ...process.env,
        HOME: homeDir,
        XDG_CONFIG_HOME: xdgConfigHome,
        XDG_DATA_HOME: xdgDataHome,
        CARGO_HOME: cargoHome,
        RUSTUP_HOME: rustupHome,
      },
    },
  );

  let shuttingDown = false;
  const stopKeepingAlive = keepProcessAlive();

  const forwardSignal = async (signal) => {
    if (shuttingDown) {
      return;
    }
    shuttingDown = true;
    stopKeepingAlive();
    killChildProcessTree(child, signal);

    if (!(await waitForProcessExit(child.pid))) {
      killChildProcessTree(child, 'SIGKILL');
      await waitForProcessExit(child.pid, 10, 100);
    }

    process.exit(0);
  };

  process.on('SIGINT', () => {
    void forwardSignal('SIGINT');
  });
  process.on('SIGTERM', () => {
    void forwardSignal('SIGTERM');
  });

  if (!(await waitForHealthy(40, 250))) {
    stopKeepingAlive();
    shuttingDown = true;
    killChildProcessTree(child, 'SIGTERM');
    await waitForProcessExit(child.pid, 20, 100);
    throw new Error(`managed web server on 127.0.0.1:${port} failed health check`);
  }

  child.on('exit', async (code, signal) => {
    stopKeepingAlive();
    if (shuttingDown) {
      process.exit(0);
      return;
    }

    if (signal) {
      process.kill(process.pid, signal);
      return;
    }

    if (!shuttingDown && (await waitForHealthy(4, 200))) {
      console.log(
        `playwright-web-server: existing healthy server detected on 127.0.0.1:${port}; continuing`,
      );
      runReuseMode();
      return;
    }

    process.exit(code ?? 1);
  });
}

startManagedServer().catch((error) => {
  console.error('playwright-web-server: failed to start', error);
  process.exit(1);
});
