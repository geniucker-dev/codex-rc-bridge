// Synthetic UI fixture only. Real authorization/tenant boundaries are exercised by Rust API tests.
import type {
  Snapshot,
  EndpointInfo,
  Account,
  AccountInfo,
  Chain,
  PairedDevice,
} from "../src/api";
export function createMockApi() {
  const account: AccountInfo = {
    user_id: "demo-user",
    workspace_id: "demo-workspace",
    sign_in_required: false,
  };
  const make = (
    id: string,
    name: string,
    kind: "in" | "out",
  ): EndpointInfo => ({
    endpoint: {
      device_id: id,
      tenant_id: "demo-tenant",
      account: { user_id: account.user_id, workspace_id: account.workspace_id },
      kind,
      identity_name: name,
      display_name: name,
    },
    lifecycle: "active",
    registered: true,
    target:
      kind === "out"
        ? { environment_id: `computer-${id}`, app_server_version: "demo/1" }
        : null,
    target_name:
      kind === "out"
        ? ({ "out-one": "Studio MacBook Pro", "out-three": "Lab Mac mini" }[
            id
          ] ?? null)
        : null,
    registration_uncertain: false,
    registration_recoverable: false,
    paired_device: kind === "in" ? (devices[id] ?? null) : undefined,
    target_online: kind === "out" ? id !== "out-three" : null,
    target_last_seen_at: kind === "out" ? 1794139200 : null,
  });
  const devices: Record<string, PairedDevice> = {
    "in-one": {
      client_id: "ipad",
      name: "我的 iPad",
      platform: "iOS",
      device_model: "iPad",
      device_type: "tablet",
      last_seen_at: 1794139200,
    },
    "in-two": {
      client_id: "pixel",
      name: "Pixel 9",
      platform: "Android",
      device_model: "Pixel 9",
    },
  };
  const data: Snapshot = {
    configuration: {
      result: "snapshot",
      value: {
        accounts: [account],
        endpoints: [
          make("in-one", "我的手机入口", "in"),
          make("out-one", "工作电脑出口", "out"),
          make("out-two", "个人电脑出口", "out"),
          make("in-two", "另一手机入口", "in"),
          make("out-three", "已占用出口", "out"),
        ],
      },
    },
    chains: [
      {
        binding: { in_id: "in-one", out_id: "out-one" },
        enabled: false,
        status: "stopped",
        connection: "stopped",
        side: null,
      },
      {
        binding: { in_id: "in-two", out_id: "out-three" },
        enabled: false,
        status: "stopped",
        connection: "stopped",
        side: null,
      },
    ],
  };
  let loggedIn = false,
    count = 0;
  let loginOutput: string | undefined;
  let loginExpected: Account | undefined;
  const same = (a: Account, b: Account) =>
    a.user_id === b.user_id && a.workspace_id === b.workspace_id;
  const endpointById = (id: string) =>
    data.configuration.value.endpoints.find((i) => i.endpoint.device_id === id);
  // A fix resumes every enabled connection that stopped for that reason.
  const resume = (stopped: (chain: Chain) => boolean) => {
    for (const chain of data.chains)
      if (chain.enabled && chain.status === "failed" && stopped(chain)) {
        chain.status = "running";
        chain.connection = "connecting";
        chain.side = null;
      }
  };
  const calls: { path: string; method: string; body: any }[] = [];
  // Deployment users as GET /api/users returns them to an administrator.
  const users = [
    {
      id: "user-admin",
      username: "demo",
      deployment_admin: true,
      locked: false,
      current: true,
    },
    {
      id: "user-alice",
      username: "alice",
      deployment_admin: false,
      locked: true,
      current: false,
    },
  ];
  const session = {
    identity: {
      deployment_admin: false,
      username: "demo",
      tenant_id: "demo-tenant",
      expires_at: 2000000000,
    },
    csrf: "synthetic-csrf",
  };
  const result = (value: unknown, status = 200) => ({
    status,
    body: JSON.stringify(value),
    contentType: "application/json",
  });
  async function handle(path: string, method: string, body: any = {}) {
    calls.push({ path, method, body });
    if (path === "/api/capabilities")
      return result({
        version: "0.1.0",
        management: true,
        remoteControl: true,
      });
    if (path === "/api/session") {
      if (method === "POST") {
        if (body.password !== "synthetic-password")
          return result({ error: "login_required" }, 401);
        loggedIn = true;
        return result(session);
      }
      if (!loggedIn) return result({ error: "login_required" }, 401);
      if (method === "DELETE") {
        loggedIn = false;
        return result(null, 204);
      }
      return result(session);
    }
    if (!loggedIn) return result({ error: "login_required" }, 401);
    if (path === "/api/session/password" && method === "POST") {
      if (body.current !== "synthetic-password")
        return result({ error: "current_password_incorrect" }, 400);
      if (String(body.password).length < 12)
        return result({ error: "invalid_password" }, 400);
      return result(null, 204);
    }
    if (path === "/api/users" || path.startsWith("/api/users/")) {
      if (!session.identity.deployment_admin)
        return result({ error: "deployment_admin_required" }, 403);
      const target = users.find((u) => u.id === path.split("/")[3]);
      if (path === "/api/users" && method === "GET") return result({ users });
      if (path === "/api/users" && method === "POST") {
        if (!/^[a-z0-9_.-]{1,64}$/.test(body.username))
          return result({ error: "invalid_username" }, 400);
        if (users.some((u) => u.username === body.username))
          return result({ error: "username_exists" }, 409);
        const user = {
          id: `user-${++count}`,
          username: body.username,
          deployment_admin: false,
          locked: false,
          current: false,
        };
        users.push(user);
        return result(user, 201);
      }
      if (!target) return result({ error: "user_not_found" }, 404);
      if (target.deployment_admin)
        return result({ error: "target_is_admin" }, 409);
      if (path.endsWith("/password") && method === "POST") {
        target.locked = false;
        return result(null, 204);
      }
      if (method === "DELETE") {
        users.splice(users.indexOf(target), 1);
        return result(null, 204);
      }
    }
    if (path === "/api/state") return result({ ...data, session });
    const id = path.split("/")[3];
    const ep = data.configuration.value.endpoints.find(
      (i) => i.endpoint.device_id === id,
    );
    // The backend throttles and compares with the service; the fixture has nothing to update.
    if (path.endsWith("/device/sync") && method === "POST")
      return result(null, 204);
    if (path === "/api/accounts/login" || path.endsWith("/authorize")) {
      loginOutput = ep?.endpoint.device_id;
      loginExpected = loginOutput ? undefined : body?.expected;
      return result({
        result: "login",
        value: {
          id: "login-demo",
          url: "https://auth.openai.com/oauth/authorize?state=synthetic",
          expires_at: 2000000000,
        },
      });
    }
    if (path.startsWith("/api/logins/")) {
      if (method === "POST" && loginOutput) {
        const output = data.configuration.value.endpoints.find(
          (i) => i.endpoint.device_id === loginOutput,
        )!;
        output.registered = true;
        output.lifecycle = "active";
        resume(
          (chain) =>
            chain.binding.out_id === loginOutput &&
            chain.connection === "device_authorization_required",
        );
      }
      const expected = loginExpected;
      if (method === "POST" && expected) {
        for (const a of data.configuration.value.accounts)
          if (same(a, expected)) a.sign_in_required = false;
        resume((chain) => {
          if (chain.connection !== "sign_in_required" || !chain.side)
            return false;
          const end = endpointById(
            chain.side === "controller"
              ? chain.binding.in_id
              : chain.binding.out_id,
          );
          return !!end && same(end.endpoint.account, expected);
        });
      }
      return result({ result: "done" });
    }
    if (
      path.endsWith("/restore-record") &&
      ep &&
      ep.registration_uncertain &&
      !ep.registration_recoverable
    ) {
      if (body.client_id !== "original-controller")
        return result({ error: "out_record_identity_mismatch" }, 409);
      ep.registration_recoverable = true;
      return result({ result: "done" });
    }
    if (
      path.endsWith("/recover") &&
      ep &&
      ep.registration_recoverable &&
      ep.lifecycle !== "deleting"
    ) {
      ep.lifecycle = "active";
      ep.registered = true;
      ep.registration_uncertain = false;
      ep.registration_recoverable = false;
      return result({ result: "done" });
    }
    if (path === "/api/accounts/check") {
      const checked = data.configuration.value.accounts.find((a) =>
        same(a, body.account),
      );
      return checked?.sign_in_required
        ? result({ error: "account_sign_in_required" }, 409)
        : result({ result: "done" });
    }
    if (path === "/api/endpoints" && method === "POST") {
      const item = make(`created-${++count}`, body.name, body.kind);
      if (body.kind === "out") {
        item.registered = false;
        item.lifecycle = "draft";
        item.target = null;
      }
      data.configuration.value.endpoints.push(item);
      return result({ result: "endpoint", value: item.endpoint });
    }
    if (path.startsWith("/api/endpoints/") && method === "DELETE" && ep) {
      ep.lifecycle = "deleting";
      ep.registration_uncertain = true;
      data.chains = data.chains.filter(
        (c) => c.binding.in_id !== id && c.binding.out_id !== id,
      );
      return result({ status: "registration_unknown" });
    }
    if (path.endsWith("/pair")) {
      if (path.includes("/inputs/") && ep) {
        ep.registered = true;
        ep.lifecycle = "active";
        ep.registration_uncertain = false;
        // One device per controller: pairing removes the current one first.
        ep.paired_device = null;
        ep.pairing_pending = true;
        ep.pairing_error = null;
        resume(
          (chain) =>
            chain.binding.in_id === id &&
            chain.connection === "controller_registration_required",
        );
      }
      if (path.includes("/inputs/"))
        return result({
          result: "pairing",
          value: { code: "DEMO1234", expires_at: 2000000000 },
        });
      // An output keeps exactly one computer: pairing replaces the previous one.
      if (!ep) return result({ error: "invalid_request" }, 400);
      ep.target = {
        environment_id: "demo-computer",
        app_server_version: "demo/1",
      };
      ep.target_name = "演示电脑";
      ep.pairing_pending = false;
      return result({
        result: "paired_output",
        value: { environment_id: "demo-computer", name: "演示电脑" },
      });
    }
    const chain = data.chains.find((c) => c.binding.in_id === id);
    if (path.endsWith("/binding")) {
      if (method === "DELETE")
        data.chains = data.chains.filter((c) => c.binding.in_id !== id);
      else
        data.chains.push({
          binding: { in_id: id, out_id: body.output },
          enabled: false,
          status: "stopped",
          connection: "stopped",
          side: null,
        });
      return result({ result: "done" });
    }
    if (chain && path.endsWith("/start")) {
      chain.enabled = true;
      chain.status = "running";
      chain.connection = "connecting";
      chain.side = null;
      return result(null, 204);
    }
    if (chain && path.endsWith("/stop")) {
      chain.enabled = false;
      chain.status = "stopped";
      chain.connection = "stopped";
      chain.side = null;
      return result(null, 204);
    }
    if (chain && path.endsWith("/switch")) {
      chain.binding.out_id = body.output;
      return result(null, 204);
    }
    return result({ error: "invalid_request" }, 400);
  }
  return {
    handle,
    calls,
    data,
    session,
    users,
    // The device entering the code; the backend records it after pairing.
    pairDevice(id: string, device: PairedDevice) {
      const ep = endpointById(id);
      if (ep)
        Object.assign(ep, { paired_device: device, pairing_pending: false });
    },
    // More than one device used the code: the backend removes them all.
    pairingFailed(id: string, code: string) {
      const ep = endpointById(id);
      if (ep) Object.assign(ep, { paired_device: null, pairing_error: code });
    },
    expire() {
      loggedIn = false;
    },
  };
}
