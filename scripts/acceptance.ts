// Foreground, local-only acceptance workspace. Never opens the normal bridge database.
import { lstat, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { Database } from "bun:sqlite";

const workspace = resolve(import.meta.dir, "..");
export function acceptancePaths(root: string) {
  return {
    database: join(root, "local-state/acceptance-v1/bridge.sqlite"),
    key: join(root, "secrets/acceptance-v1/master.key"),
  };
}
type Paths = ReturnType<typeof acceptancePaths>;
async function exists(path: string) {
  try {
    return await lstat(path);
  } catch (error) {
    if ((error as { code?: string }).code === "ENOENT") return null;
    throw error;
  }
}
async function run(command: string[]) {
  const child = Bun.spawn(command, {
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });
  if ((await child.exited) !== 0)
    throw new Error("命令未完成，请根据上方提示处理后重试。");
}
export async function prepareAcceptance(root: string, admin: string) {
  for (const relative of [
    "local-state",
    "secrets",
    "local-state/acceptance-v1",
    "secrets/acceptance-v1",
  ]) {
    const path = join(root, relative);
    if (!(await exists(path))) await mkdir(path, { mode: 0o700 });
    const info = await lstat(path);
    if (!info.isDirectory() || info.isSymbolicLink())
      throw new Error("验收目录必须是独立普通目录，不能使用符号链接。");
    if (relative.endsWith("acceptance-v1") && (info.mode & 0o077) !== 0)
      throw new Error("验收目录须为 0700；请先检查权限。");
  }
  const paths = acceptancePaths(root);
  const database = await exists(paths.database),
    key = await exists(paths.key);
  if (!!database !== !!key)
    throw new Error("验收数据库与密钥不成对；保留现场，不重新初始化。");
  if (
    [database, key].some(
      (info) => info && (!info.isFile() || info.isSymbolicLink()),
    )
  )
    throw new Error("验收状态必须使用普通文件。");
  if (!database) await run([admin, paths.database, paths.key, "init"]);
  const db = new Database(paths.database, { readonly: true, create: false });
  try {
    const tenants = db.query("SELECT id FROM tenants").all() as {
      id: string;
    }[];
    if (tenants.length !== 1)
      throw new Error("独立验收库应只有一个租户，请检查目录。");
    const tenant = tenants[0]!.id;
    const user = db
      .query("SELECT id FROM management_users WHERE tenant_id=? AND username=?")
      .get(tenant, "acceptance");
    return { ...paths, tenant, hasUser: !!user };
  } finally {
    db.close();
  }
}

export async function startAcceptance(options: {
  root: string;
  paths: Paths;
  backendPort?: number;
  webPort?: number;
}) {
  const { root, paths, backendPort = 18787, webPort = 15173 } = options;
  if (backendPort === webPort) throw new Error("前后端必须使用不同端口。");
  // Detect occupied ports before launching; never stop whichever process owns them.
  for (const port of [backendPort, webPort]) {
    const probe = Bun.listen({
      hostname: "127.0.0.1",
      port,
      socket: { data() {} },
    });
    probe.stop(true);
  }
  const origin = `http://127.0.0.1:${webPort}`;
  const backend = `http://127.0.0.1:${backendPort}`;
  const env = {
    ...process.env,
    BRIDGE_DATABASE: paths.database,
    BRIDGE_MASTER_KEY: paths.key,
    BRIDGE_BIND: `127.0.0.1:${backendPort}`,
    BRIDGE_PUBLIC_ORIGIN: origin,
  };
  const children: ReturnType<typeof Bun.spawn>[] = [];
  let closing: Promise<void> | undefined;
  const stop = () =>
    (closing ??= (async () => {
      for (const child of [...children].reverse())
        if (child.exitCode === null) child.kill("SIGTERM");
      // Do not force-kill accepted authorization/revocation work.
      await Promise.all(children.map((child) => child.exited));
    })());
  const signals = ["SIGINT", "SIGTERM"] as const;
  const onSignal = () => {
    void stop();
  };
  for (const signal of signals) process.on(signal, onSignal);
  const removeSignals = () => {
    for (const signal of signals) process.off(signal, onSignal);
  };
  async function ready(url: string, host?: string) {
    let observed = "连接未建立";
    for (let attempt = 0; attempt < 100; attempt++) {
      if (closing || children.some((child) => child.exitCode !== null))
        throw new Error("验收服务在就绪前退出。");
      try {
        const response = await fetch(url, {
          headers: host ? { host } : {},
          proxy: null,
          signal: AbortSignal.timeout(1000),
        });
        observed = `HTTP ${response.status}`;
        if (response.ok && (await response.json()).management === true) return;
      } catch {
        /* Startup is asynchronous; only bounded loopback reads are retried. */
      }
      await Bun.sleep(100);
    }
    throw new Error(
      `验收服务未就绪（${url}，${observed}），请检查端口和上方错误。`,
    );
  }
  try {
    children.push(
      Bun.spawn([join(root, "target/release/bridge-server")], {
        env,
        stdin: "ignore",
        stdout: "inherit",
        stderr: "inherit",
      }),
    );
    await ready(`${backend}/api/capabilities`, `127.0.0.1:${webPort}`);
    if (closing) throw new Error("验收启动已取消。");
    children.push(
      Bun.spawn(
        [
          process.execPath,
          "--bun",
          join(root, "ui/node_modules/vite/bin/vite.js"),
          "--host",
          "127.0.0.1",
          "--port",
          String(webPort),
          "--strictPort",
        ],
        {
          cwd: join(root, "ui"),
          env: { ...env, BRIDGE_DEV_BACKEND: backend },
          stdin: "ignore",
          stdout: "inherit",
          stderr: "inherit",
        },
      ),
    );
    await ready(`${origin}/api/capabilities`);
    const page = await fetch(origin, {
      proxy: null,
      signal: AbortSignal.timeout(3000),
    });
    if (!page.ok || !page.headers.get("content-type")?.includes("text/html"))
      throw new Error("管理页面未就绪。");
    const done = Promise.race(children.map((child) => child.exited)).then(
      async (code) => {
        const intentional = !!closing;
        await stop();
        removeSignals();
        return intentional ? 0 : code || 1;
      },
    );
    return { origin, stop, done };
  } catch (error) {
    await stop();
    removeSignals();
    throw error;
  }
}

async function main() {
  if (process.argv.slice(2).some((arg) => arg !== "--prepare"))
    throw new Error("用法：bun run acceptance [--prepare]");
  await run([
    "cargo",
    "build",
    "--release",
    "--locked",
    "-p",
    "bridge-server",
    "--bins",
  ]);
  const admin = join(workspace, "target/release/bridge-admin");
  const state = await prepareAcceptance(workspace, admin);
  if (process.argv.includes("--prepare")) {
    console.log(
      "独立验收目录已准备。运行 bun run acceptance 创建管理密码并启动；目前未启动网页或桥接。",
    );
    return;
  }
  if (!state.hasUser) {
    if (!process.stdin.isTTY)
      throw new Error(
        "请在你自己的终端运行 bun run acceptance，以隐藏输入管理密码。",
      );
    console.log(
      "为独立验收实例设置管理密码，用户名固定为 acceptance。此密码与 ChatGPT 账号无关。",
    );
    await run([
      admin,
      state.database,
      state.key,
      "add-user",
      state.tenant,
      "acceptance",
    ]);
  }
  const instance = await startAcceptance({ root: workspace, paths: state });
  console.log(
    `验收管理入口已就绪：${instance.origin}\n用户名：acceptance\n保持本终端运行；Ctrl+C 仅停止此验收实例，保留状态。手机官方客户端通过云端 IN 连接，不访问本机回环地址。`,
  );
  if ((await instance.done) !== 0)
    throw new Error("验收服务意外退出，请检查上方错误；状态已保留。");
}
if (import.meta.main)
  await main().catch((error) => {
    console.error((error as Error).message);
    process.exitCode = 1;
  });
