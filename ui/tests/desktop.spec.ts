import { test, expect } from "@playwright/test";
import { createMockApi } from "./mock-api";

for (const outcome of [
  "complete",
  "expired",
  "failed",
  "manual-overlap",
] as const) {
  test(`desktop authorization ${outcome} uses IPC and ends polling`, async ({
    page,
  }) => {
    const data = createMockApi().data;
    let finished = false;
    let polls = 0;
    const calls: string[] = [];
    await page.exposeFunction(
      "desktopInvoke",
      async (command: string, args: any) => {
        calls.push(command + ":" + (args.path ?? ""));
        if (command.startsWith("plugin:event|")) return 1;
        if (command === "desktop_set_language") {
          calls.push("language:" + args.language);
          return;
        }
        if (command === "desktop_open_authorization") {
          expect(args.url).toBe(
            "https://auth.openai.com/oauth/authorize?state=synthetic",
          );
          finished = outcome !== "manual-overlap";
          return;
        }
        expect(command).toBe("desktop_request");
        switch (args.path) {
          case "/capabilities":
            return { management: true, desktop: true };
          case "/session":
            return {
              identity: {
                username: "本机",
                tenant_id: "local",
                expires_at: 0,
                deployment_admin: true,
              },
              csrf: "",
            };
          case "/deployment/network":
            return {
              configured: true,
              mode: "direct",
              address: null,
              authenticated: false,
            };
          case "/state":
            return data;
          case "/accounts/login":
            return {
              result: "login",
              value: {
                id: "local-login",
                url: "https://auth.openai.com/oauth/authorize?state=synthetic",
                expires_at: 9999999999,
                automatic_callback: true,
              },
            };
          case "/logins/local-login":
            if (args.method === "POST") {
              finished = true;
              throw { status: 409, message: "授权正在保存，请等待结果。" };
            }
            polls++;
            return {
              status: finished
                ? outcome === "manual-overlap"
                  ? "complete"
                  : outcome
                : "processing",
              error:
                outcome === "failed"
                  ? { code: "upstream_auth_network" }
                  : undefined,
            };
          default:
            throw new Error("unexpected route");
        }
      },
    );
    await page.addInitScript(() => {
      const w = window as any;
      w.isTauri = true;
      let id = 0;
      w.__TAURI_INTERNALS__ = {
        invoke: (command: string, args: any) => w.desktopInvoke(command, args),
        transformCallback: () => ++id,
      };
      w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    });
    await page.route("**/api/**", () => {
      throw new Error("desktop must not use HTTP management API");
    });
    await page.goto("/");
    await expect(page.getByText("桌面版", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "退出登录" })).toHaveCount(0);
    await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "账号", exact: true }).click();
    await page.getByRole("button", { name: "添加账号", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("授权会自动保存");
    await dialog.getByRole("link", { name: "打开授权页面" }).click();
    if (outcome === "manual-overlap") {
      await dialog
        .getByLabel("完整回调地址")
        .fill("http://localhost:1455/auth/callback?code=synthetic");
      await dialog.getByRole("button", { name: "保存授权" }).click();
    }
    await expect(dialog).toHaveCount(0);
    const expected =
      outcome === "complete" || outcome === "manual-overlap"
        ? "授权已保存"
        : outcome === "expired"
          ? "授权已过期或已取消"
          : "上游授权服务器的通信";
    await expect(page.getByRole("status")).toContainText(expected);
    if (outcome === "complete") {
      await page.getByRole("button", { name: "设置", exact: true }).click();
      await page
        .getByRole("combobox", { name: "语言", exact: true })
        .selectOption("en");
      await expect(
        page.getByRole("heading", { name: "Settings", exact: true }),
      ).toBeVisible();
      await expect.poll(() => calls.includes("language:en")).toBe(true);
      await expect(page).toHaveURL(/#\/settings$/);
    }
    const completedPolls = polls;
    await page.waitForTimeout(1200);
    expect(polls).toBe(completedPolls);
    expect(calls).toContain("desktop_open_authorization:");
  });
}
