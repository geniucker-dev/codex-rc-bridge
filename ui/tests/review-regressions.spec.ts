import { test, expect, type Page, type Route } from "@playwright/test";
import { createMockApi } from "./mock-api";
const base = "http://127.0.0.1:5175";
type MockApi = ReturnType<typeof createMockApi>;
async function fixture(
  page: Page,
  options: {
    fakeEvents?: boolean;
    intercept?: (route: Route, path: string, mock: MockApi) => Promise<boolean>;
  } = {},
) {
  const { fakeEvents = false } = options;
  const mock = createMockApi();
  await mock.handle("/api/session", "POST", { password: "synthetic-password" });
  if (fakeEvents)
    await page.addInitScript(() => {
      const w = window as any;
      class TestEventSource extends EventTarget {
        readyState = 1;
        constructor() {
          super();
          w.reviewEventSource = this;
        }
        close() {
          this.readyState = 2;
        }
      }
      w.EventSource = TestEventSource;
    });
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/events") return route.fulfill({ status: 503, body: "" });
    if (path === "/api/deployment/network")
      return route.fulfill({
        json: {
          configured: true,
          mode: "direct",
          address: null,
          authenticated: false,
        },
      });
    if (options.intercept && (await options.intercept(route, path, mock)))
      return;
    const response = await mock.handle(
      path,
      route.request().method(),
      route.request().postDataJSON() ?? {},
    );
    return route.fulfill({
      ...response,
      body: response.status === 204 ? "" : response.body,
    });
  });
  return mock;
}
test("translated notice and normal navigation", async ({ page }) => {
  await fixture(page);
  await page.goto(base + "/#/accounts");
  await expect(
    page.getByRole("heading", { name: "账号", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  await expect(page.locator("main.content > p.notice")).toContainText(
    "刷新状态完成。",
  );
  await page
    .getByRole("combobox", { name: "语言", exact: true })
    .selectOption("en");
  await expect(
    page.getByRole("heading", { name: "Accounts", exact: true }),
  ).toBeVisible();
  await expect(page.locator("main.content > p.notice")).toHaveCount(0);
  await page
    .getByRole("button", { name: "Remote control", exact: true })
    .click();
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "Accounts", exact: true }),
  ).toBeVisible();
  await page.goForward();
  await expect(
    page.getByRole("heading", { name: "Remote control", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Remote control", exact: true }),
  ).toBeVisible();
});
test("interrupted input registration can be retried in both languages", async ({
  page,
}) => {
  const mock = await fixture(page);
  const input = mock.data.configuration.value.endpoints[0];
  input.lifecycle = "draft";
  input.registered = false;
  input.registration_uncertain = false;
  mock.data.chains = [];
  // Old input-page links land on the overview.
  await page.goto(base + "/#/in");
  // The card label is translated too, so match it by the controller's name.
  const card = page.getByRole("article", {
    name: new RegExp(`${input.endpoint.display_name}$`),
  });
  await expect(card.getByText("注册未完成", { exact: true })).toBeVisible();
  await expect(
    card.getByRole("button", { name: "重试注册", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("combobox", { name: "语言", exact: true })
    .selectOption("en");
  await expect(
    card.getByText("Registration unfinished", { exact: true }),
  ).toBeVisible();
  await card
    .getByRole("button", { name: "Retry registration", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toContainText("DEMO1234");
  // Registered now; what is left is pairing its one device.
  await expect(card.locator(".attention")).toContainText("No device paired");
  expect(
    mock.calls.filter(
      (call) => call.path === `/api/inputs/${input.endpoint.device_id}/pair`,
    ),
  ).toHaveLength(1);
  expect(
    mock.calls.filter((call) => /\/(start|stop)$/.test(call.path)),
  ).toHaveLength(0);
});
test("late endpoint response after session replacement", async ({ page }) => {
  let release!: () => void;
  const responseReady = new Promise<void>((resolve) => {
    release = resolve;
  });
  let started = false;
  let delivered = false;
  await fixture(page, {
    fakeEvents: true,
    intercept: async (
      route: Route,
      path: string,
      mock: ReturnType<typeof createMockApi>,
    ) => {
      if (path !== "/api/endpoints" || route.request().method() !== "POST")
        return false;
      const response = await mock.handle(
        path,
        "POST",
        route.request().postDataJSON(),
      );
      started = true;
      await responseReady;
      await route.fulfill(response);
      delivered = true;
      return true;
    },
  });
  await page.goto(base + "/#/in");
  await page.getByRole("button", { name: "新建控制端", exact: true }).click();
  await page.locator('select[name="account"]').selectOption({ index: 1 });
  await page
    .getByLabel("名称", { exact: true })
    .fill("synthetic-delayed-input");
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "添加并显示配对码", exact: true })
    .click();
  await expect.poll(() => started).toBe(true);
  await page.evaluate(() =>
    (window as any).reviewEventSource.dispatchEvent(
      new Event("session_expired"),
    ),
  );
  await expect(
    page.getByRole("heading", { name: "登录管理台", exact: true }),
  ).toBeVisible();
  await page.getByLabel("用户名", { exact: true }).fill("demo");
  await page.getByLabel("密码", { exact: true }).fill("synthetic-password");
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "远程控制", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "账号", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "账号", exact: true }),
  ).toBeVisible();
  release();
  await expect.poll(() => delivered).toBe(true);
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  await expect(page).toHaveURL(/#\/accounts$/);
});
test("open destructive dialog after state read becomes stale", async ({
  page,
}) => {
  let failState = false;
  const mock = await fixture(page, {
    fakeEvents: true,
    intercept: async (route: Route, path: string) => {
      if (path !== "/api/state" || !failState) return false;
      await route.fulfill({
        status: 503,
        json: { error: "temporarily_unavailable" },
      });
      return true;
    },
  });
  await page.goto(base + "/#/controllers/in-one");
  await page.getByRole("button", { name: "断开连接", exact: true }).click();
  failState = true;
  await page.evaluate(() =>
    (window as any).reviewEventSource.dispatchEvent(new Event("changed")),
  );
  await expect(page.locator("main.content > .warning")).toBeVisible();
  const button = page
    .getByRole("dialog")
    .getByRole("button", { name: "停止并断开", exact: true });
  await expect(button).toBeDisabled();
  expect(mock.data.chains).toHaveLength(2);
  await page.getByRole("button", { name: "关闭对话框", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});
test("selected account survives insertion into a live snapshot", async ({
  page,
}) => {
  const mock = await fixture(page, { fakeEvents: true });
  mock.data.configuration.value.accounts = [
    {
      user_id: "selected-user",
      workspace_id: "workspace-b",
      email: "selected@example.test",
    },
  ];
  await page.goto(base + "/#/in");
  await page.getByRole("button", { name: "新建控制端", exact: true }).click();
  await page.locator('select[name="account"]').selectOption({ index: 1 });
  await page
    .getByLabel("名称", { exact: true })
    .fill("synthetic-chosen-account-input");
  mock.data.configuration.value.accounts.unshift({
    user_id: "new-user",
    workspace_id: "workspace-a",
    email: "new@example.test",
  });
  await page.evaluate(() =>
    (window as any).reviewEventSource.dispatchEvent(new Event("changed")),
  );
  await expect(
    page.getByRole("option", { name: "new@example.test", exact: true }),
  ).toHaveCount(1);
  await expect(
    page.locator('select[name="account"] option:checked'),
  ).toHaveText("selected@example.test");
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "添加并显示配对码", exact: true })
    .click();
  await expect
    .poll(
      () => mock.calls.filter((call) => call.path === "/api/endpoints").length,
    )
    .toBe(1);
  const sentAccount = mock.calls.find((call) => call.path === "/api/endpoints")
    .body.account;
  expect(sentAccount).toEqual({
    workspace_id: "workspace-b",
    user_id: "selected-user",
  });
});
test("desktop browser-open string errors show recovery instructions", async ({
  page,
}) => {
  const data = createMockApi().data;
  await page.exposeFunction(
    "reviewDesktopInvoke",
    async (command: string, args: any) => {
      if (command.startsWith("plugin:event|")) return 1;
      if (command === "desktop_set_language") return;
      if (command !== "desktop_request")
        throw new Error("Unexpected test command");
      switch (args.path) {
        case "/capabilities":
          return { management: true, desktop: true };
        case "/session":
          return {
            identity: {
              username: "Local",
              tenant_id: "synthetic-local",
              expires_at: 0,
              deployment_admin: true,
            },
            csrf: "",
          };
        case "/state":
          return data;
        case "/deployment/network":
          return {
            configured: true,
            mode: "direct",
            address: null,
            authenticated: false,
          };
        case "/accounts/login":
          return {
            result: "login",
            value: {
              id: "synthetic-login",
              url: "https://auth.openai.com/oauth/authorize?state=synthetic",
              expires_at: 2000000000,
              automatic_callback: true,
            },
          };
        case "/logins/synthetic-login":
          return { status: "pending" };
        default:
          throw new Error("Unexpected test route");
      }
    },
  );
  await page.addInitScript(() => {
    const w = window as any;
    w.isTauri = true;
    w.reviewOpenAttempts = 0;
    w.__TAURI_INTERNALS__ = {
      invoke: (command, args) => {
        if (command === "desktop_open_authorization") {
          w.reviewOpenAttempts++;
          return Promise.reject(
            "Cannot open the system browser. Copy the authorization link instead.",
          );
        }
        return w.reviewDesktopInvoke(command, args);
      },
      transformCallback: () => 1,
    };
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  });
  await fixture(page);
  await page.goto(base + "/#/accounts");
  await page.getByRole("button", { name: "添加账号", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.getByRole("link", { name: "打开授权页面", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => (window as any).reviewOpenAttempts))
    .toBe(1);
  await expect(
    page.getByRole("button", { name: "关闭对话框", exact: true }),
  ).toBeEnabled();
  await expect(page.getByRole("dialog").locator(".notice")).toContainText(
    "无法打开系统浏览器。请复制授权链接。",
  );
});

for (const kind of ["bind", "switch"] as const) {
  test(`${kind} requires explicit reselection when the chosen output becomes unavailable`, async ({
    page,
  }) => {
    const mock = await fixture(page, { fakeEvents: true });
    if (kind === "bind") mock.data.chains.shift();
    if (kind === "bind") {
      // A free controller connects from its card on the overview.
      await page.goto(base + "/");
      await page
        .getByRole("article", { name: "控制端 我的手机入口", exact: true })
        .getByRole("button", { name: "连接被控端", exact: true })
        .click();
    } else {
      await page.goto(base + "/#/controllers/in-one");
      await page
        .getByRole("button", { name: "切换被控端", exact: true })
        .click();
    }
    const dialog = page.getByRole("dialog");
    const select = dialog.getByRole("combobox");
    const submit = dialog.getByRole("button", {
      name: kind === "bind" ? "连接并启动" : "断开并切换",
      exact: true,
    });
    await select.selectOption("out-two");
    // Reordering a live snapshot must preserve the selected device.
    mock.data.configuration.value.endpoints.reverse();
    await page.evaluate(() =>
      (window as any).reviewEventSource.dispatchEvent(new Event("changed")),
    );
    await expect(select).toHaveValue("out-two");
    const output = mock.data.configuration.value.endpoints.find(
      (item) => item.endpoint.device_id === "out-two",
    )!;
    output.lifecycle = "deleting";
    await page.evaluate(() =>
      (window as any).reviewEventSource.dispatchEvent(new Event("changed")),
    );
    await expect(select.locator('option[value="out-two"]')).toHaveCount(0);
    await expect(select).toHaveValue("");
    await expect(submit).toBeDisabled();
    expect(
      mock.calls.filter((call) =>
        call.path.endsWith(kind === "bind" ? "/binding" : "/switch"),
      ),
    ).toHaveLength(0);
    // Reappearance cannot silently restore a decision invalidated by an earlier snapshot.
    output.lifecycle = "active";
    await page.evaluate(() =>
      (window as any).reviewEventSource.dispatchEvent(new Event("changed")),
    );
    await expect(select.locator('option[value="out-two"]')).toHaveCount(1);
    await expect(select).toHaveValue("");
    await expect(submit).toBeDisabled();
    await select.selectOption("out-two");
    await submit.click();
    await expect
      .poll(
        () =>
          mock.calls.filter((call) =>
            call.path.endsWith(kind === "bind" ? "/binding" : "/switch"),
          ).length,
      )
      .toBe(1);
    expect(
      mock.calls.find((call) =>
        call.path.endsWith(kind === "bind" ? "/binding" : "/switch"),
      )!.body.output,
    ).toBe("out-two");
  });
}

for (const initialStatus of [200, 401]) {
  test(`session initialization finishes before login becomes available (${initialStatus})`, async ({
    page,
  }) => {
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let started = false;
    let checkedCsrf = "";
    await fixture(page, {
      intercept: async (route, path, mock) => {
        if (path === "/api/session" && route.request().method() === "GET") {
          started = true;
          const previous = structuredClone(mock.session);
          await held;
          await route.fulfill({
            status: initialStatus,
            json:
              initialStatus === 200 ? previous : { error: "login_required" },
          });
          return true;
        }
        if (path === "/api/session" && route.request().method() === "POST") {
          mock.session.identity.username = "new-user";
          mock.session.csrf = "synthetic-new-session-csrf";
        }
        if (path === "/api/accounts/check")
          checkedCsrf = route.request().headers()["x-csrf-token"];
        return false;
      },
    });
    await page.goto(base + "/#/accounts");
    await expect.poll(() => started).toBe(true);
    await expect(
      page.getByRole("heading", { name: "正在连接服务", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "登录", exact: true }),
    ).toHaveCount(0);
    release();
    if (initialStatus === 200) {
      await expect(
        page.getByRole("heading", { name: "账号", exact: true }),
      ).toBeVisible();
      await page.getByRole("button", { name: "退出登录", exact: true }).click();
    }
    await expect(
      page.getByRole("heading", { name: "登录管理台", exact: true }),
    ).toBeVisible();
    await page.getByLabel("用户名", { exact: true }).fill("new-user");
    await page.getByLabel("密码", { exact: true }).fill("synthetic-password");
    await page.getByRole("button", { name: "登录", exact: true }).click();
    await expect(page.locator("header")).toContainText("new-user");
    await page.getByRole("button", { name: "账号", exact: true }).click();
    await page.getByRole("button", { name: "检查登录", exact: true }).click();
    await expect.poll(() => checkedCsrf).toBe("synthetic-new-session-csrf");
    await expect(page.locator("header")).toContainText("new-user");
  });
}
