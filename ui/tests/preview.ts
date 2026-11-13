// Local-only visual QA fixture. Never used by the production app or server.
import { createMockApi } from "./mock-api";
const mock = createMockApi();
Bun.serve({
  hostname: "127.0.0.1",
  port: 5176,
  async fetch(request) {
    const path = new URL(request.url).pathname;
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
      new URL(`../dist${path === "/" ? "/index.html" : path}`, import.meta.url),
    );
    if (!(await file.exists())) return new Response(null, { status: 404 });
    return new Response(file);
  },
});
console.log(
  "Synthetic UI preview: http://127.0.0.1:5176 (demo / synthetic-password)",
);
