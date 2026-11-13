// Real streaming HTTP fixture for EventSource. All data and sessions are synthetic.
import { serve } from "bun";
import type { createMockApi } from "./mock-api";

export function eventServer(mock: ReturnType<typeof createMockApi>) {
  const clients = new Set<ReadableStreamDefaultController<Uint8Array>>();
  const encoder = new TextEncoder();
  let offline = false;
  const emit = (event: string) => {
    for (const client of clients) {
      try {
        client.enqueue(encoder.encode(`event: ${event}\ndata: {}\n\n`));
      } catch {
        clients.delete(client);
      }
    }
  };
  const server = serve({
    hostname: "127.0.0.1",
    port: 0,
    idleTimeout: 0,
    fetch() {
      if (offline) return new Response(null, { status: 503 });
      let client: ReadableStreamDefaultController<Uint8Array>;
      return new Response(
        new ReadableStream<Uint8Array>({
          start(controller) {
            client = controller;
            clients.add(client);
            client.enqueue(encoder.encode("event: changed\ndata: {}\n\n"));
          },
          cancel() {
            clients.delete(client);
          },
        }),
        {
          headers: {
            "content-type": "text/event-stream",
            "cache-control": "no-store",
            "access-control-allow-origin": "http://127.0.0.1:5175",
          },
        },
      );
    },
  });
  const timer = setInterval(async () => {
    const reply = await mock.handle("/api/session", "GET");
    emit(reply.status === 401 ? "session_expired" : "heartbeat");
  }, 500);
  const disconnect = () => {
    offline = true;
    for (const client of clients) {
      try {
        client.close();
      } catch {}
    }
    clients.clear();
  };
  return {
    url: `http://127.0.0.1:${server.port}/api/events`,
    emit,
    disconnect,
    resume() {
      offline = false;
    },
    get clients() {
      return clients.size;
    },
    async close() {
      clearInterval(timer);
      disconnect();
      await server.stop(true);
    },
  };
}
