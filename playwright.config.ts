import { defineConfig } from '@playwright/test';

const port = Number(process.env.PLAYWRIGHT_WEB_PORT || '3211');
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: './playwright',
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  timeout: 45_000,
  expect: {
    timeout: 10_000,
  },
  use: {
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  webServer: {
    command: 'node ./playwright/support/web-server.mjs',
    url: `${baseURL}/healthz`,
    reuseExistingServer: false,
    stdout: 'pipe',
    stderr: 'pipe',
    timeout: 240_000,
    env: {
      ...process.env,
      PLAYWRIGHT_WEB_PORT: String(port),
    },
  },
});
