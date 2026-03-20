import type { Page } from '@playwright/test';
import { expect, test } from '@playwright/test';

const unsupportedControls = [
  '#btn-fork-picker',
  '#btn-task-start',
  '#btn-task-complete',
  '#btn-model-picker',
  '#prompt-image-input',
  '#mock-preset-buttons',
  '#btn-session-picker',
];

async function waitForWorkspaceReady(page: Page) {
  await expect(page.getByTestId('app-shell')).toBeVisible();
  await expect(page.getByTestId('session-list').locator('button').first()).toBeVisible();
  await expect
    .poll(async () => (await page.getByTestId('status-pill').textContent()) || '')
    .toMatch(/connected|idle|streaming|running/i);
}

async function submitPrompt(page: Page, prompt: string) {
  await page.locator('#prompt-input').fill(prompt);
  await page.getByTestId('composer-submit').click();
}

function captureConsoleErrors(page: Page): string[] {
  const consoleErrors: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') {
      consoleErrors.push(message.text());
    }
  });
  page.on('pageerror', (error) => {
    consoleErrors.push(String(error));
  });
  return consoleErrors;
}

async function expectUnsupportedControlsAbsent(page: Page) {
  for (const selector of unsupportedControls) {
    await expect(page.locator(selector)).toHaveCount(0);
  }
}

test('core workspace parity', async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);

  await page.goto('/');
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId('composer-submit')).toBeEnabled();

  await expect(page).toHaveTitle(/Kley web/i);
  await expect(page.getByTestId('transcript')).toBeVisible();
  await expect(page.getByTestId('composer')).toBeVisible();
  await expect(page.getByTestId('inspector-panel')).toBeVisible();
  await expect(page.getByTestId('session-list').locator('button')).toHaveCount(1);

  await expectUnsupportedControlsAbsent(page);

  await submitPrompt(page, 'please use a tool');
  await expect(page.getByTestId('transcript')).toContainText('please use a tool');
  await expect(page.getByTestId('transcript')).toContainText('Test assistant reply: please use a tool');

  const toolCard = page.getByTestId('tool-card').filter({ hasText: 'unknown_tool' }).first();
  await expect(toolCard).toBeVisible();
  await toolCard.locator('summary').click();
  await expect(toolCard).toHaveAttribute('open', '');
  await expect(toolCard).toContainText('Call ID');

  await submitPrompt(page, 'hold-open abortable response please stop');
  await expect(page.getByTestId('abort-button')).toBeEnabled();
  await page.getByTestId('abort-button').click();
  await expect(page.getByTestId('status-pill')).toContainText('aborted');

  await page.reload();
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId('composer-submit')).toBeEnabled();
  await expect(page.getByTestId('transcript')).toContainText('please use a tool');
  await expect(page.getByTestId('transcript')).toContainText('Test assistant reply: please use a tool');
  await expect(page.getByTestId('tool-card').filter({ hasText: 'unknown_tool' })).toHaveCount(0);
  await expect(page.getByTestId('tool-card').first()).toContainText('No tool events yet.');
  expect(consoleErrors).toEqual([]);
});

test('unsupported controls absent', async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);

  await page.goto('/');
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId('composer-submit')).toBeEnabled();
  await expectUnsupportedControlsAbsent(page);

  expect(consoleErrors).toEqual([]);
});

test('reconnect recovery', async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);
  const prompt = 'hold-open: keep streaming';
  const finalReply = `Test assistant reply: ${prompt}`;

  await page.goto('/');
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId('composer-submit')).toBeEnabled();

  await submitPrompt(page, prompt);
  await expect(page.getByTestId('abort-button')).toBeEnabled();
  await expect(page.getByTestId('status-pill')).toContainText(/running|streaming/i);

  const activeMessage = page.locator('[data-message-id]').last().locator('[data-field="content"]');
  await expect
    .poll(async () => (await activeMessage.textContent()) || '')
    .not.toEqual('');

  let partialReply = '';
  await expect
    .poll(async () => {
      const text = ((await activeMessage.textContent()) || '').trim();
      if (text && text !== finalReply) {
        partialReply = text;
      }
      return partialReply;
    })
    .not.toEqual('');

  await page.reload();
  await waitForWorkspaceReady(page);

  const recoveredMessage = page.locator('[data-message-id]').last().locator('[data-field="content"]');
  await expect(page.getByTestId('transcript')).toContainText(prompt);
  await expect
    .poll(async () => ((await recoveredMessage.textContent()) || '').trim())
    .toContain(partialReply);
  await expect(page.getByTestId('transcript')).toContainText(finalReply, { timeout: 20_000 });
  await expect(page.getByTestId('composer-submit')).toBeEnabled();
  await expect(page.getByTestId('abort-button')).toBeDisabled();
  await expect
    .poll(async () => (await page.getByTestId('status-pill').textContent()) || '')
    .not.toMatch(/streaming|running/i);
  expect(consoleErrors).toEqual([]);
});
