import { test, expect, type Page } from "@playwright/test";
import { createMockApi } from "./mock-api";
import { eventServer } from "./event-server";
let events: ReturnType<typeof eventServer>;
test.afterEach(async () => {
  await events?.close();
});
async function setup(page: Page) {
  const mock = createMockApi();
  events = eventServer(mock);
  await page.route("**/api/**", async (route) => {
    const req = route.request();
    if (new URL(req.url()).pathname === "/api/events") {
      await route.continue({ url: events.url });
      return;
    }
    const response = await mock.handle(
      new URL(req.url()).pathname,
      req.method(),
      req.postDataJSON() ?? {},
    );
    await route.fulfill({
      ...response,
      body: response.status === 204 ? "" : response.body,
    });
  });
  await page.goto("/");
  await page.getByLabel("用户名", { exact: true }).fill("demo");
  await page.getByLabel("密码", { exact: true }).fill("synthetic-password");
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "远程控制", exact: true }),
  ).toBeVisible();
  await expect(connectionRow(page, "我的手机入口")).toBeVisible();
  return mock;
}
// A connected pair is one row on the overview, named after its controller.
const connectionRow = (page: Page, controller: string) =>
  page.getByRole("region", { name: `连接 ${controller}`, exact: true });
// Cards link to their own page by name.
const openPage = (page: Page, name: string) =>
  page.getByRole("link", { name, exact: true }).first().click();

test("start, stop and hard switch have explicit feedback and exclusive choices", async ({
  page,
}) => {
  const mock = await setup(page);
  const connection = connectionRow(page, "我的手机入口");
  await connection.getByRole("button", { name: "启动", exact: true }).click();
  await expect(
    connection.getByText("正在建立连接", { exact: true }),
  ).toBeVisible();
  await connection.getByRole("button", { name: "停止", exact: true }).click();
  await expect(connection.getByText("已暂停", { exact: true })).toBeVisible();
  await openPage(page, "我的手机入口");
  await expect(
    page.getByRole("heading", { name: "我的手机入口", exact: true, level: 1 }),
  ).toBeVisible();
  // Paired devices are listed on the controller's own page.
  await expect(page.getByText("我的 iPad", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "切换被控端", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("可能需要在 ChatGPT 中重新进入设备");
  // A device already used by another controller is not offered.
  await expect(
    dialog.getByRole("option").filter({ hasText: "已占用出口" }),
  ).toHaveCount(0);
  await dialog.getByLabel("切换到").selectOption("out-two");
  await dialog.getByRole("button", { name: "断开并切换" }).click();
  await expect(page.locator(".link-panel")).toContainText("个人电脑出口");
  await expect(page.locator(".notice")).toContainText(
    "请在已配对设备的 ChatGPT 中重新进入远程设备",
  );
  expect(mock.calls.filter((c) => c.path.endsWith("/switch"))).toHaveLength(1);
});

test("mobile creates a controlled device, authorizes and pairs its computer without storing callback data", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const mock = await setup(page);
  await page.getByRole("button", { name: "新建被控端", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("电脑上登录的账号").selectOption({ index: 1 });
  await dialog.getByLabel("名称").fill("新工作出口");
  await dialog.getByRole("button", { name: "添加并授权", exact: true }).click();
  // Creation continues straight into authorization, then pairing.
  await expect(
    page.getByRole("link", { name: "打开授权页面" }),
  ).toHaveAttribute("rel", "noreferrer noopener");
  await page
    .getByLabel("完整回调地址")
    .fill("http://localhost:1455/auth/callback?code=synthetic&state=synthetic");
  await page.getByRole("button", { name: "保存授权" }).click();
  await expect(dialog.getByRole("heading")).toHaveText("配对电脑");
  expect(
    await page.evaluate(() => [localStorage.length, sessionStorage.length]),
  ).toEqual([0, 0]);
  await dialog.getByLabel("电脑配对码").fill("DEMO-1234");
  await dialog.getByRole("button", { name: "配对电脑", exact: true }).click();
  await expect(page.locator(".notice")).toContainText("已与 演示电脑 配对");
  const device = page.getByRole("article", {
    name: "被控端 新工作出口",
    exact: true,
  });
  await expect(device).toContainText("演示电脑");
  // The computer is fixed by pairing; there is no separate selection step.
  expect(
    mock.calls.filter((c) => c.path === "/api/outputs/created-1/pair"),
  ).toEqual([
    {
      path: "/api/outputs/created-1/pair",
      method: "POST",
      body: { code: "DEMO-1234" },
    },
  ]);
  expect(mock.calls.some((c) => c.path.endsWith("/target"))).toBe(false);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: "test-results/mobile-overview.png",
    fullPage: true,
  });
});

test("re-pairing a computer warns that it replaces the current one", async ({
  page,
}) => {
  const mock = await setup(page);
  await openPage(page, "工作电脑出口");
  await page.getByRole("button", { name: "重新配对电脑", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("这会替换当前电脑 Studio MacBook Pro");
  await dialog.getByLabel("电脑配对码").fill("DEMO-1234");
  await dialog.getByRole("button", { name: "替换并配对", exact: true }).click();
  await expect(page.locator(".facts").first()).toContainText("演示电脑");
  expect(
    mock.calls.filter((c) => c.path === "/api/outputs/out-one/pair"),
  ).toHaveLength(1);
});

test("a failed re-pair keeps the current computer and explains why", async ({
  page,
}) => {
  await setup(page);
  await page.route("**/api/outputs/out-one/pair", (route) =>
    route.fulfill({
      status: 409,
      json: { error: "out_pairing_cleanup_pending" },
    }),
  );
  await openPage(page, "工作电脑出口");
  await page.getByRole("button", { name: "重新配对电脑", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("电脑配对码").fill("DEMO-1234");
  await dialog.getByRole("button", { name: "替换并配对", exact: true }).click();
  await expect(dialog).toContainText("原电脑的访问还没有完全撤销");
  await dialog.getByRole("button", { name: "关闭对话框" }).click();
  await expect(page.locator(".facts").first()).toContainText(
    "Studio MacBook Pro",
  );
});

test("an interrupted re-pair explains why the connection can't start and offers to finish it", async ({
  page,
}) => {
  const mock = await setup(page);
  const output = mock.data.configuration.value.endpoints.find(
    (i) => i.endpoint.device_id === "out-one",
  )!;
  output.pairing_pending = true;
  Object.assign(mock.data.chains[0], {
    enabled: true,
    status: "failed",
    connection: "invalid_state",
  });
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  const device = page.getByRole("article", {
    name: "被控端 工作电脑出口",
    exact: true,
  });
  await expect(device).toContainText("重新配对未完成");
  await device
    .getByRole("button", { name: "重新配对电脑", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("电脑配对码").fill("DEMO-1234");
  await dialog.getByRole("button", { name: "替换并配对", exact: true }).click();
  await expect(device).not.toContainText("重新配对未完成");
});

test("an expired sign-in marks the end that stopped and resumes after signing in again", async ({
  page,
}) => {
  const mock = await setup(page);
  // The controlled device uses its own account; only the controller's expired.
  const deviceAccount = {
    user_id: "device-user",
    workspace_id: "device-workspace",
    email: "device@example.com",
    sign_in_required: false,
  };
  const accounts = mock.data.configuration.value.accounts;
  accounts[0].email = "controller@example.com";
  accounts[0].sign_in_required = true;
  accounts.push(deviceAccount);
  for (const item of mock.data.configuration.value.endpoints)
    if (item.endpoint.kind === "out")
      item.endpoint.account = {
        user_id: deviceAccount.user_id,
        workspace_id: deviceAccount.workspace_id,
      };
  Object.assign(mock.data.chains[0], {
    enabled: true,
    status: "failed",
    connection: "sign_in_required",
    side: "controller",
  });
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  const row = connectionRow(page, "我的手机入口");
  const controller = row.getByRole("article", {
    name: "控制端 我的手机入口",
    exact: true,
  });
  const device = row.getByRole("article", {
    name: "被控端 工作电脑出口",
    exact: true,
  });
  await expect(controller).toHaveClass(/needs-attention/);
  await expect(controller).toContainText(
    "controller@example.com 的登录无法续期",
  );
  await expect(device).not.toHaveClass(/needs-attention/);
  // Retrying can't help until the account signs in again.
  await expect(
    row.getByRole("button", { name: "重试", exact: true }),
  ).toHaveCount(0);

  await page.getByRole("button", { name: "账号", exact: true }).click();
  const first = page.locator(".account-row").first();
  await expect(first).toHaveAttribute("aria-label", "controller@example.com");
  await expect(first).toContainText("登录已失效");
  await expect(first).toContainText("控制端：我的手机入口和另一手机入口");
  await expect(page.locator(".account-row.needs-attention")).toHaveCount(1);
  await page.getByRole("button", { name: "检查全部登录", exact: true }).click();
  await expect(page.locator(".notice")).toContainText("1 个账号检查未通过");

  await first.getByRole("button", { name: "重新登录", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  expect(
    mock.calls.find((c) => c.path === "/api/accounts/login")?.body,
  ).toEqual({
    expected: { user_id: "demo-user", workspace_id: "demo-workspace" },
  });
  await dialog
    .getByLabel("完整回调地址")
    .fill("http://localhost:1455/auth/callback?code=synthetic&state=synthetic");
  await dialog.getByRole("button", { name: "保存授权" }).click();
  await expect(page.locator(".account-row.needs-attention")).toHaveCount(0);
  await page.getByRole("button", { name: "远程控制", exact: true }).click();
  await expect(controller).not.toHaveClass(/needs-attention/);
  expect(mock.data.chains[0].status).toBe("running");
});

test("a lost device authorization is fixed on the device card", async ({
  page,
}) => {
  const mock = await setup(page);
  Object.assign(mock.data.chains[0], {
    enabled: true,
    status: "failed",
    connection: "device_authorization_required",
    side: "device",
  });
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  const row = connectionRow(page, "我的手机入口");
  const device = row.getByRole("article", {
    name: "被控端 工作电脑出口",
    exact: true,
  });
  await expect(device).toHaveClass(/needs-attention/);
  await expect(device).toContainText("被控端授权失效");
  await device.getByRole("button", { name: "重新授权", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  expect(
    mock.calls.some((c) => c.path === "/api/outputs/out-one/authorize"),
  ).toBe(true);
  // On the controller's own page, the link points to the end that needs work.
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "关闭对话框", exact: true })
    .click();
  await openPage(page, "我的手机入口");
  await expect(
    page.getByRole("button", { name: "检查被控端", exact: true }),
  ).toBeVisible();
});

test("each card offers the fix that can actually work", async ({ page }) => {
  const mock = await setup(page);
  const [inOne, outOne, outTwo, inTwo] =
    mock.data.configuration.value.endpoints;
  // An interrupted re-authorization of an active device recovers in place.
  Object.assign(outTwo, {
    registration_uncertain: true,
    registration_recoverable: true,
  });
  // A plain refusal is not fixed by registering again; retry instead.
  Object.assign(mock.data.chains[0], {
    enabled: true,
    status: "failed",
    connection: "access_denied",
    side: "controller",
  });
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  const refused = page.getByRole("article", {
    name: `控制端 ${inOne.endpoint.display_name}`,
    exact: true,
  });
  await expect(refused).toContainText("服务拒绝访问");
  await expect(refused.getByRole("button")).toHaveCount(0);
  await expect(
    connectionRow(page, inOne.endpoint.display_name).getByRole("button", {
      name: "重试",
      exact: true,
    }),
  ).toBeVisible();
  const recovering = page.getByRole("article", {
    name: `被控端 ${outTwo.endpoint.display_name}`,
    exact: true,
  });
  await recovering
    .getByRole("button", { name: "恢复授权", exact: true })
    .click();
  await expect(recovering).not.toContainText("恢复授权");
  expect(
    mock.calls.some(
      (c) => c.path === `/api/outputs/${outTwo.endpoint.device_id}/recover`,
    ),
  ).toBe(true);

  // Signing in comes before anything that needs the account, such as registration.
  inTwo.registered = false;
  mock.data.configuration.value.accounts[0].sign_in_required = true;
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  const unregistered = page.getByRole("article", {
    name: `控制端 ${inTwo.endpoint.display_name}`,
    exact: true,
  });
  await expect(
    unregistered.getByRole("button", { name: "重新登录", exact: true }),
  ).toBeVisible();
  await expect(
    unregistered.getByRole("button", { name: "重试注册", exact: true }),
  ).toHaveCount(0);
  void outOne;
});

test("connecting a free controller binds and starts in one step", async ({
  page,
}) => {
  const mock = await setup(page);
  mock.data.chains.shift();
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  const controller = page.getByRole("article", {
    name: "控制端 我的手机入口",
    exact: true,
  });
  await controller
    .getByRole("button", { name: "连接被控端", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("被控端").selectOption("out-two");
  await dialog.getByRole("button", { name: "连接并启动", exact: true }).click();
  await expect(connectionRow(page, "我的手机入口")).toContainText(
    "个人电脑出口",
  );
  expect(
    mock.calls
      .filter((c) => c.path.startsWith("/api/inputs/in-one/"))
      .map((c) => [c.path, c.body]),
  ).toEqual(
    expect.arrayContaining([
      ["/api/inputs/in-one/binding", { output: "out-two" }],
      ["/api/inputs/in-one/start", {}],
    ]),
  );
});

test("a controller keeps one device and pairing again replaces it", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const mock = await setup(page);
  const originalBinding = structuredClone(mock.data.chains[0].binding);
  await openPage(page, "我的手机入口");
  const device = page.getByRole("region", { name: "设备", exact: true });
  await expect(device).toContainText("我的 iPad");
  await expect(device).toContainText("iOS · iPad");
  await expect(device).toContainText("上次连接");
  // Both the overview and this page ask the backend to check the device.
  await expect
    .poll(
      () =>
        mock.calls.filter((c) => c.path === "/api/inputs/in-one/device/sync")
          .length,
    )
    .toBeGreaterThanOrEqual(2);
  expect(
    mock.calls.some((c) => c.path === "/api/inputs/in-two/device/sync"),
  ).toBe(true);
  await expect(
    device.getByRole("button", { name: "添加设备", exact: true }),
  ).toHaveCount(0);
  await device
    .getByRole("button", { name: "重新配对设备", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("这会替换当前设备 我的 iPad");
  expect(mock.calls.some((call) => call.path.endsWith("/pair"))).toBe(false);
  await dialog.getByRole("button", { name: "移除并配对", exact: true }).click();
  await expect(dialog).toContainText("DEMO1234");
  // The replaced device must not read as the newly paired one.
  await expect(dialog).not.toContainText("已与");
  await expect(device).toContainText("未配对设备");
  mock.pairDevice("in-one", {
    client_id: "new-phone",
    name: "新手机",
    platform: "Android",
  });
  events.emit("changed");
  await expect(dialog).toContainText("已与 新手机 配对");
  await dialog.getByRole("button", { name: "完成", exact: true }).click();
  await expect(device).toContainText("新手机");
  expect(mock.data.chains[0].binding).toEqual(originalBinding);
  expect(
    mock.calls.filter((call) => /\/(start|stop)$/.test(call.path)),
  ).toHaveLength(0);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
});

test("a controller without a device says so on its card and pairs from there", async ({
  page,
}) => {
  const mock = await setup(page);
  mock.data.configuration.value.endpoints.find(
    (item) => item.endpoint.device_id === "in-two",
  )!.paired_device = null;
  events.emit("changed");
  const card = page.getByRole("article", { name: "控制端 另一手机入口" });
  await expect(card).toContainText("未配对设备");
  await expect(card).toHaveClass(/needs-attention/);
  let cleanupFails = true;
  await page.route("**/api/inputs/in-two/pair", async (route) => {
    if (cleanupFails)
      await route.fulfill({
        status: 409,
        json: { error: "in_pairing_cleanup_pending" },
      });
    else await route.fallback();
  });
  await card.getByRole("button", { name: "配对设备", exact: true }).click();
  await expect(page.locator(".notice")).toContainText(
    "当前设备的访问还没能完全移除",
  );
  cleanupFails = false;
  await card.getByRole("button", { name: "配对设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("DEMO1234");
  await expect(dialog).toContainText("只在一台设备上输入");
  mock.pairDevice("in-two", { client_id: "pixel", name: "Pixel 9" });
  events.emit("changed");
  await expect(dialog).toContainText("已与 Pixel 9 配对");
  await dialog.getByRole("button", { name: "完成", exact: true }).click();
  await expect(card).toContainText("Pixel 9");
  await expect(card).not.toHaveClass(/needs-attention/);

  // A second device using the code removes both; the card and dialog say why.
  await card.getByRole("link", { name: "另一手机入口", exact: true }).click();
  await page.getByRole("button", { name: "重新配对设备", exact: true }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "移除并配对", exact: true })
    .click();
  await expect(dialog).toContainText("DEMO1234");
  mock.pairingFailed("in-two", "in_pairing_multiple_devices");
  events.emit("changed");
  await expect(dialog).toContainText("有不止一台设备使用了配对码");
  await dialog
    .getByRole("button", { name: "重新配对设备", exact: true })
    .click();
  await expect(dialog).toContainText("DEMO1234");
  await expect(dialog).not.toContainText("有不止一台设备");
  await dialog.getByRole("button", { name: "关闭对话框", exact: true }).click();

  // A check finding extra devices keeps the record and asks to pair again.
  Object.assign(
    mock.data.configuration.value.endpoints.find(
      (item) => item.endpoint.device_id === "in-two",
    )!,
    {
      paired_device: { client_id: "pixel", name: "Pixel 9" },
      pairing_pending: false,
      pairing_error: "in_multiple_devices",
    },
  );
  events.emit("changed");
  const attention = page.locator(".attention");
  await expect(attention).toContainText("多台设备");
  await expect(attention).toContainText("重新配对，只保留一台");
  await attention
    .getByRole("button", { name: "重新配对设备", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toContainText(
    "这会替换当前设备 Pixel 9",
  );
});

for (const previous of ["device", "conflict"] as const) {
  for (const outcome of ["paired", "conflict"] as const) {
    test(`re-pairing shows a direct ${outcome} result after an old ${previous} without an intermediate snapshot`, async ({
      page,
    }) => {
      const mock = await setup(page);
      if (previous === "conflict") {
        mock.pairingFailed("in-two", "in_pairing_multiple_devices");
        events.emit("changed");
        await expect(
          page.getByRole("article", { name: "控制端 另一手机入口" }),
        ).toContainText("有不止一台设备使用了配对码");
      }
      await openPage(page, "另一手机入口");
      let pairing = false;
      let releaseSnapshot!: () => void;
      let snapshotRequested!: () => void;
      const snapshotGate = new Promise<void>((resolve) => {
        releaseSnapshot = resolve;
      });
      const requested = new Promise<void>((resolve) => {
        snapshotRequested = resolve;
      });
      await page.route("**/api/state", async (route) => {
        if (pairing) {
          snapshotRequested();
          await snapshotGate;
        }
        await route.fallback();
      });
      await page.route("**/api/inputs/in-two/pair", async (route) => {
        const response = await mock.handle("/api/inputs/in-two/pair", "POST");
        // The first readable snapshot is already final; a watch notification can
        // coalesce the waiting state. Even the same device/error is a new result.
        if (outcome === "paired")
          mock.pairDevice("in-two", { client_id: "pixel", name: "Pixel 9" });
        else mock.pairingFailed("in-two", "in_pairing_multiple_devices");
        pairing = true;
        await route.fulfill(response);
      });
      try {
        await page
          .getByRole("button", { name: "重新配对设备", exact: true })
          .click();
        const dialog = page.getByRole("dialog");
        if (previous === "device")
          await dialog
            .getByRole("button", { name: "移除并配对", exact: true })
            .click();
        await requested;
        await expect(dialog).toContainText("DEMO1234");
        // The old snapshot is not this attempt's result while its refresh waits.
        await expect(dialog).not.toContainText("已与 Pixel 9 配对");
        await expect(dialog.getByRole("alert")).toHaveCount(0);
        releaseSnapshot();
        await expect(dialog).toContainText(
          outcome === "paired"
            ? "已与 Pixel 9 配对"
            : "有不止一台设备使用了配对码",
        );
      } finally {
        releaseSnapshot();
      }
    });
  }
}

test("a controlled device shows its computer's state without a running connection", async ({
  page,
}) => {
  const mock = await setup(page);
  // Free computers are checked too, not only the ones in a running connection.
  await expect
    .poll(() =>
      mock.calls.some((c) => c.path === "/api/outputs/out-two/device/sync"),
    )
    .toBe(true);
  const card = page.getByRole("article", { name: "被控端 个人电脑出口" });
  await expect(card).toContainText("在线");
  await card.getByRole("link", { name: "个人电脑出口", exact: true }).click();
  const computer = page.getByRole("region", { name: "电脑", exact: true });
  await expect(computer).toContainText("上次在线");

  Object.assign(
    mock.data.configuration.value.endpoints.find(
      (item) => item.endpoint.device_id === "out-two",
    )!,
    { target_online: false, pairing_error: "out_target_missing" },
  );
  events.emit("changed");
  await expect(computer).toContainText("离线");
  const attention = page.locator(".attention");
  await expect(attention).toContainText("电脑不可用");
  await expect(attention).toContainText("不再允许这个被控端访问");
  await attention
    .getByRole("button", { name: "重新配对电脑", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toContainText("电脑配对码");
});

test("lost mutation response is not retried and unknown deletion stays pending", async ({
  page,
}) => {
  const mock = await setup(page);
  let starts = 0;
  await page.route("**/api/inputs/in-one/start", async (route) => {
    starts++;
    await route.abort();
  });
  await connectionRow(page, "我的手机入口")
    .getByRole("button", { name: "启动", exact: true })
    .click();
  await expect(page.locator(".notice")).toContainText("服务可能仍在处理");
  expect(starts).toBe(1);
  await openPage(page, "工作电脑出口");
  await page.getByRole("button", { name: "删除被控端", exact: true }).click();
  await page.getByRole("button", { name: "停止并删除" }).click();
  const attention = page.locator(".attention");
  await expect(attention).toContainText("删除待完成");
  await expect(
    attention.getByRole("button", { name: "重试清理" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "授权", exact: true }),
  ).toHaveCount(0);
  expect(mock.data.chains.some((c) => c.binding.out_id === "out-one")).toBe(
    false,
  );
});

test("expired session clears private state and authorization dialog", async ({
  page,
}) => {
  const mock = await setup(page);
  await page.getByRole("button", { name: "账号", exact: true }).click();
  await page.getByRole("button", { name: "添加账号", exact: true }).click();
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.getByRole("button", { name: "复制授权链接", exact: true }).click();
  await expect(page.getByRole("dialog")).toContainText("授权链接已复制");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    "https://auth.openai.com/oauth/authorize?state=synthetic",
  );
  expect(
    mock.calls.filter(
      (c) => c.path.startsWith("/api/logins/") && c.method === "POST",
    ),
  ).toHaveLength(0);
  await page.evaluate(() =>
    Object.defineProperty(navigator.clipboard, "writeText", {
      value: async () => {
        throw new Error("denied");
      },
      configurable: true,
    }),
  );
  await page.getByRole("button", { name: "复制授权链接", exact: true }).click();
  await expect(page.getByLabel("授权链接（手动复制）")).toHaveValue(
    "https://auth.openai.com/oauth/authorize?state=synthetic",
  );
  await page
    .getByLabel("完整回调地址")
    .fill("http://localhost:1455/auth/callback?code=synthetic");
  // Expire at submission, so background state reads cannot remove the button first.
  await page.route("**/api/logins/*", async (route) => {
    if (route.request().method() === "POST") mock.expire();
    await route.fallback();
  });
  await page.getByRole("button", { name: "保存授权" }).click();
  await expect(page.getByRole("heading", { name: "登录管理台" })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByText("demo-workspace", { exact: false })).toHaveCount(
    0,
  );
  await expect(page.getByRole("alert")).toContainText("登录已失效");
});

test("pending actions are named and cannot be submitted twice", async ({
  page,
}) => {
  await setup(page);
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  let calls = 0;
  await page.route("**/api/accounts/check", async (route) => {
    calls++;
    await gate;
    await route.fulfill({ json: { result: "done" } });
  });
  await page.getByRole("button", { name: "账号", exact: true }).click();
  await page.getByRole("button", { name: "检查登录" }).click();
  await expect(page.locator(".notice")).toContainText("正在检查账号登录");
  await expect(
    page.getByRole("button", { name: "正在检查…", exact: true }),
  ).toBeDisabled();
  await expect(page.getByRole("button", { name: "退出登录" })).toBeDisabled();
  release();
  await expect(page.locator(".notice")).toContainText("账号凭据可用");
  expect(calls).toBe(1);
});

test("foundation and disconnected services provide a retry instead of controls", async ({
  page,
}) => {
  await page.route("**/api/**", (route) =>
    route.fulfill({ json: { version: "0.1.0", remoteControl: false } }),
  );
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "管理服务尚未配置" }),
  ).toBeVisible();
  await page.route("**/api/**", (route) => route.abort());
  await page.getByRole("button", { name: "重新检查" }).click();
  await expect(
    page.getByRole("heading", { name: "暂时无法连接服务" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "启动" })).toHaveCount(0);
});

test("failed consumed authorization can be restarted and an expired dialog can be closed", async ({
  page,
}) => {
  await setup(page);
  await page.getByRole("button", { name: "账号", exact: true }).click();
  await page.getByRole("button", { name: "添加账号", exact: true }).click();
  await page.route("**/api/logins/*", (route) =>
    route.fulfill({
      status: 409,
      json: { error: "operation_failed_check_authorization_or_state" },
    }),
  );
  await page
    .getByLabel("完整回调地址")
    .fill("http://localhost:1455/auth/callback?code=synthetic");
  await page.getByRole("button", { name: "保存授权" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator(".notice")).toContainText("操作未完成");
  await page.getByRole("button", { name: "添加账号", exact: true }).click();
  await page.getByRole("button", { name: "关闭对话框" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("checkpointed output can recover while an unrecorded result cannot be retried blindly", async ({
  page,
}) => {
  const mock = await setup(page);
  mock.data.chains = [];
  const output = mock.data.configuration.value.endpoints.find(
    (i) => i.endpoint.device_id === "out-one",
  )!;
  output.lifecycle = "draft";
  output.registered = false;
  output.registration_uncertain = true;
  output.registration_recoverable = true;
  const unknown = mock.data.configuration.value.endpoints.find(
    (i) => i.endpoint.device_id === "out-two",
  )!;
  unknown.lifecycle = "draft";
  unknown.registered = false;
  unknown.registration_uncertain = true;
  unknown.registration_recoverable = false;
  await page.getByRole("button", { name: "刷新状态", exact: true }).click();
  const recorded = page.getByRole("article", {
    name: "被控端 工作电脑出口",
    exact: true,
  });
  await expect(
    recorded.getByRole("button", { name: "授权", exact: true }),
  ).toHaveCount(0);
  await recorded.getByRole("button", { name: "恢复授权" }).click();
  await expect(recorded.locator(".attention")).toHaveCount(0);
  await expect(page.locator(".notice")).toContainText("原设备身份和密钥");
  const missing = page.getByRole("article", {
    name: "被控端 个人电脑出口",
    exact: true,
  });
  await expect(missing.getByRole("button", { name: "恢复授权" })).toHaveCount(
    0,
  );
  await expect(
    missing.getByRole("button", { name: "授权", exact: true }),
  ).toHaveCount(0);
  await expect(
    missing.getByRole("button", { name: "补回注册记录" }),
  ).toBeVisible();
});

for (const lifecycle of ["draft", "deleting"] as const) {
  test(`missing ${lifecycle} OUT verifies its original ID without silently reactivating`, async ({
    page,
  }) => {
    if (lifecycle === "draft")
      await page.setViewportSize({ width: 390, height: 844 });
    const mock = await setup(page);
    mock.data.chains = [];
    const output = mock.data.configuration.value.endpoints.find(
      (i) => i.endpoint.device_id === "out-one",
    )!;
    Object.assign(output, {
      lifecycle,
      registered: false,
      registration_uncertain: true,
      registration_recoverable: false,
    });
    await page.getByRole("button", { name: "刷新状态", exact: true }).click();
    const card = page.getByRole("article", {
      name: "被控端 工作电脑出口",
      exact: true,
    });
    // Reauthentication stays bound to the endpoint's existing account, including during deletion.
    await card.getByRole("button", { name: "重新登录所属账号" }).click();
    expect(
      mock.calls.findLast((c) => c.path === "/api/accounts/login")?.body
        .expected,
    ).toEqual(output.endpoint.account);
    await page.getByRole("button", { name: "关闭对话框" }).click();
    await card.getByRole("button", { name: "补回注册记录" }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("补回记录后被控端保持停用");
    await dialog.getByLabel("原注册 client ID").fill("wrong-controller");
    await dialog.getByRole("button", { name: "核验并补回记录" }).click();
    await expect(dialog.getByRole("status")).toContainText(
      "未能确认该注册属于此出口的原身份",
    );
    expect(output.registration_recoverable).toBe(false);
    await dialog.getByLabel("原注册 client ID").fill("original-controller");
    await dialog.getByRole("button", { name: "核验并补回记录" }).click();
    await expect(dialog).toHaveCount(0);
    expect(output.lifecycle).toBe(lifecycle);
    expect(output.registered).toBe(false);
    expect(
      mock.calls.filter(
        (c) =>
          c.path.endsWith("/recover") ||
          c.path.endsWith("/authorize") ||
          (c.method === "DELETE" && c.path.startsWith("/api/endpoints/")),
      ),
    ).toHaveLength(0);
    await page.reload();
    if (lifecycle === "draft") {
      await card.getByRole("button", { name: "恢复授权" }).click();
      await expect(card.locator(".attention")).toHaveCount(0);
    } else {
      await expect(card.getByRole("button", { name: "恢复授权" })).toHaveCount(
        0,
      );
      await expect(
        card.getByRole("button", { name: "重试清理" }),
      ).toBeVisible();
    }
  });
}

test("live connection observations, stream reconnect and session expiry", async ({
  page,
}) => {
  const mock = await setup(page);
  await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible();
  const connection = connectionRow(page, "我的手机入口");
  const chain = mock.data.chains[0];
  chain.enabled = true;
  chain.status = "running";
  chain.connection = "target_offline";
  events.emit("changed");
  await expect(
    connection.getByText("被控端离线", { exact: true }),
  ).toBeVisible();
  await expect(connection.getByText("连接可用", { exact: true })).toHaveCount(
    0,
  );
  chain.connection = "connected";
  events.emit("changed");
  await expect(connection.getByText("连接可用", { exact: true })).toBeVisible();
  chain.connection = "reconnecting";
  events.emit("changed");
  await expect(
    connection.getByText("正在重新连接", { exact: true }),
  ).toBeVisible();
  events.disconnect();
  await expect(
    page.getByText("每隔几秒检查一次状态", { exact: true }),
  ).toBeVisible();
  chain.binding.out_id = "out-two";
  chain.connection = "connected";
  events.resume();
  await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible({
    timeout: 10000,
  });
  await expect(connection).toContainText("个人电脑出口");
  await expect(connection.getByText("连接可用", { exact: true })).toBeVisible();
  chain.status = "failed";
  chain.connection = "sign_in_required";
  chain.side = "controller";
  events.emit("changed");
  await expect(
    connection.getByText("登录已失效", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    connection
      .getByRole("article", { name: "控制端 我的手机入口", exact: true })
      .getByRole("button", { name: "重新登录", exact: true }),
  ).toBeVisible();
  mock.expire();
  await expect(page.getByLabel("用户名", { exact: true })).toBeVisible();
  await expect(connection).toHaveCount(0);
  await expect.poll(() => events.clients).toBe(0);
});

test("failed state reads recover while SSE heartbeats remain healthy", async ({
  page,
}) => {
  const mock = await setup(page);
  await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible();
  let failing = true;
  await page.route("**/api/state", async (route) => {
    if (failing)
      await route.fulfill({
        status: 503,
        json: { error: "temporarily_unavailable" },
      });
    else await route.fallback();
  });
  events.emit("changed");
  await expect(
    page.getByText("状态更新中断，下面可能是旧数据。请刷新状态后再操作。"),
  ).toBeVisible();
  await expect(
    connectionRow(page, "我的手机入口").getByRole("button", {
      name: "启动",
      exact: true,
    }),
  ).toBeDisabled();
  mock.data.chains[0].connection = "checking_target";
  failing = false;
  await expect(
    page.getByText("正在确认被控端状态", { exact: true }),
  ).toBeVisible({ timeout: 10000 });
  await expect(
    connectionRow(page, "我的手机入口").getByRole("button", {
      name: "启动",
      exact: true,
    }),
  ).toBeEnabled();
});

test("session expiry during a slow operation permits login and ignores its late dialog", async ({
  page,
}) => {
  const mock = await setup(page);
  let release!: () => void;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/accounts/login", async (route) => {
    await pending;
    await route.fulfill({
      json: {
        result: "login",
        value: {
          id: "expired-session-login",
          url: "https://auth.openai.com/unused-synthetic",
          expires_at: 2000000000,
        },
      },
    });
  });
  try {
    await page.getByRole("button", { name: "账号", exact: true }).click();
    await page.getByRole("button", { name: "添加账号", exact: true }).click();
    await expect(page.locator(".notice")).toContainText("正在准备授权");
    mock.expire();
    await expect(
      page.getByRole("button", { name: "登录", exact: true }),
    ).toBeEnabled();
    await page.getByLabel("用户名", { exact: true }).fill("demo");
    await page.getByLabel("密码", { exact: true }).fill("synthetic-password");
    await page.getByRole("button", { name: "登录", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "退出登录", exact: true }),
    ).toBeVisible();
    release();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await page.getByRole("button", { name: "刷新状态", exact: true }).click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(page.locator(".notice")).not.toContainText("授权链接已准备好");
  } finally {
    release();
  }
});

test("readable account labels cover lists, endpoint ownership and selectors without changing identity requests", async ({
  page,
}) => {
  const mock = await setup(page);
  const account = mock.data.configuration.value.accounts[0]!;
  account.email = "person@example.test";
  account.name = "Example Person";
  mock.data.configuration.value.accounts.push(
    { ...account, workspace_id: "second-workspace" },
    { user_id: "legacy-user", workspace_id: "legacy-workspace" },
  );
  await page.reload();
  await page.getByRole("button", { name: "账号", exact: true }).click();
  await expect(
    page.getByRole("heading", {
      name: "person@example.test · 工作区 demo-workspace",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", {
      name: "person@example.test · 工作区 second-workspace",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", {
      name: "账号 legacy-user · 工作区 legacy-workspace",
      exact: true,
    }),
  ).toBeVisible();
  const first = page.locator(".account-row").filter({
    has: page.getByRole("heading", {
      name: "person@example.test · 工作区 demo-workspace",
      exact: true,
    }),
  });
  await expect(
    first.getByText("用户 ID：demo-user", { exact: true }),
  ).not.toBeVisible();
  await first.getByText("账号详情", { exact: true }).click();
  await expect(
    first.getByText("用户 ID：demo-user", { exact: true }),
  ).toBeVisible();
  await first.getByRole("button", { name: "检查登录", exact: true }).click();
  expect(
    mock.calls.findLast((c) => c.path === "/api/accounts/check")?.body,
  ).toEqual({
    account: { user_id: "demo-user", workspace_id: "demo-workspace" },
  });
  await page.getByRole("button", { name: "远程控制", exact: true }).click();
  await expect(
    page.getByRole("article", { name: "被控端 工作电脑出口", exact: true }),
  ).toContainText("person@example.test · 工作区 demo-workspace");
  await page.getByRole("button", { name: "新建被控端", exact: true }).click();
  await expect(
    page.getByRole("option", {
      name: "person@example.test · 工作区 second-workspace",
      exact: true,
    }),
  ).toHaveCount(1);
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("电脑上登录的账号").selectOption({ index: 2 });
  await dialog.getByLabel("名称").fill("other-workspace-output");
  await dialog.getByRole("button", { name: "添加并授权", exact: true }).click();
  expect(
    mock.calls.findLast((c) => c.path === "/api/endpoints")?.body.account,
  ).toEqual({ user_id: "demo-user", workspace_id: "second-workspace" });
});

for (const width of [1280, 390]) {
  test(`overview and detail pages fit without horizontal scrolling (${width}px)`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const mock = await setup(page);
    mock.data.chains.pop();
    await page.getByRole("button", { name: "刷新状态", exact: true }).click();
    await expect(page.getByRole("heading", { name: "未连接" })).toBeVisible();
    const fits = () =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);
    expect(await fits()).toBe(true);
    await openPage(page, "我的手机入口");
    await expect(page.locator(".link-panel")).toContainText("工作电脑出口");
    expect(await fits()).toBe(true);
    await page
      .locator(".link-panel")
      .getByRole("link", { name: "工作电脑出口" })
      .click();
    await expect(
      page.getByRole("heading", { name: "工作电脑出口", level: 1 }),
    ).toBeVisible();
    await expect(page.locator(".facts").first()).toContainText(
      "Studio MacBook Pro",
    );
    expect(await fits()).toBe(true);
  });
}

test("only deployment admins configure persistent proxy and see actual applied state", async ({
  page,
}) => {
  const mock = await setup(page);
  await expect(
    page.getByRole("button", { name: "设置", exact: true }),
  ).toHaveCount(0);
  mock.session.identity.deployment_admin = true;
  let network = {
    configured: false,
    mode: "direct",
    address: null as string | null,
    authenticated: false,
  };
  const saved: unknown[] = [];
  await page.route("**/api/deployment/network", async (route) => {
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON();
      saved.push(body);
      network = {
        configured: true,
        mode: body.proxy_url ? "proxy" : "direct",
        address: body.proxy_url ? "http://127.0.0.1:7890/" : null,
        authenticated: !!body.proxy_url,
      };
    }
    await route.fulfill({ json: { ...network, restart_failures: 0 } });
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "设置", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("首次使用请先选择网络方式，再添加 ChatGPT 账号。"),
  ).toBeVisible();
  await page.getByLabel("连接方式").selectOption("proxy");
  await page
    .getByLabel("代理地址")
    .fill("http://demo:synthetic-proxy-password@127.0.0.1:7890");
  await page.getByRole("button", { name: "保存并应用", exact: true }).click();
  await expect(
    page.getByText("当前生效：http://127.0.0.1:7890/（已配置代理认证）"),
  ).toBeVisible();
  await expect(page.getByLabel("代理地址")).toHaveValue("");
  expect(saved).toEqual([
    { proxy_url: "http://demo:synthetic-proxy-password@127.0.0.1:7890" },
  ]);
  await page.reload();
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByLabel("代理地址")).toHaveValue("");
  await page.getByLabel("连接方式").selectOption("direct");
  await page.getByRole("button", { name: "保存并应用", exact: true }).click();
  await expect(page.getByText("当前生效：直连")).toBeVisible();
  expect(saved[1]).toEqual({ proxy_url: null });
});
