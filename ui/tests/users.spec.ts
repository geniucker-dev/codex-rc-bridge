import { test, expect, type Page } from "@playwright/test";
import { createMockApi } from "./mock-api";
import { eventServer } from "./event-server";
let events: ReturnType<typeof eventServer>;
test.afterEach(async () => {
  await events?.close();
});
async function setup(page: Page, admin: boolean) {
  const mock = createMockApi();
  mock.session.identity.deployment_admin = admin;
  events = eventServer(mock);
  await page.route("**/api/**", async (route) => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    if (path === "/api/events") {
      await route.continue({ url: events.url });
      return;
    }
    const response = await mock.handle(
      path,
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
  return mock;
}
const row = (page: Page, name: string) =>
  page.locator(".user-row").filter({
    has: page.getByRole("heading", { name, exact: false }),
  });

test("non-admins have no users page, even by link", async ({ page }) => {
  await setup(page, false);
  const nav = page.getByRole("navigation");
  await expect(nav.getByRole("button", { name: "用户" })).toHaveCount(0);
  await page.goto("/#/users");
  await expect(
    page.getByRole("heading", { name: "远程控制", exact: true }),
  ).toBeVisible();
});

test("admin adds, resets and deletes a user; admins have no actions", async ({
  page,
}) => {
  const mock = await setup(page, true);
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "用户" })
    .click();
  await expect(page.getByText("用户：2")).toBeVisible();
  const self = row(page, "demo");
  await expect(self).toContainText("管理员，在服务器命令行中管理。");
  await expect(self.getByRole("button")).toHaveCount(0);
  await expect(row(page, "alice")).toContainText("登录已锁定一分钟");

  // Add: mismatched passwords are caught locally, a taken name by the server.
  await page.getByRole("button", { name: "添加用户" }).click();
  let dialog = page.getByRole("dialog");
  await dialog.getByLabel("用户名").fill("alice");
  await dialog.getByLabel("密码", { exact: true }).fill("synthetic-pass-1");
  await dialog.getByLabel("再输入一次").fill("synthetic-pass-2");
  await dialog.getByRole("button", { name: "添加用户" }).click();
  await expect(dialog).toContainText("两次输入的新密码不一致。");
  expect(
    mock.calls.filter((c) => c.path === "/api/users" && c.method === "POST"),
  ).toHaveLength(0);
  await dialog.getByLabel("再输入一次").fill("synthetic-pass-1");
  await dialog.getByRole("button", { name: "添加用户" }).click();
  await expect(dialog).toContainText("这个用户名已被使用");
  await dialog.getByLabel("用户名").fill("bob");
  await dialog.getByRole("button", { name: "添加用户" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByText("已添加 bob。")).toBeVisible();
  await expect(row(page, "bob")).toBeVisible();
  await expect(page.getByText("用户：3")).toBeVisible();

  // Reset unlocks a locked user.
  await row(page, "alice").getByRole("button", { name: "重置密码" }).click();
  dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("其连接会继续运行");
  await dialog.getByLabel("新密码").fill("synthetic-pass-3");
  await dialog.getByLabel("再输入一次").fill("synthetic-pass-3");
  await dialog.getByRole("button", { name: "重置密码" }).click();
  await expect(
    page.getByText("密码已重置，alice 已在所有浏览器上退出登录。"),
  ).toBeVisible();
  await expect(row(page, "alice")).not.toContainText("登录已锁定");

  // Delete needs the exact username and warns about ChatGPT leftovers.
  await row(page, "bob").getByText("更多").click();
  await row(page, "bob").getByRole("button", { name: "删除用户" }).click();
  dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("不会在 ChatGPT 那边撤销授权");
  const confirm = dialog.getByRole("button", { name: "删除用户" });
  await expect(confirm).toBeDisabled();
  await dialog.getByLabel("输入 bob 确认").fill("Bob");
  await expect(confirm).toBeDisabled();
  await dialog.getByLabel("输入 bob 确认").fill("bob");
  await confirm.click();
  await expect(page.getByText("已删除 bob 及其全部数据。")).toBeVisible();
  await expect(row(page, "bob")).toHaveCount(0);
  expect(
    mock.calls
      .filter((c) => c.path.startsWith("/api/users/"))
      .map((c) => `${c.method} ${c.path}`),
  ).toEqual([
    "POST /api/users/user-alice/password",
    "DELETE /api/users/user-1",
  ]);
});

test("a user already removed elsewhere refreshes the list", async ({
  page,
}) => {
  const mock = await setup(page, true);
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "用户" })
    .click();
  await expect(row(page, "alice")).toBeVisible();
  mock.users.splice(1, 1);
  await row(page, "alice").getByRole("button", { name: "重置密码" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("新密码").fill("synthetic-pass-3");
  await dialog.getByLabel("再输入一次").fill("synthetic-pass-3");
  await dialog.getByRole("button", { name: "重置密码" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByText("该用户已不存在，列表已刷新。")).toBeVisible();
  await expect(row(page, "alice")).toHaveCount(0);
});

test("every user changes their own password; a wrong current one keeps the dialog and session", async ({
  page,
}) => {
  const mock = await setup(page, false);
  await page.getByRole("button", { name: "修改密码" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("当前密码").fill("wrong-password");
  await dialog.getByLabel("新密码").fill("synthetic-pass-4");
  await dialog.getByLabel("再输入一次").fill("synthetic-pass-4");
  await dialog.getByRole("button", { name: "修改密码" }).click();
  await expect(dialog).toContainText("当前密码不正确");
  await expect(dialog.getByLabel("新密码")).toHaveValue("synthetic-pass-4");
  await dialog.getByLabel("当前密码").fill("synthetic-password");
  await dialog.getByRole("button", { name: "修改密码" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByText("密码已修改")).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "远程控制", exact: true }),
  ).toBeVisible();
  expect(
    mock.calls.filter((c) => c.path === "/api/session/password"),
  ).toHaveLength(2);
});

test("refreshing status reloads users changed in another browser", async ({
  page,
}) => {
  const mock = await setup(page, true);
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "用户" })
    .click();
  await expect(row(page, "alice")).toContainText("登录已锁定");
  mock.users[1].locked = false;
  mock.users.push({
    id: "user-carol",
    username: "carol",
    deployment_admin: false,
    locked: false,
    current: false,
  });
  await page.getByRole("button", { name: "刷新状态" }).click();
  await expect(row(page, "carol")).toBeVisible();
  await expect(row(page, "alice")).not.toContainText("登录已锁定");
});
