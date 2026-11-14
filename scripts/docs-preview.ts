// Synthetic documentation screenshots, using the real built UI and test API.
// Run `bun run build`, then `bun scripts/docs-preview.ts`; stop with Ctrl-C.
// In an isolated browser, set bridge.language in localStorage to en or zh.
// Capture /#/home at 1280 x 640. No account state or upstream is used.
import { createMockApi } from "../ui/tests/mock-api";

const mock = createMockApi();
const personal = {
  user_id: "docs-personal",
  workspace_id: "docs-personal-workspace",
  email: "personal@example.com",
  sign_in_required: false,
};
const work = {
  user_id: "docs-work",
  workspace_id: "docs-work-workspace",
  email: "work@example.com",
  sign_in_required: false,
};
mock.data.configuration.value.accounts = [personal, work];
const names: Record<string, string> = {
  "in-one": "iPad",
  "in-two": "ChatGPT Desktop",
  "out-one": "MacBook Pro",
  "out-three": "Mac mini",
};
mock.data.configuration.value.endpoints =
  mock.data.configuration.value.endpoints.filter((info) => {
    const name = names[info.endpoint.device_id];
    if (!name) return false;
    Object.assign(info.endpoint, {
      identity_name: name,
      display_name: name,
      account: info.endpoint.kind === "in" ? personal : work,
    });
    if (info.endpoint.kind === "out") info.target_name = name;
    return true;
  });
mock.pairDevice("in-one", {
  client_id: "docs-ipad",
  name: "iPad Air",
  platform: "iOS",
  device_model: "iPad Air",
  device_type: "tablet",
});
mock.pairDevice("in-two", {
  client_id: "docs-desktop",
  name: "MacBook Air",
  platform: "macOS",
  device_model: "MacBook Air",
  device_type: "desktop",
});
mock.data.chains[0] = {
  ...mock.data.chains[0]!,
  enabled: true,
  status: "running",
  connection: "connected",
};
// The preview starts signed in to a synthetic session and binds only loopback.
await mock.handle("/api/session", "POST", { password: "synthetic-password" });
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  idleTimeout: 0,
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/api/events") {
      let heartbeat: ReturnType<typeof setInterval>;
      return new Response(
        new ReadableStream({
          start(controller) {
            const emit = () =>
              controller.enqueue(
                new TextEncoder().encode("event: heartbeat\ndata: {}\n\n"),
              );
            emit();
            heartbeat = setInterval(emit, 5000);
          },
          cancel() {
            clearInterval(heartbeat);
          },
        }),
        {
          headers: {
            "content-type": "text/event-stream",
            "cache-control": "no-store",
          },
        },
      );
    }
    if (path.startsWith("/api/")) {
      const body =
        request.method === "POST" ? await request.json().catch(() => ({})) : {};
      const reply = await mock.handle(path, request.method, body);
      return new Response(reply.status === 204 ? null : reply.body, {
        status: reply.status,
        headers: {
          "content-type": reply.contentType,
          "cache-control": "no-store",
        },
      });
    }
    if (path !== "/" && !/^\/assets\/[A-Za-z0-9_.-]+$/.test(path))
      return new Response(null, { status: 404 });
    const file = Bun.file(
      new URL(
        `../ui/dist${path === "/" ? "/index.html" : path}`,
        import.meta.url,
      ),
    );
    if (!(await file.exists())) return new Response(null, { status: 404 });
    return new Response(file);
  },
});
console.log(
  `Synthetic documentation preview: http://127.0.0.1:${server.port}/#/home`,
);
