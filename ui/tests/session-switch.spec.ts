import { test, expect } from "@playwright/test";
import { createMockApi } from "./mock-api";
import type { Session } from "../src/api";

for (const samePrincipal of [false, true]) {
  test(`shared cookie session replacement clears old operations (${samePrincipal ? "same user" : "different tenant"})`, async ({
    context,
  }) => {
    const origin = "http://127.0.0.1:5175";
    const initial: Session = {
      identity: {
        username: "alice",
        tenant_id: "tenant-alice",
        expires_at: 2000000000,
      },
      csrf: "synthetic-csrf-A",
    };
    const replacement: Session = {
      identity: samePrincipal
        ? { ...initial.identity }
        : { username: "bob", tenant_id: "tenant-bob", expires_at: 2000000000 },
      csrf: "synthetic-csrf-B",
    };
    let validA = true;
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let pending = false;
    let delivered = false;
    let mutation: { cookie: string; csrf: string; status: number } | undefined;
    await context.addCookies([
      {
        name: "review_session",
        value: "A",
        url: origin,
        httpOnly: true,
        sameSite: "Strict",
      },
    ]);
    await context.addInitScript(() => {
      const w = window as any;
      class Source extends EventTarget {
        readyState = 1;
        onerror: (() => void) | null = null;
        constructor() {
          super();
          w.reviewSessionSource = this;
          queueMicrotask(() => {
            if (this.readyState === 1) this.dispatchEvent(new Event("changed"));
          });
        }
        close() {
          this.readyState = 2;
        }
      }
      w.EventSource = Source;
    });
    await context.route("**/api/**", async (route) => {
      const path = new URL(route.request().url()).pathname;
      const method = route.request().method();
      const cookie = /review_session=([AB])/.exec(
        route.request().headers().cookie ?? "",
      )?.[1] as "A" | "B" | undefined;
      const session =
        cookie === "B"
          ? replacement
          : cookie === "A" && validA
            ? initial
            : undefined;
      if (path === "/api/capabilities")
        return route.fulfill({ json: { management: true } });
      if (path === "/api/session" && method === "POST")
        return route.fulfill({
          json: replacement,
          headers: {
            "set-cookie": "review_session=B; Path=/; HttpOnly; SameSite=Strict",
          },
        });
      if (!session)
        return route.fulfill({
          status: 401,
          json: { error: "login_required" },
        });
      if (path === "/api/session" && method === "DELETE") {
        validA = false;
        return route.fulfill({
          status: 204,
          headers: {
            "set-cookie":
              "review_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
          },
        });
      }
      if (path === "/api/session") return route.fulfill({ json: session });
      if (path === "/api/state") {
        const data = createMockApi().data;
        data.configuration.value.accounts[0].email =
          session.identity.username + "@example.test";
        for (const item of data.configuration.value.endpoints)
          item.endpoint.tenant_id = session.identity.tenant_id;
        return route.fulfill({ json: { ...data, session } });
      }
      if (path === "/api/accounts/login") {
        pending = true;
        await held;
        await route.fulfill({
          json: {
            result: "login",
            value: {
              id: "old-login",
              url: "https://auth.openai.com/oauth/authorize?state=synthetic-old",
              expires_at: 2000000000,
            },
          },
        });
        delivered = true;
        return;
      }
      if (path === "/api/accounts/check") {
        const csrf = route.request().headers()["x-csrf-token"];
        const status = csrf === session.csrf ? 200 : 403;
        mutation = { cookie: cookie!, csrf, status };
        return route.fulfill({
          status,
          json:
            status === 200
              ? { result: "done" }
              : { error: "request_origin_or_csrf_rejected" },
        });
      }
      return route.fulfill({ status: 400, json: { error: "invalid_request" } });
    });
    const first = await context.newPage(),
      second = await context.newPage();
    await first.goto(origin + "/#/accounts");
    await second.goto(origin + "/#/accounts");
    await expect(first.locator(".account-row h2")).toHaveText(
      "alice@example.test",
    );
    // This result belongs to A even if it arrives after another tab replaces the cookie.
    await first.getByRole("button", { name: "添加账号", exact: true }).click();
    await expect.poll(() => pending).toBe(true);
    await second.getByRole("button", { name: "退出登录", exact: true }).click();
    await second
      .getByLabel("用户名", { exact: true })
      .fill(replacement.identity.username);
    await second.getByLabel("密码", { exact: true }).fill("synthetic-password");
    await second.getByRole("button", { name: "登录", exact: true }).click();
    await expect(second.locator("header .user-menu span")).toHaveText(
      replacement.identity.username,
    );
    // Reconnection/fallback fetches now use the real shared B cookie.
    await first.evaluate(() => (window as any).reviewSessionSource.onerror());
    await expect(first.locator("header .user-menu span")).toHaveText(
      replacement.identity.username,
    );
    await expect(
      first.getByRole("button", { name: "退出登录", exact: true }),
    ).toBeEnabled();
    await first.getByRole("button", { name: "远程控制", exact: true }).click();
    await first
      .getByRole("button", { name: "新建控制端", exact: true })
      .click();
    await expect(
      first.locator('select[name="account"] option').last(),
    ).toHaveText(replacement.identity.username + "@example.test");
    release();
    await expect.poll(() => delivered).toBe(true);
    await expect(first.getByRole("dialog").getByRole("heading")).toHaveText(
      "新建控制端",
    );
    await first
      .getByRole("button", { name: "关闭对话框", exact: true })
      .click();
    await first.getByRole("button", { name: "账号", exact: true }).click();
    await expect(first.locator(".account-row h2")).toHaveText(
      replacement.identity.username + "@example.test",
    );
    await first.getByRole("button", { name: "检查登录", exact: true }).click();
    await expect
      .poll(() => mutation)
      .toEqual({ cookie: "B", csrf: "synthetic-csrf-B", status: 200 });
  });
}
