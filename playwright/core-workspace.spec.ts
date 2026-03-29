import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";

const unsupportedControls = [
  "#btn-fork-picker",
  "#btn-task-start",
  "#btn-task-complete",
  "#btn-model-picker",
  "#prompt-image-input",
  "#mock-preset-buttons",
  "#btn-session-picker",
];

async function waitForWorkspaceReady(page: Page) {
  await expect(page.getByTestId("app-shell")).toBeVisible();
  await expect(
    page.getByTestId("session-list").locator("button").first(),
  ).toBeVisible();
  await expect
    .poll(
      async () => (await page.getByTestId("status-pill").textContent()) || "",
    )
    .toMatch(/connected|idle|streaming|running/i);
}

async function submitPrompt(page: Page, prompt: string) {
  await page.locator("#prompt-input").fill(prompt);
  await page.getByTestId("composer-submit").click();
}

function captureConsoleErrors(page: Page): string[] {
  const consoleErrors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") {
      consoleErrors.push(message.text());
    }
  });
  page.on("pageerror", (error) => {
    consoleErrors.push(String(error));
  });
  return consoleErrors;
}

async function expectUnsupportedControlsAbsent(page: Page) {
  for (const selector of unsupportedControls) {
    await expect(page.locator(selector)).toHaveCount(0);
  }
}

test("core workspace parity", async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);

  await page.goto("/");
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId("composer-submit")).toBeEnabled();

  await expect(page).toHaveTitle(/Kley web/i);
  await expect(page.getByTestId("transcript")).toBeVisible();
  await expect(page.getByTestId("composer")).toBeVisible();
  await expect(page.getByTestId("inspector-panel")).toBeVisible();
  await expect(page.getByTestId("session-list").locator("button")).toHaveCount(
    1,
  );
  await expect(page.getByTestId("selected-session-meta")).toContainText(
    "status: active",
  );
  await expect(page.getByTestId("selected-session-meta")).toContainText(
    "provider/model: test/test-model",
  );
  await expect(
    page.getByTestId("session-list").locator("button").first(),
  ).toContainText("updated");
  await expect(page.getByTestId("filter-chip-all")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(
    page.locator('.filter-chip[aria-disabled="true"]', { hasText: "UI" }),
  ).toBeVisible();

  await expectUnsupportedControlsAbsent(page);

  await submitPrompt(page, "please use a tool");
  await expect(page.getByTestId("transcript")).toContainText(
    "please use a tool",
  );
  await expect(page.getByTestId("transcript")).toContainText(
    "Test assistant reply: please use a tool",
  );

  const toolCard = page
    .getByTestId("tool-card")
    .filter({ hasText: "unknown_tool" })
    .first();
  await expect(toolCard).toBeVisible();
  await toolCard.locator("summary").click();
  await expect(toolCard).toHaveAttribute("open", "");
  await expect(toolCard).toContainText("Call ID");

  const messageRows = page.locator(
    '#transcript-rows article[data-feed-category="messages"]',
  );
  const toolRows = page.locator(
    '#transcript-rows article[data-feed-category="tools"]',
  );
  await expect.poll(async () => await toolRows.count()).toBeGreaterThan(0);

  await page.getByTestId("filter-chip-tools").click();
  await expect(page.getByTestId("filter-chip-tools")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(toolRows.first()).toBeVisible();
  await expect(messageRows.first()).toBeHidden();

  await page.getByTestId("filter-chip-messages").click();
  await expect(page.getByTestId("filter-chip-messages")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(messageRows.first()).toBeVisible();

  await submitPrompt(page, "hold-open abortable response please stop");
  await expect(page.getByTestId("abort-button")).toBeEnabled();
  await page.getByTestId("abort-button").click();
  await expect(page.getByTestId("status-pill")).toContainText("aborted");

  await page.reload();
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId("composer-submit")).toBeEnabled();
  await expect(page.getByTestId("transcript")).toContainText(
    "please use a tool",
  );
  await expect(page.getByTestId("transcript")).toContainText(
    "Test assistant reply: please use a tool",
  );
  await expect(
    page.getByTestId("tool-card").filter({ hasText: "unknown_tool" }),
  ).toHaveCount(0);
  await expect(page.getByTestId("tool-card").first()).toContainText(
    "No tool events yet.",
  );
  expect(consoleErrors).toEqual([]);
});

test("tool card renders edit observation from mock websocket", async ({
  page,
}) => {
  const consoleErrors = captureConsoleErrors(page);

  await page.addInitScript(() => {
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = class extends NativeWebSocket {
      constructor(url: string | URL, protocols?: string | string[]) {
        const raw = typeof url === "string" ? url : url.toString();
        const resolved = new URL(raw, window.location.href);
        if (resolved.pathname === "/ws") {
          resolved.pathname = "/ws/mock";
        }
        super(resolved.toString(), protocols);
      }
    } as unknown as typeof WebSocket;
  });

  await page.goto("/");
  await waitForWorkspaceReady(page);

  await submitPrompt(page, "please use a tool with edit observation");
  const toolCard = page
    .getByTestId("tool-card")
    .filter({ hasText: "read" })
    .first();
  await expect(toolCard).toBeVisible();
  await toolCard.locator("summary").click();
  await expect(toolCard).toContainText("Edit observation");
  await expect(toolCard).toContainText("Engine: mock-engine");
  await expect(toolCard).toContainText("Path: templates/index.html");
  await expect(toolCard).toContainText("Applied 2/3");
  await expect(toolCard).toContainText(
    "Artifact: /tmp/mock-edit-artifact.json",
  );

  expect(consoleErrors).toEqual([]);
});

test("unsupported controls absent", async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);

  await page.goto("/");
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId("composer-submit")).toBeEnabled();
  await expectUnsupportedControlsAbsent(page);

  expect(consoleErrors).toEqual([]);
});

test("reconnect recovery", async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);
  const prompt = "hold-open: keep streaming";
  const finalReply = `Test assistant reply: ${prompt}`;

  await page.goto("/");
  await waitForWorkspaceReady(page);
  await expect(page.getByTestId("composer-submit")).toBeEnabled();

  await submitPrompt(page, prompt);
  await expect(page.getByTestId("abort-button")).toBeEnabled();
  await expect(page.getByTestId("status-pill")).toContainText(
    /running|streaming/i,
  );

  const activeMessage = page
    .locator("[data-message-id]")
    .last()
    .locator('[data-field="content"]');
  await expect
    .poll(async () => (await activeMessage.textContent()) || "")
    .not.toEqual("");

  let partialReply = "";
  await expect
    .poll(async () => {
      const text = ((await activeMessage.textContent()) || "").trim();
      if (text && text !== finalReply) {
        partialReply = text;
      }
      return partialReply;
    })
    .not.toEqual("");

  await page.reload();
  await waitForWorkspaceReady(page);

  const recoveredMessage = page
    .locator("[data-message-id]")
    .last()
    .locator('[data-field="content"]');
  await expect(page.getByTestId("transcript")).toContainText(prompt);
  await expect
    .poll(async () => ((await recoveredMessage.textContent()) || "").trim())
    .toContain(partialReply);
  await expect(page.getByTestId("transcript")).toContainText(finalReply, {
    timeout: 20_000,
  });
  await expect(page.getByTestId("composer-submit")).toBeEnabled();
  await expect(page.getByTestId("abort-button")).toBeDisabled();
  await expect
    .poll(
      async () => (await page.getByTestId("status-pill").textContent()) || "",
    )
    .not.toMatch(/streaming|running/i);
  expect(consoleErrors).toEqual([]);
});

test("transport loss clears stranded pending settings state", async ({
  page,
}) => {
  const consoleErrors = captureConsoleErrors(page);

  await page.addInitScript(() => {
    const NativeWebSocket = window.WebSocket;
    (window as unknown as { __kleySockets: WebSocket[] }).__kleySockets = [];
    (
      window as unknown as { __blockSettingsCommand: boolean }
    ).__blockSettingsCommand = false;

    window.WebSocket = class extends NativeWebSocket {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        (
          window as unknown as { __kleySockets: WebSocket[] }
        ).__kleySockets.push(this);
      }

      override send(data: string | ArrayBufferLike | Blob | ArrayBufferView) {
        const block = (window as unknown as { __blockSettingsCommand: boolean })
          .__blockSettingsCommand;
        if (
          block &&
          typeof data === "string" &&
          data.includes('"type":"session.settings.update"')
        ) {
          return;
        }
        super.send(data);
      }
    } as unknown as typeof WebSocket;
  });

  await page.goto("/");
  await waitForWorkspaceReady(page);

  await page.evaluate(() => {
    (
      window as unknown as { __blockSettingsCommand: boolean }
    ).__blockSettingsCommand = true;
  });

  await page.getByTestId("session-settings-submit").click();
  await expect(page.getByTestId("session-settings-submit")).toBeDisabled();

  await page.evaluate(() => {
    const sockets = (window as unknown as { __kleySockets: WebSocket[] })
      .__kleySockets;
    sockets.at(-1)?.close(4000, "playwright-transport-loss");
  });

  await expect(page.getByTestId("status-pill")).toContainText(
    /reconnecting|connected/i,
  );
  await expect(page.getByTestId("session-settings-submit")).toBeEnabled({
    timeout: 20_000,
  });
  await expect(page.getByTestId("composer-submit")).toBeEnabled({
    timeout: 20_000,
  });

  expect(consoleErrors).toEqual([]);
});
