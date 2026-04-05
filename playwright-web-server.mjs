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
const OPENAI_CONTROL_PREFIX = 'mock-openai-control:';

function extractLatestUserPrompt(items) {
  if (!Array.isArray(items)) {
    return '';
  }

  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item?.type !== 'message' || item?.role !== 'user') {
      continue;
    }
    const { content } = item;
    if (typeof content === 'string') {
      return content;
    }
    if (Array.isArray(content)) {
      return content
        .map((part) => {
          if (typeof part === 'string') {
            return part;
          }
          if (typeof part?.text === 'string') {
            return part.text;
          }
          return '';
        })
        .join('');
    }
  }

  return '';
}

function parseControlledPrompt(prompt) {
  if (typeof prompt !== 'string' || !prompt.startsWith(OPENAI_CONTROL_PREFIX)) {
    return null;
  }

  try {
    return JSON.parse(prompt.slice(OPENAI_CONTROL_PREFIX.length));
  } catch {
    return null;
  }
}

function textSse(text) {
  return [
    `event: response.output_text.delta\ndata: ${JSON.stringify({ type: 'response.output_text.delta', delta: text })}\n`,
    `event: response.completed\ndata: ${JSON.stringify({ type: 'response.completed', usage: { input_tokens: 13, output_tokens: 5, total_tokens: 18 } })}\n`,
    '',
  ].join('\n');
}

function toolCallSse(name, argumentsObject) {
  const argumentsText = JSON.stringify(argumentsObject ?? {});
  return [
    `event: response.output_item.added\ndata: ${JSON.stringify({ type: 'response.output_item.added', item: { type: 'function_call', call_id: 'call-1', name } })}\n`,
    `event: response.function_call_arguments.delta\ndata: ${JSON.stringify({ type: 'response.function_call_arguments.delta', delta: argumentsText })}\n`,
    `event: response.function_call_arguments.done\ndata: ${JSON.stringify({ type: 'response.function_call_arguments.done', call_id: 'call-1', name })}\n`,
    `event: response.completed\ndata: ${JSON.stringify({ type: 'response.completed', usage: { input_tokens: 11, output_tokens: 7, total_tokens: 18 } })}\n`,
    '',
  ].join('\n');
}

async function writeSlowTextResponse(response, text, delayMs = 150) {
  const chunks = ['Mock ', 'assistant ', `reply: ${text}`];
  response.writeHead(200, { 'Content-Type': 'text/event-stream' });

  for (const chunk of chunks) {
    response.write(
      `event: response.output_text.delta\ndata: ${JSON.stringify({ type: 'response.output_text.delta', delta: chunk })}\n\n`,
    );
    await new Promise((resolveDelay) => setTimeout(resolveDelay, delayMs));
  }

  response.end(
    `event: response.completed\ndata: ${JSON.stringify({ type: 'response.completed', usage: { input_tokens: 13, output_tokens: 5, total_tokens: 18 } })}\n\n`,
  );
}

async function startMockOpenAiServer() {
  const server = http.createServer(async (request, response) => {
    if (request.method !== 'POST' || request.url !== '/responses') {
      response.writeHead(404);
      response.end();
      return;
    }

    const chunks = [];
    for await (const chunk of request) {
      chunks.push(chunk);
    }

    const payload = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
    const inputItems = Array.isArray(payload?.input) ? payload.input : [];
    const latestUser = extractLatestUserPrompt(inputItems);
    const latestType = inputItems.at(-1)?.type || '';
    const promptLower = latestUser.toLowerCase();
    const controlled = parseControlledPrompt(latestUser);

    if (latestType === 'function_call_output') {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      response.end(textSse(`Mock assistant reply: ${latestUser}`));
      return;
    }

    if (controlled?.type === 'tool_call') {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      response.end(toolCallSse(controlled.name, controlled.arguments));
      return;
    }

    if (controlled?.type === 'text') {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      response.end(textSse(controlled.content));
      return;
    }

    if (promptLower.includes('hold-open') || promptLower.includes('abortable')) {
      await writeSlowTextResponse(response, latestUser, promptLower.includes('hold-open') ? 150 : 50);
      return;
    }

    if (promptLower.includes('tool')) {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      response.end(toolCallSse('unknown_tool', {}));
      return;
    }

    response.writeHead(200, { 'Content-Type': 'text/event-stream' });
    response.end(textSse(`Mock assistant reply: ${latestUser}`));
  });

  await new Promise((resolveListen, rejectListen) => {
    server.once('error', rejectListen);
    server.listen(0, '127.0.0.1', resolveListen);
  });

  const address = server.address();
  if (!address || typeof address === 'string') {
    server.close();
    throw new Error('mock OpenAI server failed to bind an address');
  }

  return {
    server,
    baseUrl: `http://127.0.0.1:${address.port}`,
  };
}

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
  let mockOpenAiServer = null;
  if (await waitForHealthy()) {
    console.log(`playwright-web-server: reusing healthy server on 127.0.0.1:${port}`);
    runReuseMode();
    return;
  }

  rmSync(homeDir, { force: true, recursive: true });
  mkdirSync(xdgConfigHome, { recursive: true });
  mkdirSync(xdgDataHome, { recursive: true });

  const childEnv = {
    ...process.env,
    HOME: homeDir,
    XDG_CONFIG_HOME: xdgConfigHome,
    XDG_DATA_HOME: xdgDataHome,
    CARGO_HOME: cargoHome,
    RUSTUP_HOME: rustupHome,
  };

  if (!childEnv.OPENAI_API_KEY) {
    mockOpenAiServer = await startMockOpenAiServer();
    childEnv.OPENAI_API_KEY = 'test-key';
    childEnv.OPENAI_BASE_URL = mockOpenAiServer.baseUrl;
  }

  const child = spawn(
    'cargo',
    ['run', '--bin', 'kley', '--', 'web', '--bind', `127.0.0.1:${port}`],
    {
      cwd: repoRoot,
      stdio: 'ignore',
      env: childEnv,
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

    if (mockOpenAiServer) {
      await new Promise((resolveClose) => mockOpenAiServer.server.close(resolveClose));
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
    if (mockOpenAiServer) {
      await new Promise((resolveClose) => mockOpenAiServer.server.close(resolveClose));
    }
    throw new Error(`managed web server on 127.0.0.1:${port} failed health check`);
  }

  child.on('exit', async (code, signal) => {
    stopKeepingAlive();
    if (mockOpenAiServer) {
      await new Promise((resolveClose) => mockOpenAiServer.server.close(resolveClose));
    }
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
