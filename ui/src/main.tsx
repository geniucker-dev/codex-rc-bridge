import { LanguageSelect } from "./LanguageSelect";
import { useLanguage, locale } from "./i18n";
import { useView, type View } from "./navigation";
import { syncDevices, type Ops } from "./ops";
import { t } from "./i18n";
import { desktop, listenDesktop, setDesktopLanguage } from "./desktop";
import { NetworkSettings } from "./NetworkSettings";
import { ManagementPages } from "./ManagementPages";
import { UsersPage } from "./Users";
import { OperationDialog, type Dialog } from "./OperationDialog";
import { submit, type Page } from "./ui-types";
import { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  request,
  operationMessage,
  accountIdentity,
  ApiError,
  type Account,
  type LoginStart,
  type Reply,
  type Snapshot,
  type Session,
} from "./api";
import "./style.css";
import { syncState } from "./state-sync";

function App() {
  useLanguage();
  const currentLocale = locale();
  useEffect(() => {
    if (desktop) void setDesktopLanguage(currentLocale).catch(() => {});
  }, [currentLocale]);
  const names: Record<Page, string> = {
    home: t("Remote control"),
    accounts: t("Accounts"),
    users: t("Users"),
    settings: t("Settings"),
  };

  const [mode, setMode] = useState<
    "loading" | "foundation" | "management" | "offline"
  >("loading");
  const [session, setSession] = useState<Session | null>(null);
  const [data, setData] = useState<Snapshot | null>(null);
  const [view, go] = useView();
  const page = view.page;
  const setPage = (next: Page) => go({ page: next });
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [busy, setBusy] = useState("");
  const [notice, setNotice] = useState("");
  // Failures stay until the next action; confirmations fade after a short read.
  const [noticeTone, setNoticeTone] = useState<"done" | "error">("done");
  // Completed transient feedback belongs to the language in which it was shown.
  useEffect(() => setNotice(""), [currentLocale]);
  const [fresh, setFresh] = useState(true);
  // Pages that read beyond the snapshot reload when status is refreshed.
  const [refreshed, setRefreshed] = useState(0);
  const operation = useRef<symbol | null>(null);
  const sessionEpoch = useRef(0);
  const renderEpoch = sessionEpoch.current;
  const guardedDialog = (next: Dialog | null) => {
    if (renderEpoch === sessionEpoch.current) setDialog(next);
  };
  const guardedNotice = (next: string) => {
    if (renderEpoch === sessionEpoch.current) setNotice(next);
  };
  const guardedGo = (next: View) => {
    if (renderEpoch === sessionEpoch.current) go(next);
  };
  const sync = useRef<ReturnType<typeof syncState> | null>(null);
  const [updates, setUpdates] = useState<"connecting" | "live" | "polling">(
    "connecting",
  );
  const accounts = data?.configuration.value.accounts ?? [];
  const endpoints = data?.configuration.value.endpoints ?? [];
  const chains = data?.chains ?? [];
  const api = <T,>(path: string, method = "GET", body?: unknown) =>
    request<T>(path, method, body, session?.csrf);
  const resetSession = () => {
    sessionEpoch.current++;
    sync.current?.close();
    sync.current = null;
    operation.current = null;
    setBusy("");
    setSession(null);
    setPage("home");
    setData(null);
    setDialog(null);
  };
  async function bootstrap() {
    const epoch = ++sessionEpoch.current;
    setMode("loading");
    setNotice("");
    try {
      const capabilities = await request<{ management?: boolean }>(
        "/capabilities",
      );
      if (epoch !== sessionEpoch.current) return;
      if (!capabilities.management) {
        setMode("foundation");
        return;
      }
      try {
        const current = await request<Session>("/session");
        if (epoch !== sessionEpoch.current) return;
        setSession(current);
      } catch (error) {
        if (epoch !== sessionEpoch.current) return;
        if (!(error instanceof ApiError && error.status === 401)) throw error;
      }
      setMode("management");
    } catch (error) {
      if (epoch !== sessionEpoch.current) return;
      setMode("offline");
      setNotice((error as Error).message);
    }
  }
  useEffect(() => {
    void bootstrap();
  }, []);
  useEffect(() => {
    if (!session) return;
    const subscription = syncState(session, {
      sessionChanged: (current) => {
        resetSession();
        setNotice("");
        setFresh(false);
        setSession(current);
      },
      snapshot: (current) => {
        setData(current);
        setFresh(true);
      },
      stale: () => setFresh(false),
      expired: () => {
        resetSession();
        setNotice(t("Your management session has expired. Sign in again."));
      },
      mode: setUpdates,
    });
    sync.current = subscription;
    return () => {
      subscription.close();
      if (sync.current === subscription) sync.current = null;
    };
  }, [session]);
  useEffect(() => {
    if (!session?.identity.deployment_admin) return;
    let active = true;
    void request<{ configured: boolean }>("/deployment/network")
      .then((value) => {
        if (active && !value.configured) setPage("settings");
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [session]);
  useEffect(() => {
    if (!desktop) return;
    let active = true;
    let stop: (() => void) | undefined;
    void listenDesktop("desktop-exiting", () => {
      operation.current = Symbol(t("Quit"));
      setBusy(t("Stop bridging and quit"));
    }).then((unsubscribe) => {
      if (active) stop = unsubscribe;
      else unsubscribe();
    });
    return () => {
      active = false;
      stop?.();
    };
  }, []);
  useEffect(() => {
    if (!desktop || dialog?.kind !== "authorize") return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const id = dialog.login.id;
    const check = async () => {
      try {
        const result = await request<{
          status: string;
          error?: { code?: string; message?: string };
        }>(`/logins/${id}`);
        if (!active) return;
        if (result.status !== "pending" && result.status !== "processing") {
          setDialog(
            result.status === "complete" && dialog.next ? dialog.next : null,
          );
          setNoticeTone(result.status === "complete" ? "done" : "error");
          setNotice(
            result.status === "complete"
              ? t("Authorization saved.")
              : result.status === "expired"
                ? t(
                    "This authorization has expired or was canceled. Start again.",
                  )
                : operationMessage(result.error),
          );
          void sync.current?.refresh().catch(() => {});
          return;
        }
      } catch {
        /* Manual submission remains available while the listener is unavailable. */
      }
      if (active) timer = setTimeout(() => void check(), 1000);
    };
    void check();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [dialog]);
  useEffect(() => {
    // Only confirmations inside the workspace fade; sign-in and service messages stay.
    if (!session || !notice || busy || noticeTone !== "done" || dialog) return;
    const timer = setTimeout(() => setNotice(""), 6000);
    return () => clearTimeout(timer);
  }, [session, notice, busy, noticeTone, dialog]);
  async function perform(
    label: string,
    action: () => Promise<string | void>,
    refresh = true,
  ) {
    if (operation.current) return;
    const ticket = Symbol(label);
    const epoch = sessionEpoch.current;
    operation.current = ticket;
    setBusy(label);
    setNotice("");
    setNoticeTone("done");
    try {
      const message = await action();
      if (epoch !== sessionEpoch.current) return;
      setNotice(message ?? t("{0} completed.", [label]));
      if (refresh && session) {
        try {
          await sync.current?.refresh();
        } catch {
          if (epoch !== sessionEpoch.current) return;
          setFresh(false);
          setNoticeTone("error");
          setNotice(
            t("{0} Status could not be refreshed. Check again.", [
              message ?? t("{0} completed.", [label]),
            ]),
          );
        }
      }
    } catch (error) {
      if (epoch !== sessionEpoch.current) return;
      if (error instanceof ApiError && error.status === 401) resetSession();
      else if (session) void sync.current?.refresh().catch(() => {});
      if (error instanceof ApiError && error.status === 0) setFresh(false);
      setNoticeTone("error");
      setNotice((error as Error).message);
    } finally {
      if (operation.current === ticket) {
        operation.current = null;
        setBusy("");
      }
    }
  }
  const closeDialog = () => {
    if (dialog?.kind === "authorize") {
      void perform(
        t("Cancel authorization"),
        async () => {
          if (!desktop) setDialog(null);
          await api(`/logins/${dialog.login.id}`, "DELETE");
          if (desktop) setDialog(null);
        },
        false,
      );
    } else {
      setDialog(null);
      setNotice("");
    }
  };
  const lead =
    page === "home"
      ? t(
          "Each controller lets its paired devices, through its ChatGPT account, control a computer that may use another account.",
        )
      : page === "users"
        ? t(
            "Each user has a separate sign-in and their own accounts and devices. Only administrators can add or remove users.",
          )
        : page === "settings"
          ? desktop
            ? t("Set the network proxy used by this application.")
            : t(
                "Network settings apply to the whole deployment and require administrator access.",
              )
          : t(
              "Sign in to the ChatGPT accounts used on the devices you control from and the computers you control. One account can serve several controllers and controlled devices.",
            );
  // The desktop app has one user; on the web only administrators manage users.
  const canManageUsers = !desktop && !!session?.identity.deployment_admin;
  useEffect(() => {
    if (session && page === "users" && !canManageUsers) setPage("home");
  }, [session, page, canManageUsers]);
  const detailId =
    "controller" in view
      ? view.controller
      : "device" in view
        ? view.device
        : "";
  const detail = detailId
    ? endpoints.find((item) => item.endpoint.device_id === detailId)
    : undefined;
  const ops: Ops = {
    api,
    perform,
    setDialog: guardedDialog,
    setNotice: guardedNotice,
    go: guardedGo,
    authorize: (output?: string, expected?: Account, next?: Dialog) =>
      void perform(
        t("Prepare authorization"),
        async () => {
          const result = await api<Reply<LoginStart>>(
            output ? `/outputs/${output}/authorize` : "/accounts/login",
            "POST",
            output
              ? undefined
              : { expected: expected && accountIdentity(expected) },
          );
          guardedDialog({ kind: "authorize", login: result.value, next });
          return t("Authorization link ready.");
        },
        false,
      ),
  };
  return (
    <div className="app-shell">
      <header className="app-header">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            <svg
              viewBox="0 0 1024 1024"
              fill="none"
              stroke="currentColor"
              strokeWidth="72"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M260 380h500l-130-130M764 644H264l130 130" />
            </svg>
          </span>
          <strong>Codex RC Bridge</strong>
        </div>
        <span className="preview">{desktop ? t("Desktop") : t("Web")}</span>
        {desktop && (
          <span className="hint">
            {t("Closing the window keeps the bridge running in the menu bar.")}
          </span>
        )}
        {!desktop && <LanguageSelect />}
        {session && !desktop && (
          <div className="user-menu">
            <span>{session.identity.username}</span>
            <button
              className="quiet"
              disabled={!!busy}
              onClick={() => setDialog({ kind: "change-password" })}
            >
              {t("Change password")}
            </button>
            <button
              className="quiet"
              disabled={!!busy}
              onClick={() =>
                void perform(
                  t("Sign out"),
                  async () => {
                    await api("/session", "DELETE");
                    resetSession();
                  },
                  false,
                )
              }
            >
              {t("Sign out")}
            </button>
          </div>
        )}
      </header>
      {mode !== "management" ? (
        <main className="welcome">
          <h1>{t("Connect across accounts")}</h1>
          <p>{t("Bridge remote control between different accounts.")}</p>
          <div className="service-message" role="status">
            <h2>
              {mode === "loading"
                ? t("Connecting to the service")
                : mode === "foundation"
                  ? t("Management is not configured")
                  : t("Cannot connect to the service")}
            </h2>
            <p>
              {mode === "foundation"
                ? t(
                    "Configure management sign-in to add accounts, inputs and outputs.",
                  )
                : notice || t("Checking management access.")}
            </p>
            <button
              disabled={mode === "loading"}
              onClick={() => void bootstrap()}
            >
              {t("Check again")}
            </button>
          </div>
        </main>
      ) : !session ? (
        <main className="login-layout">
          <div className="login-copy">
            <h1>
              {t("One device controls another,")}
              <br />
              {t("across ChatGPT accounts.")}
            </h1>
            <p>
              {t(
                "Control a computer running Codex from a phone, tablet or another computer, even when each is signed in to a different ChatGPT account. Each user's accounts and devices stay separate.",
              )}
            </p>
            <div className="route-track login-route" aria-hidden="true">
              {[
                [t("Your device"), t("Account A")],
                [t("Bridge"), t("Connects the two accounts")],
                [t("Your computer"), t("Account B, runs Codex")],
              ].map(([name, note], index) => (
                <div
                  className={`stop stop-live${index === 2 ? " stop-device" : ""}`}
                  key={name}
                >
                  <div className="rail">
                    <span className="node" />
                    {index < 2 && <span className="span span-live" />}
                  </div>
                  <div className="stop-text">
                    <h2>{name}</h2>
                    <p>{note}</p>
                  </div>
                </div>
              ))}
            </div>
          </div>
          <form
            className="login-form"
            onSubmit={(e) =>
              submit(e, (form) => {
                e.currentTarget.reset();
                void perform(
                  t("Sign in to management"),
                  async () => {
                    setSession(
                      await request<Session>("/session", "POST", {
                        username: form.get("username"),
                        password: form.get("password"),
                      }),
                    );
                    return "";
                  },
                  false,
                );
              })
            }
          >
            <h2>{t("Sign in to management")}</h2>
            <p>
              {t(
                "Use the credentials provided by your deployment administrator.",
              )}
            </p>
            {notice && (
              <p className="notice" role="alert">
                {notice}
              </p>
            )}
            <fieldset disabled={!!busy}>
              <label>
                {t("Username")}
                <input
                  name="username"
                  autoComplete="username"
                  required
                  maxLength={64}
                />
              </label>
              <label>
                {t("Password")}
                <input
                  name="password"
                  type="password"
                  autoComplete="current-password"
                  required
                />
              </label>
              <button className="primary">{busy || t("Sign in")}</button>
            </fieldset>
          </form>
        </main>
      ) : (
        <div className="workspace">
          <nav aria-label={t("Management navigation")}>
            {(Object.keys(names) as Page[])
              .filter((key) =>
                key === "settings"
                  ? desktop || session.identity.deployment_admin
                  : key !== "users" || canManageUsers,
              )
              .map((key) => (
                <button
                  key={key}
                  aria-current={
                    page === key ? (detail ? "true" : "page") : undefined
                  }
                  onClick={() => {
                    if (key !== page && !busy) setNotice("");
                    setPage(key);
                  }}
                >
                  {names[key]}
                </button>
              ))}
          </nav>
          <main className="content">
            <div className="page-heading">
              <div>
                {detail && (
                  <a
                    className="crumb"
                    href="#/home"
                    onClick={(event) => {
                      event.preventDefault();
                      if (!busy) setNotice("");
                      setPage("home");
                    }}
                  >
                    {t("Remote control")}
                  </a>
                )}
                <h1>{detail ? detail.endpoint.display_name : names[page]}</h1>
                <p>
                  {detail
                    ? detail.endpoint.kind === "in"
                      ? t("Controller")
                      : t("Controlled device")
                    : lead}
                </p>
              </div>
              <div className="sync">
                <span
                  className={`sync-state sync-${fresh ? updates : "stale"}`}
                  aria-live="polite"
                >
                  {!fresh
                    ? t("Status updates stopped")
                    : updates === "live"
                      ? t("Status is up to date")
                      : updates === "polling"
                        ? t("Checking status every few seconds")
                        : t("Connecting to status updates…")}
                </span>
                <button
                  className="quiet"
                  disabled={!!busy}
                  onClick={() =>
                    void perform(
                      t("Refresh status"),
                      async () => {
                        setRefreshed((n) => n + 1);
                        syncDevices(api, endpoints);
                        await sync.current?.refresh();
                      },
                      false,
                    )
                  }
                >
                  {t("Refresh status")}
                </button>
              </div>
            </div>
            {!dialog && (busy || notice) && (
              <p
                className={`notice${!busy && noticeTone === "error" ? " error" : ""}`}
                role="status"
              >
                {busy ? t("{0}…", [busy]) : notice}
              </p>
            )}
            {!fresh && (
              <p className="warning" role="alert">
                {t(
                  "Status updates stopped. The information below may be outdated. Refresh before acting.",
                )}
              </p>
            )}
            {!data ? (
              <p role="status">
                {fresh
                  ? t("Loading your configuration…")
                  : t("Service status is unavailable.")}
              </p>
            ) : (
              <fieldset disabled={!!busy || !fresh}>
                {page === "users" && canManageUsers ? (
                  <UsersPage
                    key={renderEpoch}
                    api={api}
                    refreshed={refreshed}
                    open={guardedDialog}
                  />
                ) : page === "settings" ? (
                  <>
                    {desktop && (
                      <section className="general-settings">
                        <h2>{t("General")}</h2>
                        <LanguageSelect />
                      </section>
                    )}
                    {session.identity.deployment_admin && (
                      <NetworkSettings
                        key={renderEpoch}
                        api={api}
                        perform={perform}
                      />
                    )}
                  </>
                ) : (
                  <ManagementPages
                    view={view}
                    accounts={accounts}
                    endpoints={endpoints}
                    chains={chains}
                    ops={ops}
                  />
                )}
              </fieldset>
            )}
          </main>
        </div>
      )}
      {dialog && (
        <OperationDialog
          dialog={dialog}
          accounts={accounts}
          endpoints={endpoints}
          chains={chains}
          busy={busy}
          fresh={fresh}
          notice={notice}
          api={api}
          perform={perform}
          closeDialog={closeDialog}
          setDialog={guardedDialog}
          addAccount={(next) => ops.authorize(undefined, undefined, next)}
        />
      )}
    </div>
  );
}
createRoot(document.getElementById("root")!).render(<App />);
