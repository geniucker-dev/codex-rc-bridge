import { test, expect, type Page } from "@playwright/test";
import { createMockApi } from "./mock-api";
async function fixture(page: Page) {
  const mock = createMockApi();
  await mock.handle("/api/session", "POST", { password: "synthetic-password" });
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/events") return route.fulfill({ status: 503, body: "" });
    const response = await mock.handle(
      path,
      route.request().method(),
      route.request().postDataJSON() ?? {},
    );
    await route.fulfill({
      ...response,
      body: response.status === 204 ? "" : response.body,
    });
  });
  return mock;
}
test("refresh, history and brand preserve navigation; language changes preserve the open form", async ({
  page,
}) => {
  await fixture(page);
  await page.goto("/#/accounts");
  await expect(
    page.getByRole("heading", { name: "账号", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "账号", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "远程控制", exact: true }).click();
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "账号", exact: true }),
  ).toBeVisible();
  await page.locator(".brand").click();
  await expect(page).toHaveURL(/#\/accounts$/);
  await page.getByRole("button", { name: "远程控制", exact: true }).click();
  await page.getByRole("button", { name: "新建控制端", exact: true }).click();
  await page.getByLabel("名称", { exact: true }).fill("unchanged-identity");
  // A modal correctly prevents interaction outside it. Simulate a system-language preference change.
  await page.evaluate(() => {
    localStorage.setItem("bridge.language", "en");
    window.dispatchEvent(
      new StorageEvent("storage", { key: "bridge.language" }),
    );
  });
  await expect(page.getByRole("dialog")).toContainText("New controller");
  await expect(page.getByLabel("Name", { exact: true })).toHaveValue(
    "unchanged-identity",
  );
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Remote control", exact: true }),
  ).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
});
test("unsupported system language falls back to English and ordinary users can choose Chinese", async ({
  page,
}) => {
  await page.addInitScript(() =>
    Object.defineProperty(navigator, "languages", { value: ["fr-FR"] }),
  );
  await fixture(page);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Remote control", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(
    page.getByRole("heading", { name: "远程控制", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "远程控制", exact: true }),
  ).toBeVisible();
});
test("disconnect requires confirmation and keeps both sides", async ({
  page,
}) => {
  const mock = await fixture(page);
  // Older connection links open the controller's page.
  await page.goto("/#/connections/in-one");
  await expect(page).toHaveURL(/#\/controllers\/in-one$/);
  await page.getByRole("button", { name: "断开连接", exact: true }).click();
  await expect(page.getByRole("dialog")).toContainText("之后可以重新连接");
  await page.getByRole("button", { name: "关闭对话框", exact: true }).click();
  expect(mock.data.chains).toHaveLength(2);
  await page.getByRole("button", { name: "断开连接", exact: true }).click();
  await page.getByRole("button", { name: "停止并断开", exact: true }).click();
  // The controller page stays, now offering to connect again.
  await expect(page.locator(".link-panel")).toContainText("未连接");
  await expect(
    page.getByRole("button", { name: "连接被控端", exact: true }),
  ).toBeVisible();
  expect(mock.data.chains.map((c) => c.binding.in_id)).toEqual(["in-two"]);
  expect(
    mock.data.configuration.value.endpoints.map((e) => e.endpoint.device_id),
  ).toContain("in-one");
});
