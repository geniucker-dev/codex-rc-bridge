import { t } from "./i18n";
import { useEffect, useState } from "react";
import type { Api, Perform } from "./ui-types";
import { desktop } from "./desktop";
type Network = {
  configured: boolean;
  mode: "direct" | "proxy";
  address: string | null;
  authenticated: boolean;
  restart_failures?: number;
};
export function NetworkSettings({
  api,
  perform,
}: {
  api: Api;
  perform: Perform;
}) {
  const [current, setCurrent] = useState<Network | null>(null);
  const [mode, setMode] = useState<"direct" | "proxy">("direct");
  const [url, setUrl] = useState("");
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);
  useEffect(() => {
    let active = true;
    void api<Network>("/deployment/network")
      .then((value) => {
        if (!active) return;
        setCurrent(value);
        setMode(value.mode);
        setUrl(value.authenticated ? "" : (value.address ?? ""));
        setError("");
      })
      .catch((e) => {
        if (active) setError(e.message);
      });
    return () => {
      active = false;
    };
  }, [reload]);
  if (error)
    return (
      <div role="alert">
        <p>{error}</p>
        <button onClick={() => setReload((n) => n + 1)}>
          {t("Reload network settings")}
        </button>
      </div>
    );
  if (!current) return <p role="status">{t("Loading network settings…")}</p>;
  return (
    <section className="network-settings">
      <h2>{t("Network proxy")}</h2>
      <p>
        {desktop
          ? t("Shared by all accounts in this application.")
          : t(
              "Shared by the whole deployment. Only administrators can change it.",
            )}
        {t(
          "Used for account authorization, renewal and remote-control connections.",
        )}
      </p>
      <p className="notice">
        {t("Currently active: ")}
        {current.mode === "direct" ? t("Direct connection") : current.address}
        {current.authenticated ? t(" (proxy authentication configured)") : ""}
      </p>
      {!current.configured && (
        <p className="warning">
          {t("Choose a network connection before adding ChatGPT accounts.")}
        </p>
      )}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void perform(
            t("Save and apply proxy"),
            async () => {
              const value = await api<Network>("/deployment/network", "POST", {
                proxy_url: mode === "proxy" ? url : null,
              });
              setCurrent(value);
              setUrl(value.authenticated ? "" : (value.address ?? ""));
              return value.restart_failures
                ? t(
                    "Network settings applied. Some connections could not recover. Check their authorization and status on Connections.",
                  )
                : t(
                    "Network settings applied. Previously enabled connections will reconnect; stopped connections remain stopped.",
                  );
            },
            false,
          );
        }}
      >
        <label>
          {t("Connection method")}
          <select
            value={mode}
            onChange={(e) => setMode(e.target.value as "direct" | "proxy")}
          >
            <option value="direct">{t("Direct connection")}</option>
            <option value="proxy">{t("HTTP proxy")}</option>
          </select>
        </label>
        {mode === "proxy" && (
          <>
            <label>
              {t("Proxy address")}
              <input
                type="text"
                autoComplete="off"
                spellCheck={false}
                required
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="http://127.0.0.1:7890"
              />
            </label>
            <p className="hint">
              {t(
                "Use http://host:port, optionally with a username and password. Credentials are not displayed after saving; enter the full address when changing an authenticated proxy. SOCKS and HTTPS proxies are not supported.",
              )}
            </p>
          </>
        )}
        <p>
          {t(
            desktop
              ? "Applying settings briefly disconnects all active bridges in this application, then reconnects them. Tasks already running on the controlled device will continue."
              : "Applying settings briefly disconnects all active bridges in this deployment, then reconnects them. Tasks already running on the controlled device will continue.",
          )}
        </p>
        <button className="primary">{t("Save and apply")}</button>
      </form>
    </section>
  );
}
