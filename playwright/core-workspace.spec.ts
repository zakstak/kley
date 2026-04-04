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

const CONTROL_BLOCK_START = "<kley-test-provider>";
const CONTROL_BLOCK_END = "</kley-test-provider>";

type ProbeMatch = {
  type?: string;
  requestId?: string;
  taskId?: string;
  minSequenceExclusive?: number;
};

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

async function runPython(script: string, args: string[]) {
  const childProcess = (await import("node:" + "child_process")) as {
    execFileSync: (
      command: string,
      args: string[],
      options: { stdio: "pipe" },
    ) => void;
  };
  childProcess.execFileSync("python3", ["-c", script, ...args], {
    stdio: "pipe",
  });
}

async function seedDelegationParentTask(
  taskId: string,
  ownerSessionId: string,
) {
  const policySnapshot = JSON.stringify({
    allow_autonomous_spawn: true,
    current_depth: 0,
    max_depth: 3,
    max_concurrency: 2,
    budget: 20,
    allowed_providers: ["test"],
    allowed_models: ["test-model"],
    approved_tools: ["delegate_task", "report_status", "read_file"],
    tool_approval_mode: "ask",
    parent_close_policy: "request_cancel_descendants",
  });

  await runPython(
    [
      "import datetime, sqlite3, sys",
      "db_path, task_id, policy, owner_session_id = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]",
      "now = datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00', 'Z')",
      "conn = sqlite3.connect(db_path)",
      "conn.execute(\"INSERT INTO tasks (task_id, parent_task_id, title, priority, policy_snapshot, parent_close_policy, recovery_checkpoint, owner_session_id, created_at, updated_at) VALUES (?, NULL, ?, ?, ?, ?, NULL, ?, ?, ?)\", (task_id, 'playwright-parent-task', 10, policy, 'request_cancel_descendants', owner_session_id, now, now))",
      "conn.commit()",
      "conn.close()",
    ].join("\n"),
    [".playwright/home/.kley/kley.db", taskId, policySnapshot, ownerSessionId],
  );
}

async function appendTaskEvent(
  taskId: string,
  attemptId: string,
  eventType: string,
  payload: string,
) {
  await runPython(
    [
      "import datetime, sqlite3, sys",
      "db_path, task_id, attempt_id, event_type, payload = sys.argv[1:6]",
      "now = datetime.datetime.now(datetime.timezone.utc).isoformat().replace('+00:00', 'Z')",
      "conn = sqlite3.connect(db_path)",
      'conn.execute("INSERT INTO task_events (task_id, attempt_id, session_id, event_type, payload, recorded_at) VALUES (?, ?, NULL, ?, ?, ?)", (task_id, attempt_id, event_type, payload, now))',
      "conn.commit()",
      "conn.close()",
    ].join("\n"),
    [".playwright/home/.kley/kley.db", taskId, attemptId, eventType, payload],
  );
}

async function installSocketProbe(page: Page) {
  await page.addInitScript(() => {
    const NativeWebSocket = window.WebSocket;
    const state = {
      sockets: [] as WebSocket[],
      frames: [] as Array<Record<string, unknown>>,
    };

    const recordFrame = (raw: string) => {
      try {
        const parsed = JSON.parse(raw) as Record<string, unknown>;
        state.frames.push(parsed);
      } catch {
        state.frames.push({ type: "__raw__", raw });
      }
    };

    window.WebSocket = class extends NativeWebSocket {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        state.sockets.push(this);
        this.addEventListener("message", (event: MessageEvent) => {
          if (typeof event.data === "string") {
            recordFrame(event.data);
          }
        });
      }
    } as unknown as typeof WebSocket;

    (window as unknown as { __kleySocketProbe: unknown }).__kleySocketProbe = {
      clear: () => {
        state.frames.length = 0;
      },
      getFrames: () => state.frames,
      send: (command: unknown) => {
        const openSocket = [...state.sockets]
          .reverse()
          .find((socket) => socket.readyState === 1);
        const target = openSocket || state.sockets[state.sockets.length - 1];
        if (!target) {
          throw new Error("no socket available");
        }
        target.send(JSON.stringify(command));
      },
      closeLatest: (code: number, reason: string) => {
        const latest = state.sockets[state.sockets.length - 1];
        if (latest) {
          latest.close(code, reason);
        }
      },
      hasOpenSocket: () =>
        state.sockets.some((socket) => socket.readyState === 1),
    };
  });
}

async function waitForOpenSocket(page: Page) {
  await expect
    .poll(async () => {
      return page.evaluate(
        () =>
          (
            window as unknown as {
              __kleySocketProbe?: { hasOpenSocket?: () => boolean };
            }
          ).__kleySocketProbe?.hasOpenSocket?.() ?? false,
      );
    })
    .toBeTruthy();
}

async function sendProbeCommand(page: Page, command: Record<string, unknown>) {
  await page.evaluate((cmd) => {
    (
      window as unknown as {
        __kleySocketProbe: { send: (command: Record<string, unknown>) => void };
      }
    ).__kleySocketProbe.send(cmd);
  }, command);
}

async function waitForProbeFrame(
  page: Page,
  matcher: ProbeMatch,
  timeout = 15_000,
) {
  await expect
    .poll(
      async () => {
        return page.evaluate((match) => {
          const probe = (
            window as unknown as {
              __kleySocketProbe?: {
                getFrames?: () => Array<Record<string, unknown>>;
              };
            }
          ).__kleySocketProbe;
          const frames = probe?.getFrames?.() || [];
          return frames.some((frame) => {
            if (match.type && frame.type !== match.type) {
              return false;
            }
            if (match.requestId && frame.request_id !== match.requestId) {
              return false;
            }
            const frameTaskId =
              typeof frame.task_id === "string"
                ? frame.task_id
                : typeof frame.data === "object" && frame.data
                  ? (frame.data as Record<string, unknown>).task_id
                  : undefined;
            if (match.taskId && frameTaskId !== match.taskId) {
              return false;
            }
            if (typeof match.minSequenceExclusive === "number") {
              if (typeof frame.sequence !== "number") {
                return false;
              }
              if (frame.sequence <= match.minSequenceExclusive) {
                return false;
              }
            }
            return true;
          });
        }, matcher);
      },
      { timeout },
    )
    .toBeTruthy();

  return page.evaluate((match) => {
    const probe = (
      window as unknown as {
        __kleySocketProbe?: {
          getFrames?: () => Array<Record<string, unknown>>;
        };
      }
    ).__kleySocketProbe;
    const frames = probe?.getFrames?.() || [];
    const matches = frames.filter((frame) => {
      if (match.type && frame.type !== match.type) {
        return false;
      }
      if (match.requestId && frame.request_id !== match.requestId) {
        return false;
      }
      const frameTaskId =
        typeof frame.task_id === "string"
          ? frame.task_id
          : typeof frame.data === "object" && frame.data
            ? (frame.data as Record<string, unknown>).task_id
            : undefined;
      if (match.taskId && frameTaskId !== match.taskId) {
        return false;
      }
      if (typeof match.minSequenceExclusive === "number") {
        if (typeof frame.sequence !== "number") {
          return false;
        }
        if (frame.sequence <= match.minSequenceExclusive) {
          return false;
        }
      }
      return true;
    });
    return matches.length ? matches[matches.length - 1] : null;
  }, matcher);
}

async function waitForProbeResponse(
  page: Page,
  requestId: string,
  timeout = 15_000,
) {
  await expect
    .poll(
      async () => {
        return page.evaluate((id) => {
          const probe = (
            window as unknown as {
              __kleySocketProbe?: {
                getFrames?: () => Array<Record<string, unknown>>;
              };
            }
          ).__kleySocketProbe;
          const frames = probe?.getFrames?.() || [];
          return frames.some(
            (frame) =>
              frame.request_id === id &&
              (frame.type === "response.ok" || frame.type === "response.error"),
          );
        }, requestId);
      },
      { timeout },
    )
    .toBeTruthy();

  return page.evaluate((id) => {
    const probe = (
      window as unknown as {
        __kleySocketProbe?: {
          getFrames?: () => Array<Record<string, unknown>>;
        };
      }
    ).__kleySocketProbe;
    const frames = probe?.getFrames?.() || [];
    const matches = frames.filter(
      (frame) =>
        frame.request_id === id &&
        (frame.type === "response.ok" || frame.type === "response.error"),
    );
    return matches.length ? matches[matches.length - 1] : null;
  }, requestId);
}

function toolCallPrompt(
  name: string,
  argumentsObject: Record<string, unknown>,
) {
  return `${CONTROL_BLOCK_START}${JSON.stringify({ type: "tool_call", name, arguments: argumentsObject })}${CONTROL_BLOCK_END}`;
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
  await expect(page.getByTestId("transcript")).not.toContainText(
    "please use a tool",
  );
  await expect(page.getByTestId("transcript")).not.toContainText(
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
    const latest = sockets[sockets.length - 1];
    if (latest) {
      latest.close(4000, "playwright-transport-loss");
    }
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

test("task delegation lifecycle", async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);
  const runId = `${Date.now()}-${Math.floor(Math.random() * 100_000)}`;
  const parentTaskId = `pw-parent-${runId}`;
  const childTaskId = `pw-child-${runId}`;
  const watchRequestId = `req-task-watch-${runId}`;

  await installSocketProbe(page);
  await page.goto("/");
  await waitForWorkspaceReady(page);
  await waitForOpenSocket(page);

  const snapshot = (await waitForProbeFrame(page, {
    type: "state.snapshot",
  })) as {
    session_id: string;
  };
  const sessionId = snapshot.session_id;

  await seedDelegationParentTask(parentTaskId, sessionId);

  await submitPrompt(
    page,
    toolCallPrompt("delegate_task", {
      parent_task_id: parentTaskId,
      child_task_id: childTaskId,
      title: "playwright delegated child",
      priority: 7,
      handoff_brief: "Investigate logs and return summary",
      artifact_ids: ["artifact://logs/playwright"],
      requested_policy_json: JSON.stringify({
        budget: 5,
        approved_tools: ["report_status"],
        tool_approval_mode: "never",
      }),
      after_sequence: 0,
    }),
  );

  await expect(page.getByTestId("transcript")).toContainText(childTaskId, {
    timeout: 20_000,
  });

  await sendProbeCommand(page, {
    type: "task.watch",
    request_id: watchRequestId,
    session_id: sessionId,
    task_id: childTaskId,
    after_sequence: 0,
  });

  const watchResponse = (await waitForProbeResponse(page, watchRequestId)) as {
    type: string;
    error?: { message?: string };
    data?: { cursor?: { latest_sequence?: number } };
  };
  expect(watchResponse.type).toBe("response.ok");
  const watchAck = watchResponse as {
    data: { cursor: { latest_sequence: number } };
  };
  expect(watchAck.data.cursor.latest_sequence).toBeGreaterThan(0);

  const listSnapshot = await waitForProbeFrame(page, {
    type: "task.list.snapshot",
    requestId: watchRequestId,
  });
  expect((listSnapshot as { task_id: string }).task_id).toBe(childTaskId);

  const detailSnapshot = (await waitForProbeFrame(page, {
    type: "task.detail.snapshot",
    requestId: watchRequestId,
  })) as {
    task_id: string;
    task: {
      task_id: string;
      parent_task_id: string;
      latest_attempt_id: string | null;
    };
  };
  expect(detailSnapshot.task_id).toBe(childTaskId);
  expect(detailSnapshot.task.task_id).toBe(childTaskId);
  expect(detailSnapshot.task.parent_task_id).toBe(parentTaskId);
  expect(detailSnapshot.task.latest_attempt_id).toBeTruthy();

  await waitForProbeFrame(page, {
    type: "task.event",
    requestId: watchRequestId,
    taskId: childTaskId,
  });

  expect(consoleErrors).toEqual([]);
});

test("task watch survives reconnect", async ({ page }) => {
  const consoleErrors = captureConsoleErrors(page);
  const runId = `${Date.now()}-${Math.floor(Math.random() * 100_000)}`;
  const parentTaskId = `pw-reconnect-parent-${runId}`;
  const childTaskId = `pw-reconnect-child-${runId}`;
  const watch1RequestId = `req-task-watch-1-${runId}`;
  const watch2RequestId = `req-task-watch-2-${runId}`;

  await installSocketProbe(page);
  await page.goto("/");
  await waitForWorkspaceReady(page);
  await waitForOpenSocket(page);

  const snapshot = (await waitForProbeFrame(page, {
    type: "state.snapshot",
  })) as {
    session_id: string;
  };
  const sessionId = snapshot.session_id;

  await seedDelegationParentTask(parentTaskId, sessionId);

  await submitPrompt(
    page,
    toolCallPrompt("delegate_task", {
      parent_task_id: parentTaskId,
      child_task_id: childTaskId,
      title: "playwright reconnect child",
      priority: 8,
      handoff_brief: "watch reconnect replay",
      artifact_ids: ["artifact://logs/reconnect"],
      requested_policy_json: JSON.stringify({
        budget: 4,
        approved_tools: ["report_status"],
        tool_approval_mode: "never",
      }),
      after_sequence: 0,
    }),
  );
  await expect(page.getByTestId("transcript")).toContainText(childTaskId, {
    timeout: 20_000,
  });

  await sendProbeCommand(page, {
    type: "task.watch",
    request_id: watch1RequestId,
    session_id: sessionId,
    task_id: childTaskId,
    after_sequence: 0,
  });

  const watch1Response = (await waitForProbeResponse(
    page,
    watch1RequestId,
  )) as {
    type: string;
    data?: { cursor?: { latest_sequence?: number } };
  };
  expect(watch1Response.type).toBe("response.ok");
  const ack1 = watch1Response as {
    data: { cursor: { latest_sequence: number } };
  };
  const initialCursor = ack1.data.cursor.latest_sequence;
  expect(initialCursor).toBeGreaterThan(0);

  const detail1 = (await waitForProbeFrame(page, {
    type: "task.detail.snapshot",
    requestId: watch1RequestId,
  })) as {
    attempts: Array<{ attempt_id: string }>;
  };
  const attemptId = detail1.attempts[detail1.attempts.length - 1]?.attempt_id;
  expect(attemptId).toBeTruthy();
  if (!attemptId) {
    throw new Error("expected attempt id from task.detail.snapshot");
  }

  await page.evaluate(() => {
    (
      window as unknown as {
        __kleySocketProbe: {
          closeLatest: (code: number, reason: string) => void;
        };
      }
    ).__kleySocketProbe.closeLatest(4001, "playwright-task-watch-reconnect");
  });

  await expect(page.getByTestId("status-pill")).toContainText(
    /reconnecting|connected/i,
  );
  await waitForOpenSocket(page);
  const reconnectSnapshot = (await waitForProbeFrame(
    page,
    { type: "state.snapshot" },
    20_000,
  )) as {
    session_id: string;
  };
  const reconnectSessionId = reconnectSnapshot.session_id;

  await appendTaskEvent(
    childTaskId,
    attemptId,
    "attempt.state.transition",
    JSON.stringify({ from: "running", to: "completed" }),
  );
  await appendTaskEvent(
    childTaskId,
    attemptId,
    "task.state.transition",
    JSON.stringify({ from: "running", to: "completed" }),
  );

  await sendProbeCommand(page, {
    type: "task.watch",
    request_id: watch2RequestId,
    session_id: reconnectSessionId,
    task_id: childTaskId,
    after_sequence: initialCursor,
  });

  const watch2Response = (await waitForProbeResponse(
    page,
    watch2RequestId,
  )) as {
    type: string;
    data?: { cursor?: { after_sequence?: number; latest_sequence?: number } };
  };
  expect(watch2Response.type).toBe("response.ok");
  const ack2 = watch2Response as {
    data: { cursor: { after_sequence: number; latest_sequence: number } };
  };
  expect(ack2.data.cursor.after_sequence).toBe(initialCursor);
  expect(ack2.data.cursor.latest_sequence).toBeGreaterThan(initialCursor);

  const replayedAfterReconnect = (await waitForProbeFrame(page, {
    type: "task.event",
    requestId: watch2RequestId,
    taskId: childTaskId,
    minSequenceExclusive: initialCursor,
  })) as { sequence: number; task_id: string };
  expect(replayedAfterReconnect.task_id).toBe(childTaskId);
  expect(replayedAfterReconnect.sequence).toBeGreaterThan(initialCursor);

  expect(consoleErrors).toEqual([]);
});
