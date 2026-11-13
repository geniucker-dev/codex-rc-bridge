import { test, expect } from "@playwright/test";
import { Database } from "bun:sqlite";
import { mkdtemp, mkdir, rm, rename } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { prepareAcceptance, startAcceptance } from "../../scripts/acceptance";

test("acceptance launcher isolates state, refuses conflicts and preserves login across a joined shutdown", async ({
  page,
}) => {
  const root = resolve(import.meta.dir, "../..");
  const temp = await mkdtemp(join(tmpdir(), "bridge-acceptance-test-"));
  let instance: Awaited<ReturnType<typeof startAcceptance>> | undefined;
  try {
    await mkdir(join(temp, "local-state"), { mode: 0o700 });
    await mkdir(join(temp, "secrets"), { mode: 0o700 });
    const dailyDb = join(temp, "local-state/bridge.sqlite");
    const dailyKey = join(temp, "secrets/master.key");
    await Bun.write(dailyDb, "unrelated-daily-database");
    await Bun.write(dailyKey, "unrelated-daily-key");
    const admin = join(root, "target/release/bridge-admin");
    const state = await prepareAcceptance(temp, admin);
    expect(state.hasUser).toBe(false);
    const keyHash = new Bun.CryptoHasher("sha256")
      .update(await Bun.file(state.key).bytes())
      .digest("hex");
    expect((await prepareAcceptance(temp, admin)).tenant).toBe(state.tenant);
    await rename(state.key, `${state.key}.held`);
    await expect(prepareAcceptance(temp, admin)).rejects.toThrow("不成对");
    expect(await Bun.file(state.key).exists()).toBe(false);
    await rename(`${state.key}.held`, state.key);
    const db = new Database(state.database, { readwrite: true, create: false });
    try {
      db.query(
        "INSERT INTO management_users(id,tenant_id,username,password_hash,deployment_admin) VALUES(?,?,?,?,1)",
      ).run(
        crypto.randomUUID(),
        state.tenant,
        "acceptance",
        await Bun.password.hash("synthetic-acceptance-password", {
          algorithm: "argon2id",
        }),
      );
    } finally {
      db.close();
    }
    expect((await prepareAcceptance(temp, admin)).hasUser).toBe(true);
    const probes = [0, 1].map(() =>
      Bun.serve({
        hostname: "127.0.0.1",
        port: 0,
        fetch: () => new Response(),
      }),
    );
    const [backendPort, webPort] = probes.map((probe) => probe.port!);
    await Promise.all(probes.map((probe) => probe.stop(true)));
    const options = { root, paths: state, backendPort, webPort };
    instance = await startAcceptance(options);
    await expect(startAcceptance(options)).rejects.toThrow();
    await page.goto(instance.origin);
    await page.getByLabel("用户名", { exact: true }).fill("acceptance");
    await page
      .getByLabel("密码", { exact: true })
      .fill("synthetic-acceptance-password");
    await page.getByRole("button", { name: "登录", exact: true }).click();
    await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "设置", exact: true }),
    ).toBeVisible();
    await page.getByLabel("连接方式").selectOption("proxy");
    await page
      .getByLabel("代理地址")
      .fill("http://demo:synthetic-proxy-password@127.0.0.1:9");
    await page.getByRole("button", { name: "保存并应用", exact: true }).click();
    await expect(
      page.getByText("当前生效：http://127.0.0.1:9/（已配置代理认证）"),
    ).toBeVisible();
    await expect(page.getByLabel("代理地址")).toHaveValue("");
    await instance.stop();
    expect(await instance.done).toBe(0);
    instance = await startAcceptance(options);
    await page.reload();
    await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible();
    await expect(
      page.getByRole("button", { name: "退出登录", exact: true }),
    ).toBeVisible();
    await page.getByRole("button", { name: "设置", exact: true }).click();
    await expect(
      page.getByText("当前生效：http://127.0.0.1:9/（已配置代理认证）"),
    ).toBeVisible();
    await expect(page.getByLabel("代理地址")).toHaveValue("");
    await page.getByLabel("连接方式").selectOption("direct");
    await page.getByRole("button", { name: "保存并应用", exact: true }).click();
    await expect(page.getByText("当前生效：直连")).toBeVisible();
    await instance.stop();
    expect(await instance.done).toBe(0);
    expect(
      new Bun.CryptoHasher("sha256")
        .update(await Bun.file(state.key).bytes())
        .digest("hex"),
    ).toBe(keyHash);
    expect(await Bun.file(dailyDb).text()).toBe("unrelated-daily-database");
    expect(await Bun.file(dailyKey).text()).toBe("unrelated-daily-key");
  } finally {
    await instance?.stop();
    await rm(temp, { recursive: true, force: true });
  }
});
