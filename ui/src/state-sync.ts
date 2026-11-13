import { t } from "./i18n";
import { desktop, listenDesktop } from "./desktop";
import {
  ApiError,
  request,
  type Snapshot,
  type ManagementSnapshot,
  type Session,
} from "./api";

// One subscription and one serialized snapshot reader per management session.
// Events invalidate state; reconnect always reads a full snapshot of the current binding.
export function syncState(
  session: Session,
  callbacks: {
    snapshot: (snapshot: Snapshot) => void;
    sessionChanged: (session: Session) => void;
    stale: () => void;
    expired: () => void;
    mode: (mode: "connecting" | "live" | "polling") => void;
  },
) {
  const abort = new AbortController();
  let closed = false;
  let source: EventSource | null = null;
  let unlisten: (() => void) | undefined;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let watchdog: ReturnType<typeof setTimeout> | undefined;
  let pending: Promise<void> | undefined;
  let readRetry: ReturnType<typeof setTimeout> | undefined;
  let dirty = false;
  const close = () => {
    closed = true;
    source?.close();
    unlisten?.();
    clearTimeout(retry);
    clearTimeout(watchdog);
    clearTimeout(readRetry);
    abort.abort();
  };
  const expired = () => {
    if (closed) return;
    close();
    callbacks.expired();
  };
  const refresh = (): Promise<void> => {
    if (closed)
      return Promise.reject(new ApiError(0, t("Status subscription closed.")));
    dirty = true;
    if (!pending) {
      pending = (async () => {
        while (dirty && !closed) {
          dirty = false;
          const snapshot = await request<ManagementSnapshot>(
            "/state",
            "GET",
            undefined,
            undefined,
            abort.signal,
          );
          if (!closed && !dirty) {
            if (!desktop) {
              const current = snapshot.session;
              if (!current)
                throw new ApiError(
                  0,
                  t(
                    "Cannot read service status. Check your connection and refresh.",
                  ),
                );
              if (
                current.csrf !== session.csrf ||
                current.identity.tenant_id !== session.identity.tenant_id ||
                current.identity.username !== session.identity.username ||
                !!current.identity.deployment_admin !==
                  !!session.identity.deployment_admin
              ) {
                close();
                callbacks.sessionChanged(current);
                return;
              }
            }
            clearTimeout(readRetry);
            callbacks.snapshot(snapshot);
          }
        }
      })()
        .catch((error: unknown) => {
          if (!closed) {
            callbacks.stale();
            if (error instanceof ApiError && error.status === 401) expired();
            else {
              clearTimeout(readRetry);
              readRetry = setTimeout(() => {
                void refresh().catch(() => {});
              }, 5000);
            }
          }
          throw error;
        })
        .finally(() => {
          pending = undefined;
          if (dirty && !closed) void refresh().catch(() => {});
        });
    }
    return pending;
  };
  const update = () => {
    void refresh().catch(() => {});
  };
  const connect = () => {
    if (closed) return;
    if (desktop) {
      void listenDesktop("bridge-changed", update)
        .then((stop) => {
          if (closed) {
            stop();
            return;
          }
          unlisten = stop;
          callbacks.mode("live");
          update();
        })
        .catch(() => {
          if (closed) return;
          callbacks.mode("polling");
          retry = setTimeout(() => {
            update();
            connect();
          }, 5000);
        });
      return;
    }
    const stream = new EventSource("/api/events");
    source = stream;
    const lost = () => {
      if (closed || source !== stream) return;
      stream.close();
      source = null;
      clearTimeout(watchdog);
      callbacks.stale();
      callbacks.mode("polling");
      // A successful fallback read permits operations using current data.
      update();
      retry = setTimeout(() => {
        update();
        connect();
      }, 5000);
    };
    const alive = () => {
      if (closed || source !== stream) return;
      clearTimeout(watchdog);
      watchdog = setTimeout(lost, 15000);
      callbacks.mode("live");
    };
    watchdog = setTimeout(lost, 15000);
    stream.addEventListener("changed", () => {
      alive();
      if (!closed && source === stream) update();
    });
    stream.addEventListener("heartbeat", alive);
    stream.addEventListener("session_expired", expired);
    stream.addEventListener("unavailable", lost);
    stream.onerror = lost;
  };
  callbacks.mode("connecting");
  update();
  connect();
  return { refresh, close };
}
