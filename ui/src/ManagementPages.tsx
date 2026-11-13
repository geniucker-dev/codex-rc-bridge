import { useState } from "react";
import { t } from "./i18n";
import {
  accountLabel,
  accountIdentity,
  sameAccount,
  type AccountInfo,
  type EndpointInfo,
  type Chain,
} from "./api";
import type { View } from "./navigation";
import type { Ops } from "./ops";
import { Home, joinNames } from "./Home";
import { ControllerDetail, DeviceDetail } from "./Detail";

export function ManagementPages({
  view,
  accounts,
  endpoints,
  chains,
  ops,
}: {
  view: View;
  accounts: AccountInfo[];
  endpoints: EndpointInfo[];
  chains: Chain[];
  ops: Ops;
}) {
  if ("controller" in view)
    return (
      <ControllerDetail
        id={view.controller}
        accounts={accounts}
        endpoints={endpoints}
        chains={chains}
        ops={ops}
      />
    );
  if ("device" in view)
    return (
      <DeviceDetail
        id={view.device}
        accounts={accounts}
        endpoints={endpoints}
        chains={chains}
        ops={ops}
      />
    );
  if (view.page === "home")
    return (
      <Home
        accounts={accounts}
        endpoints={endpoints}
        chains={chains}
        ops={ops}
      />
    );
  return <Accounts accounts={accounts} endpoints={endpoints} ops={ops} />;
}

// Accounts whose sign-in expired come first, with what they affect and the fix.
function Accounts({
  accounts,
  endpoints,
  ops,
}: {
  accounts: AccountInfo[];
  endpoints: EndpointInfo[];
  ops: Ops;
}) {
  const [checking, setChecking] = useState<string | null>(null);
  const key = (account: AccountInfo) =>
    `${account.workspace_id}/${account.user_id}`;
  const check = (account: AccountInfo) =>
    ops.api("/accounts/check", "POST", { account: accountIdentity(account) });
  const sorted = [...accounts].sort(
    (a, b) => Number(b.sign_in_required) - Number(a.sign_in_required),
  );
  const expired = accounts.filter((a) => a.sign_in_required).length;
  return (
    <>
      <div className="toolbar">
        <span>
          {expired
            ? t("Accounts: {0}, {1} need to sign in again", [
                accounts.length,
                expired,
              ])
            : t("Accounts: {0}", [accounts.length])}
        </span>
        <div className="actions">
          {accounts.length > 0 && (
            <button
              disabled={checking !== null}
              onClick={() =>
                void ops.perform(t("Check all sign-ins"), async () => {
                  let failed = 0;
                  for (const account of accounts) {
                    setChecking(key(account));
                    try {
                      await check(account);
                    } catch {
                      failed += 1;
                    }
                  }
                  setChecking(null);
                  return failed
                    ? t(
                        "{0} accounts failed the check. Any that need to sign in again are marked below.",
                        [failed],
                      )
                    : t("Every account's sign-in is valid.");
                })
              }
            >
              {t("Check all sign-ins")}
            </button>
          )}
          <button className="primary" onClick={() => ops.authorize()}>
            {t("Add account")}
          </button>
        </div>
      </div>
      {!accounts.length && (
        <div className="empty">
          <h2>{t("Connect your ChatGPT accounts")}</h2>
          <p>
            {t(
              "Add the accounts used by your controller and controlled device. They can be the same or different.",
            )}
          </p>
        </div>
      )}
      {sorted.map((account) => {
        const users = endpoints.filter((item) =>
          sameAccount(item.endpoint.account, account),
        );
        return (
          <article
            className={`account-row${account.sign_in_required ? " needs-attention" : ""}`}
            key={key(account)}
            aria-label={accountLabel(account, accounts)}
          >
            <div>
              <h2 className="id-text">{accountLabel(account, accounts)}</h2>
              {account.email &&
                account.name &&
                account.name !== account.email && (
                  <p className="hint">{account.name}</p>
                )}
              {!account.email && (
                <p className="hint">
                  {t("No email provided for this account")}
                </p>
              )}
              {account.sign_in_required && (
                <p className="account-alert">
                  <strong>{t("Sign-in expired")}</strong>{" "}
                  {t(
                    "Sign in again; connections using this account resume afterwards.",
                  )}
                </p>
              )}
              {users.length ? (
                (["in", "out"] as const).map((kind) => {
                  const names = users
                    .filter((item) => item.endpoint.kind === kind)
                    .map((item) => item.endpoint.display_name);
                  return (
                    names.length > 0 && (
                      <p className="hint" key={kind}>
                        {kind === "in"
                          ? t("Controllers: {0}", [joinNames(names)])
                          : t("Controlled devices: {0}", [joinNames(names)])}
                      </p>
                    )
                  );
                })
              ) : (
                <p className="hint">
                  {t("Not used by any controller or controlled device")}
                </p>
              )}
              <details className="account-details">
                <summary>{t("Account details")}</summary>
                <p className="hint id-text">
                  {t("User ID: ")}
                  {account.user_id}
                </p>
                <p className="hint id-text">
                  {t("Workspace ID: ")}
                  {account.workspace_id}
                </p>
              </details>
            </div>
            <div className="actions">
              <button
                className={account.sign_in_required ? "primary" : ""}
                onClick={() => ops.authorize(undefined, account)}
              >
                {t("Sign in again")}
              </button>
              <button
                disabled={checking !== null}
                onClick={() =>
                  void ops.perform(t("Check account sign-in"), async () => {
                    setChecking(key(account));
                    try {
                      await check(account);
                    } finally {
                      setChecking(null);
                    }
                    return t(
                      "Account credentials are valid. This does not mean the controlled device is online.",
                    );
                  })
                }
              >
                {checking === key(account)
                  ? t("Checking…")
                  : t("Check sign-in")}
              </button>
            </div>
          </article>
        );
      })}
    </>
  );
}
