import { test, expect } from "@playwright/test";
import { resolve } from "node:path";

// Cleanup runs in its own hook so a test that exhausts its time still removes what it created.
let cleanup: (() => Promise<void>) | null = null;
test.afterAll(async () => {
  await cleanup?.();
  cleanup = null;
});

// One unique Compose project per run; cleanup never targets an existing deployment.
test("Linux images serve the real UI/API, restore separate volumes and stop cleanly", async ({
  page,
  request,
}) => {
  const root = resolve(import.meta.dir, "../..");
  const project = `bridge-test-${crypto.randomUUID().slice(0, 8)}`;
  const backupVolumes = [`${project}-backup-data`, `${project}-backup-key`];
  const createdBackups: string[] = [];
  const probe = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch: () => new Response(),
  });
  const port = probe.port!;
  probe.stop(true);
  const origin = `http://127.0.0.1:${port}`;
  const env = {
    ...process.env,
    BRIDGE_PUBLIC_ORIGIN: origin,
    BRIDGE_HTTP_BIND: "127.0.0.1",
    BRIDGE_HTTP_PORT: String(port),
    // The image built from this checkout, never the published one.
    BRIDGE_IMAGE: "codex-rc-bridge-server:local",
  };
  delete env.BRIDGE_UPSTREAM_PROXY;
  async function docker(args: string[], expected = 0) {
    const child = Bun.spawn(["docker", ...args], {
      cwd: root,
      env,
      stdout: "pipe",
      stderr: "pipe",
    });
    const timer = setTimeout(() => child.kill(), 60_000);
    try {
      const [stdout, stderr, status] = await Promise.all([
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
        child.exited,
      ]);
      if (expected === 0 && status !== 0)
        throw new Error(`Docker command failed (${args[0]}): ${stderr}`);
      if (expected !== 0 && status === 0)
        throw new Error("Expected the isolated operation to be refused");
      return stdout;
    } finally {
      clearTimeout(timer);
    }
  }
  const composeArgs = [
    "compose",
    "--env-file",
    "/dev/null",
    "-f",
    resolve(root, "compose.yaml"),
    "-p",
    project,
  ];
  const compose = (args: string[], expected = 0) =>
    docker([...composeArgs, ...args], expected);
  cleanup = async () => {
    try {
      await compose([
        "down",
        "--volumes",
        "--remove-orphans",
        "--timeout",
        "30",
      ]);
    } finally {
      if (createdBackups.length)
        await docker(["volume", "rm", ...createdBackups]);
    }
  };
  const admin = (args: string[], expected = 0) =>
    compose(["run", "--rm", "--no-deps", "-T", "admin", ...args], expected);
  const shell = (command: string) =>
    compose([
      "run",
      "--rm",
      "--no-deps",
      "-T",
      "--entrypoint",
      "sh",
      "admin",
      "-c",
      command,
    ]);
  // Offline helpers only see this test's volumes. Archives stay separate and private.
  const archive = (source: string, backup: string, restore: boolean) =>
    docker([
      "run",
      "--rm",
      "--network",
      "none",
      "--read-only",
      "--user",
      "0:0",
      "--mount",
      `type=volume,source=${source},target=/state${restore ? "" : ",readonly"}`,
      "--mount",
      `type=volume,source=${backup},target=/backup${restore ? ",readonly" : ""}`,
      "--entrypoint",
      "sh",
      "codex-rc-bridge-server:local",
      "-ec",
      restore
        ? 'test -z "$(ls -A /state)"; tar xpf /backup/state.tar -C /state'
        : 'umask 077; chmod 0700 /backup; tar cpf /backup/state.tar -C /state .; test "$(stat -c %a /backup/state.tar)" = 600',
    ]);
  async function observeEvents() {
    await page.evaluate(() => {
      const source = new EventSource("/api/events");
      const records: { event: string; data: string }[] = [];
      (window as any).__bridgeEventProbe = { source, records };
      for (const event of [
        "changed",
        "heartbeat",
        "session_expired",
        "unavailable",
      ]) {
        source.addEventListener(event, (message) => {
          records.push({ event, data: (message as MessageEvent).data });
          if (event === "session_expired") source.close();
        });
      }
    });
    await expect.poll(() => eventCount("changed")).toBeGreaterThan(0);
  }
  async function eventCount(event: string) {
    return page.evaluate(
      (event) =>
        (window as any).__bridgeEventProbe.records.filter(
          (record: { event: string }) => record.event === event,
        ).length,
      event,
    );
  }
  try {
    const initialized = await admin(["init"]);
    const tenant = initialized.match(/Tenant：([0-9a-f-]{36})/)?.[1];
    expect(tenant).toBeTruthy();
    const keyBefore = await shell("sha256sum /run/bridge-secrets/master.key");
    expect(
      (await shell("stat -c '%u:%g:%a' /run/bridge-secrets/master.key")).trim(),
    ).toBe("10001:10001:600");
    await admin(["init"], 1);
    expect(await shell("sha256sum /run/bridge-secrets/master.key")).toBe(
      keyBefore,
    );
    await docker([
      "run",
      "--rm",
      "--network",
      "none",
      "--user",
      "10001:10001",
      "-v",
      `${project}_bridge-data:/data`,
      "-v",
      `${resolve(root, "ui/container-tests/bootstrap.ts")}:/fixture.ts:ro`,
      "oven/bun:1.3.11",
      "bun",
      "/fixture.ts",
      tenant!,
    ]);
    await compose(["up", "-d", "--no-build", "server"]);
    await expect
      .poll(
        async () => {
          try {
            return (await request.get(`${origin}/api/capabilities`)).status();
          } catch {
            return 0;
          }
        },
        { timeout: 30_000 },
      )
      .toBe(200);
    const serverId = (await compose(["ps", "-q", "server"])).trim();
    const info = JSON.parse(await docker(["inspect", serverId]))[0];
    expect(info.Config.User).toBe("10001:10001");
    expect(info.HostConfig.ReadonlyRootfs).toBe(true);
    // Only the loopback port meant for the host's reverse proxy is published.
    expect(
      Object.values(info.NetworkSettings.Ports ?? {})
        .flat()
        .every((value: any) => value === null || value.HostIp === "127.0.0.1"),
    ).toBe(true);
    expect(
      info.Mounts.find(
        (m: { Destination: string }) => m.Destination === "/run/bridge-secrets",
      ).RW,
    ).toBe(false);
    const api = await request.get(`${origin}/api/capabilities`);
    expect(await api.json()).toMatchObject({
      management: true,
      remoteControl: true,
    });
    expect((await request.get(`${origin}/api/state`)).status()).toBe(401);
    expect(
      (
        await request.post(`${origin}/api/session`, {
          headers: { Origin: "https://untrusted.example" },
          data: {},
        })
      ).status(),
    ).toBe(403);
    expect(
      (
        await request.get(`${origin}/api/capabilities`, {
          headers: { Host: "127.0.0.1:1" },
        })
      ).status(),
    ).toBe(403);
    const html = await page.goto(origin);
    expect(html?.headers()["content-security-policy"]).toContain(
      "frame-ancestors 'none'",
    );
    await page.getByLabel("用户名", { exact: true }).fill("container-demo");
    await page
      .getByLabel("密码", { exact: true })
      .fill("container-synthetic-password");
    await page.getByRole("button", { name: "登录", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "远程控制", exact: true }),
    ).toBeVisible();
    await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible();
    await observeEvents();
    await expect
      .poll(() => eventCount("heartbeat"), { timeout: 12_000 })
      .toBeGreaterThan(0);
    // Exercise a local-only mutation. No OAuth code is exchanged.
    const managementSession = await (
      await page.request.get(`${origin}/api/session`)
    ).json();
    const before = await eventCount("changed");
    const refreshed = page.waitForResponse(
      (response) =>
        response.url() === `${origin}/api/state` && response.status() === 200,
    );
    const started = await page.request.post(`${origin}/api/accounts/login`, {
      headers: { Origin: origin, "x-csrf-token": managementSession.csrf },
      data: {},
    });
    expect(started.status()).toBe(200);
    const loginId = (await started.json()).value.id;
    await expect.poll(() => eventCount("changed")).toBeGreaterThan(before);
    await refreshed;
    expect(
      (
        await page.request.delete(`${origin}/api/logins/${loginId}`, {
          headers: { Origin: origin, "x-csrf-token": managementSession.csrf },
        })
      ).status(),
    ).toBe(200);
    const state = await page.request.get(`${origin}/api/state`);
    expect(state.status()).toBe(200);
    expect((await state.json()).configuration.value.endpoints).toEqual([]);
    const cookies = await page.context().cookies();
    expect(cookies.some((c) => c.httpOnly && c.sameSite === "Strict")).toBe(
      true,
    );
    await admin(["new-tenant"], 1); // Server owns the SQLite lock.
    await compose(["stop", "-t", "-1", "server"]);
    const stopped = JSON.parse(await docker(["inspect", serverId]))[0];
    expect(stopped.State.ExitCode).toBe(0);
    expect(stopped.State.OOMKilled).toBe(false);
    expect(await shell("sha256sum /run/bridge-secrets/master.key")).toBe(
      keyBefore,
    );
    // The original database/seed can still decrypt with the original key, and the lock is released.
    expect(await admin(["new-tenant"])).toContain("Tenant：");
    const databaseBefore = await shell(
      "sha256sum /var/lib/bridge/bridge.sqlite",
    );
    for (const volume of backupVolumes) {
      await docker(["volume", "create", volume]);
      createdBackups.push(volume);
    }
    await archive(`${project}_bridge-data`, backupVolumes[0]!, false);
    await archive(`${project}_bridge-key`, backupVolumes[1]!, false);
    // Remove the source volumes, then restore into newly initialized empty volumes.
    await compose(["down", "--remove-orphans", "--timeout", "30"]);
    await docker([
      "volume",
      "rm",
      `${project}_bridge-data`,
      `${project}_bridge-key`,
    ]);
    await compose(["create", "--no-build", "server"]);
    await archive(`${project}_bridge-data`, backupVolumes[0]!, true);
    await archive(`${project}_bridge-key`, backupVolumes[1]!, true);
    expect(await shell("sha256sum /var/lib/bridge/bridge.sqlite")).toBe(
      databaseBefore,
    );
    expect(await shell("sha256sum /run/bridge-secrets/master.key")).toBe(
      keyBefore,
    );
    expect(
      (await shell("stat -c '%u:%g:%a' /run/bridge-secrets/master.key")).trim(),
    ).toBe("10001:10001:600");
    await compose(["up", "-d", "--no-build", "server"]);
    await expect
      .poll(
        async () => {
          try {
            return (await page.request.get(`${origin}/api/state`)).status();
          } catch {
            return 0;
          }
        },
        { timeout: 30_000 },
      )
      .toBe(200);
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "远程控制", exact: true }),
    ).toBeVisible();
    await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    // A permission regression must fail closed, never recreate or silently accept a public key file.
    await compose(["stop", "-t", "30", "server"]);
    await shell("chmod 0644 /run/bridge-secrets/master.key");
    await compose(["run", "--rm", "--no-deps", "-T", "server"], 1);
    await shell("chmod 0600 /run/bridge-secrets/master.key");
    expect(await shell("sha256sum /run/bridge-secrets/master.key")).toBe(
      keyBefore,
    );
    // With the key private again, the server starts and a fresh session works end to end.
    await compose(["up", "-d", "--no-build", "server"]);
    await expect
      .poll(
        async () => {
          try {
            return (await request.get(`${origin}/api/capabilities`)).status();
          } catch {
            return 0;
          }
        },
        { timeout: 30_000 },
      )
      .toBe(200);
    await page.context().clearCookies();
    await page.goto(origin);
    await page.getByLabel("用户名", { exact: true }).fill("container-demo");
    await page
      .getByLabel("密码", { exact: true })
      .fill("container-synthetic-password");
    await page.getByRole("button", { name: "登录", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "远程控制", exact: true }),
    ).toBeVisible();
    await expect(page.getByText("状态实时更新", { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    await observeEvents();
    await expect
      .poll(() => eventCount("heartbeat"), { timeout: 12_000 })
      .toBeGreaterThan(0);
    // TLS belongs to the host's reverse proxy; the Secure flag is covered by the API tests.
    expect(
      (await page.context().cookies()).some(
        (c) => c.httpOnly && c.sameSite === "Strict",
      ),
    ).toBe(true);
    expect(
      (
        await page.request.delete(`${origin}/api/session`, {
          headers: { Origin: origin },
        })
      ).status(),
    ).toBe(403);
    await page.getByRole("button", { name: "退出登录", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "登录", exact: true }),
    ).toBeVisible();
    expect((await page.request.get(`${origin}/api/state`)).status()).toBe(401);
    await expect
      .poll(() => eventCount("session_expired"), { timeout: 12_000 })
      .toBe(1);
    const payloads = await page.evaluate(() =>
      (window as any).__bridgeEventProbe.records.map(
        (record: { data: string }) => record.data,
      ),
    );
    expect(payloads.every((data: string) => data === "{}")).toBe(true);
    await page.evaluate(() =>
      (window as any).__bridgeEventProbe.source.close(),
    );
  } catch (error) {
    // Test-owned containers only; keep request headers and bodies out of diagnostics.
    const logs = await compose([
      "logs",
      "--no-color",
      "--no-log-prefix",
      "--tail",
      "20",
      "server",
    ]);
    const messages = logs
      .split("\n")
      .map((line) => {
        try {
          const value = JSON.parse(line);
          return String(value.msg ?? "") + " " + String(value.error ?? "");
        } catch {
          return line;
        }
      })
      .join("\n");
    console.error(`Container diagnostics:\n${messages}`);
    throw error;
  }
});
