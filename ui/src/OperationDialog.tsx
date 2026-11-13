import { t, locale } from "./i18n";
import { desktop, openAuthorization } from "./desktop";
import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  accountLabel,
  accountIdentity,
  operationMessage,
  type AccountInfo,
  type EndpointInfo,
  type Chain,
  type LoginStart,
  type Reply,
} from "./api";
import { submit, type Api, type Perform } from "./ui-types";
import { showPairingCode } from "./ops";
import { endpointIssue } from "./Attention";
import { userDialog, type UserDialog } from "./Users";
export type Dialog =
  | UserDialog
  | { kind: "create"; role: "in" | "out" }
  | { kind: "authorize"; login: LoginStart; next?: Dialog }
  // `device` is the controller whose code this is; its paired device shows up here.
  | {
      kind: "pair-code";
      device: string;
      code: string;
      expires: number;
    }
  | { kind: "connect"; input?: string; output?: string }
  | {
      kind:
        | "pair-in"
        | "pair-out"
        | "unbind"
        | "switch"
        | "delete"
        | "restore-record";
      device: string;
    };

const time = (seconds: number) =>
  new Date(seconds * 1000).toLocaleTimeString(locale(), {
    hour: "2-digit",
    minute: "2-digit",
  });
function Modal({
  title,
  busy,
  disabled,
  message,
  close,
  children,
}: {
  title: string;
  busy: boolean;
  disabled: boolean;
  message: string;
  close: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = ref.current!;
    element.showModal();
    return () => element.close();
  }, []);
  return (
    <dialog
      ref={ref}
      aria-labelledby="dialog-title"
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) close();
      }}
    >
      <div className="dialog-head">
        <h2 id="dialog-title">{title}</h2>
        <button
          type="button"
          className="quiet"
          disabled={busy}
          onClick={close}
          aria-label={t("Close dialog")}
        >
          {t("Close")}
        </button>
      </div>
      {message && (
        <p className="notice" role="status">
          {message}
        </p>
      )}
      <fieldset disabled={busy || disabled}>{children}</fieldset>
    </dialog>
  );
}

export function OperationDialog({
  dialog,
  accounts,
  endpoints,
  chains,
  busy,
  fresh,
  notice,
  api,
  perform,
  closeDialog,
  setDialog,
  addAccount,
}: {
  dialog: Dialog;
  accounts: AccountInfo[];
  endpoints: EndpointInfo[];
  chains: Chain[];
  busy: string;
  fresh: boolean;
  notice: string;
  api: Api;
  perform: Perform;
  closeDialog: () => void;
  setDialog: (value: Dialog | null) => void;
  // Starts an account sign-in and returns to `next` once it is saved.
  addAccount: (next: Dialog) => void;
}) {
  const [selectedAccount, setSelectedAccount] = useState("");
  const [manualCopyFor, setManualCopyFor] = useState<string | null>(null);
  const endpoint = (id: string) =>
    endpoints.find((item) => item.endpoint.device_id === id);
  const activeDevice =
    "device" in dialog
      ? endpoint(dialog.device)
      : dialog.kind === "connect"
        ? endpoint(dialog.input ?? dialog.output ?? "")
        : undefined;
  const boundIn = (id: string) => chains.some((c) => c.binding.in_id === id);
  const boundOut = (id: string, except?: string) =>
    chains.some((c) => c.binding.out_id === id && c.binding.in_id !== except);
  // Only ends with nothing left to fix can be connected now.
  const usable = (item: EndpointInfo) =>
    item.lifecycle === "active" && !endpointIssue(item, accounts);
  const deviceChoices = (input?: string) =>
    endpoints.filter(
      (item) =>
        item.endpoint.kind === "out" &&
        usable(item) &&
        !boundOut(item.endpoint.device_id, input),
    );
  const controllerChoices = () =>
    endpoints.filter(
      (item) =>
        item.endpoint.kind === "in" &&
        usable(item) &&
        !boundIn(item.endpoint.device_id),
    );
  // A choice is kept by ID; if it becomes unavailable it is cleared and must be made again.
  const [selected, setSelected] = useState("");
  const choiceIds = (
    dialog.kind === "switch"
      ? deviceChoices(dialog.device)
      : dialog.kind === "connect"
        ? dialog.input
          ? deviceChoices()
          : controllerChoices()
        : []
  ).map((item) => item.endpoint.device_id);
  const selectionValid = choiceIds.includes(selected);
  useEffect(() => {
    if (!selectionValid) setSelected("");
  }, [selectionValid, selected]);
  const chosen = selectionValid ? selected : "";
  const option = (item: EndpointInfo) => (
    <option key={item.endpoint.device_id} value={item.endpoint.device_id}>
      {item.endpoint.kind === "out"
        ? t("{0} ({1})", [
            item.endpoint.display_name,
            item.target_name ?? t("Unnamed computer"),
          ])
        : item.endpoint.display_name}
    </option>
  );
  let modalTitle = "";
  let modalContent: ReactNode;
  if (
    dialog.kind === "add-user" ||
    dialog.kind === "reset-password" ||
    dialog.kind === "delete-user" ||
    dialog.kind === "change-password"
  ) {
    [modalTitle, modalContent] = userDialog(dialog, api, perform, () =>
      setDialog(null),
    );
  } else if (dialog.kind === "create") {
    const controller = dialog.role === "in";
    modalTitle = controller ? t("New controller") : t("New controlled device");
    modalContent = (
      <form
        onSubmit={(e) =>
          submit(
            e,
            (form) =>
              void perform(
                controller ? t("Add controller") : t("Add controlled device"),
                async () => {
                  const account = accounts.find(
                    (item) =>
                      JSON.stringify(accountIdentity(item)) === selectedAccount,
                  );
                  if (!account)
                    throw new Error(t("Add the ChatGPT account first."));
                  const created = await api<Reply<{ device_id: string }>>(
                    "/endpoints",
                    "POST",
                    {
                      kind: dialog.role,
                      account: accountIdentity(account),
                      name: form.get("name"),
                    },
                  );
                  const id = created.value.device_id;
                  // Go straight to the next step: a code for the phone, or
                  // authorization followed by pairing the computer.
                  if (controller) {
                    const code = await api<
                      Reply<{ code: string; expires_at: number }>
                    >(`/inputs/${id}/pair`, "POST");
                    setDialog({
                      kind: "pair-code",
                      device: id,
                      code: code.value.code,
                      expires: code.value.expires_at,
                    });
                    return t(
                      "Controller added. Enter the pairing code in ChatGPT on the device you control from.",
                    );
                  }
                  const login = await api<Reply<LoginStart>>(
                    `/outputs/${id}/authorize`,
                    "POST",
                  );
                  setDialog({
                    kind: "authorize",
                    login: login.value,
                    next: { kind: "pair-out", device: id },
                  });
                  return t(
                    "Controlled device added. Sign in with the computer's ChatGPT account.",
                  );
                },
              ),
          )
        }
      >
        <p>
          {controller
            ? t(
                "A controller appears as a remote device in ChatGPT on the device you control from, whether a phone, tablet or computer. Choose the account signed in to ChatGPT there.",
              )
            : t(
                "A controlled device reaches one computer running Codex. Choose the account signed in on that computer; it can differ from the controller's.",
              )}
        </p>
        {accounts.length ? (
          <label>
            {controller
              ? t("Account on the controlling device")
              : t("Account on the computer")}
            <select
              name="account"
              required
              value={selectedAccount}
              onChange={(event) => setSelectedAccount(event.target.value)}
            >
              <option value="" disabled>
                {t("Choose account")}
              </option>
              {accounts.map((a) => (
                <option
                  key={JSON.stringify(accountIdentity(a))}
                  value={JSON.stringify(accountIdentity(a))}
                >
                  {accountLabel(a, accounts)}
                </option>
              ))}
            </select>
          </label>
        ) : (
          <p className="warning">
            {t("No ChatGPT account has been added yet. Add one first.")}
          </p>
        )}
        <div className="actions">
          <button type="button" onClick={() => addAccount(dialog)}>
            {t("Add account")}
          </button>
        </div>
        <label>
          {t("Name")}
          <input
            name="name"
            required
            maxLength={80}
            autoComplete="off"
            placeholder={controller ? t("My devices") : t("Work MacBook")}
          />
        </label>
        <p className="hint">
          {t(
            "The name can't be changed later. Creating it again with the same account and name restores the same identity.",
          )}
        </p>
        <button className="primary" disabled={!accounts.length}>
          {controller ? t("Add and show code") : t("Add and authorize")}
        </button>
      </form>
    );
  } else if (dialog.kind === "authorize") {
    modalTitle = t("Complete ChatGPT authorization");
    modalContent = (
      <form
        onSubmit={(e) =>
          submit(e, (form) => {
            e.currentTarget.reset();
            void perform(t("Submit authorization"), async () => {
              // Token exchange consumes the transaction even when the remote reply fails.
              if (!desktop) setDialog(null);
              await api(`/logins/${dialog.login.id}`, "POST", {
                callback: form.get("callback"),
              });
              setDialog(dialog.next ?? null);
              return dialog.next
                ? t("Authorization saved. Pair the computer next.")
                : t("Authorization saved.");
            });
          })
        }
      >
        <p>
          {desktop
            ? dialog.login.automatic_callback
              ? t(
                  "Sign in with the correct account in your system browser, then return here. Authorization is saved automatically. You can also submit the callback URL manually.",
                )
              : t(
                  "The local callback port is busy. Finish authorization in your browser, then copy the complete callback URL below.",
                )
            : t(
                "Sign in with the correct account in a new window. Paste the complete callback URL from the address bar below, even if the localhost page cannot open.",
              )}
        </p>
        <div className="actions">
          <a
            className="button primary"
            href={dialog.login.url}
            onClick={
              desktop
                ? (event) => {
                    event.preventDefault();
                    void perform(
                      t("Open authorization page"),
                      async () => {
                        await openAuthorization(dialog.login.url);
                      },
                      false,
                    );
                  }
                : undefined
            }
            target="_blank"
            rel="noreferrer noopener"
          >
            {t("Open authorization page")}
          </a>
          <button
            type="button"
            onClick={() =>
              void perform(
                t("Copy authorization link"),
                async () => {
                  try {
                    await navigator.clipboard.writeText(dialog.login.url);
                    setManualCopyFor(null);
                    return t("Authorization link copied.");
                  } catch {
                    setManualCopyFor(dialog.login.id);
                    return t(
                      "Clipboard access was denied. Select the link below and copy it manually.",
                    );
                  }
                },
                false,
              )
            }
          >
            {t("Copy authorization link")}
          </button>
        </div>
        {manualCopyFor === dialog.login.id && (
          <label>
            {t("Authorization link (copy manually)")}
            <textarea
              readOnly
              value={dialog.login.url}
              rows={3}
              onFocus={(event) => event.currentTarget.select()}
              autoComplete="off"
              spellCheck={false}
            />
          </label>
        )}
        <p className="hint">
          {t(
            "This link expires at {0} and is only valid for this authorization.",
            [time(dialog.login.expires_at)],
          )}
        </p>
        <label>
          {t("Complete callback URL")}
          <textarea
            name="callback"
            required
            rows={4}
            autoComplete="off"
            spellCheck={false}
            placeholder="http://localhost:1455/auth/callback?..."
          />
        </label>
        <button className="primary">{t("Save authorization")}</button>
      </form>
    );
  } else if (dialog.kind === "pair-code") {
    // perform() keeps the operation busy through its post-mutation snapshot read.
    // That read can already contain a result; the waiting state may be coalesced.
    const ready = !busy && fresh;
    const paired = ready ? activeDevice?.paired_device : null;
    const failed = ready ? activeDevice?.pairing_error : null;
    modalTitle = t("Pair a device");
    modalContent = failed ? (
      <>
        <p className="warning" role="alert">
          {operationMessage({ code: failed })}
        </p>
        <div className="actions">
          <button onClick={closeDialog}>{t("Close")}</button>
          <button
            className="primary"
            onClick={() =>
              showPairingCode(
                { api, perform, setDialog },
                dialog.device,
                t("Pair device again"),
              )
            }
          >
            {t("Pair device again")}
          </button>
        </div>
      </>
    ) : paired ? (
      <>
        <p role="status">
          {t("Paired with {0}.", [paired.name ?? t("Unnamed device")])}{" "}
          {t("Open remote control in ChatGPT on it to use this controller.")}
        </p>
        <button className="primary" onClick={closeDialog}>
          {t("Done")}
        </button>
      </>
    ) : (
      <>
        <p>
          {t(
            "In ChatGPT on the device you control from, open remote control and enter this code. Enter it on one device only.",
          )}
        </p>
        <div className="pair-code">{dialog.code}</div>
        <p className="hint">
          {t(
            "Expires at {0}. This dialog updates once the device has paired; the code is not shown again after closing.",
            [time(dialog.expires)],
          )}
        </p>
        <button onClick={closeDialog}>{t("Close")}</button>
      </>
    );
  } else if (dialog.kind === "pair-in") {
    modalTitle = t("Pair device again");
    modalContent = (
      <>
        <p className="warning">
          {t(
            "This replaces the current device, {0}: its access is removed first, then a new code is shown. Use this when the device was changed, reset or lost.",
            [activeDevice?.paired_device?.name ?? t("Unnamed device")],
          )}
        </p>
        <div className="actions">
          <button onClick={closeDialog}>{t("Cancel")}</button>
          <button
            className="primary"
            onClick={() =>
              showPairingCode(
                { api, perform, setDialog },
                dialog.device,
                t("Pair device again"),
              )
            }
          >
            {t("Remove and pair")}
          </button>
        </div>
      </>
    );
  } else if (dialog.kind === "pair-out") {
    const again = !!activeDevice?.target;
    modalTitle = again ? t("Pair computer again") : t("Pair computer");
    modalContent = (
      <form
        onSubmit={(e) =>
          submit(e, (form) => {
            e.currentTarget.reset();
            void perform(t("Pair computer"), async () => {
              const result = await api<
                Reply<{ environment_id: string; name: string | null }>
              >(`/outputs/${dialog.device}/pair`, "POST", {
                code: form.get("code"),
              });
              setDialog(null);
              return t("Paired with {0}.", [
                result.value?.name ?? t("the computer"),
              ]);
            });
          })
        }
      >
        <p>
          {t(
            "On the computer, turn on remote control in Codex and show its pairing code. Enter it here.",
          )}
        </p>
        {again && (
          <p className="warning">
            {t(
              "This replaces the current computer, {0}: its access is removed first, then the new code is paired. If the connection is running, it stops and resumes afterwards.",
              [activeDevice?.target_name ?? t("Unnamed computer")],
            )}
          </p>
        )}
        <label>
          {t("Computer pairing code")}
          <input
            name="code"
            required
            pattern="[A-Za-z0-9]{4}(-| )?[A-Za-z0-9]{4}"
            maxLength={9}
            placeholder="ABCD-EFGH"
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <button className="primary">
          {again ? t("Replace and pair") : t("Pair computer")}
        </button>
      </form>
    );
  } else if (dialog.kind === "connect") {
    const fromController = !!dialog.input;
    const choices = fromController ? deviceChoices() : controllerChoices();
    modalTitle = t("Connect");
    modalContent = (
      <form
        onSubmit={(e) =>
          submit(
            e,
            () =>
              void perform(t("Connect"), async () => {
                if (!chosen) throw new Error(t("Choose one from the list"));
                const input = dialog.input ?? chosen;
                await api(`/inputs/${input}/binding`, "POST", {
                  output: dialog.output ?? chosen,
                });
                await api(`/inputs/${input}/start`, "POST");
                setDialog(null);
                return t(
                  "Connected. Open the remote device in ChatGPT on a paired device.",
                );
              }),
          )
        }
      >
        <p>
          {fromController ? t("Controller: ") : t("Controlled device: ")}
          <strong>{activeDevice?.endpoint.display_name}</strong>
        </p>
        <label>
          {fromController ? t("Controlled device") : t("Controller")}
          <select
            name="peer"
            required
            value={chosen}
            onChange={(event) => setSelected(event.target.value)}
          >
            <option value="" disabled>
              {t("Choose one from the list")}
            </option>
            {choices.map(option)}
          </select>
        </label>
        {!choices.length && (
          <p className="hint">
            {fromController
              ? t(
                  "No controlled device is free. Add one, or disconnect another first; each serves one controller at a time.",
                )
              : t(
                  "No controller is free. Add one, or disconnect another first.",
                )}
          </p>
        )}
        <button className="primary" disabled={!chosen}>
          {t("Connect and start")}
        </button>
      </form>
    );
  } else if (dialog.kind === "switch") {
    const choices = deviceChoices(dialog.device);
    modalTitle = t("Switch device");
    modalContent = (
      <form
        onSubmit={(e) =>
          submit(
            e,
            () =>
              void perform(t("Switch device"), async () => {
                if (!chosen) throw new Error(t("Choose one from the list"));
                await api(`/inputs/${dialog.device}/switch`, "POST", {
                  output: chosen,
                });
                setDialog(null);
                return t(
                  "Switched. Reopen the remote device in ChatGPT on a paired device.",
                );
              }),
          )
        }
      >
        <p>
          {t("Controller: ")}
          <strong>{activeDevice?.endpoint.display_name}</strong>
        </p>
        <label>
          {t("Switch to")}
          <select
            name="output"
            required
            value={chosen}
            onChange={(event) => setSelected(event.target.value)}
          >
            <option value="" disabled>
              {t("Choose one from the list")}
            </option>
            {choices.map(option)}
          </select>
        </label>
        {!choices.length && (
          <p className="hint">
            {t(
              "No other controlled device is free. Each serves one controller at a time.",
            )}
          </p>
        )}
        <p className="warning">
          {t(
            "Switching disconnects the current computer before connecting the new one. Running tasks continue. You may need to reopen the device in ChatGPT. With ChatGPT Desktop, use a separate controller for each computer.",
          )}
        </p>
        <button className="primary" disabled={!chosen}>
          {t("Disconnect and switch")}
        </button>
      </form>
    );
  } else if (dialog.kind === "restore-record") {
    modalTitle = t("Restore registration for {0}", [
      activeDevice?.endpoint.display_name ?? t("controlled device"),
    ]);
    modalContent = (
      <form
        onSubmit={(e) =>
          submit(
            e,
            (form) =>
              void perform(t("Verify original registration"), async () => {
                await api(`/outputs/${dialog.device}/restore-record`, "POST", {
                  client_id: String(form.get("client_id")).trim(),
                });
                setDialog(null);
                return activeDevice?.lifecycle === "deleting"
                  ? t(
                      "Original identity verified and cleanup record restored. The controlled device stays stopped. Retry cleanup.",
                    )
                  : t(
                      "Original identity verified and registration restored. Recover authorization, or delete the controlled device to clean up.",
                    );
              }),
          )
        }
      >
        <p>
          {t(
            "Use the original client ID from this controlled device's backup. The account and public key must match before the record is saved.",
          )}
        </p>
        <label>
          {t("Original registration client ID")}
          <input
            name="client_id"
            required
            maxLength={4096}
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <p className="hint">
          {t(
            "This is the original service registration ID. Ask the administrator for a backup containing it if it is missing.",
          )}
        </p>
        <p className="warning">
          {t(
            "Restoring this record leaves the controlled device stopped. It does not change its identity or affect other devices.",
          )}
        </p>
        <button className="primary">{t("Verify and restore record")}</button>
      </form>
    );
  } else if (dialog.kind === "unbind") {
    modalTitle = t("Disconnect");
    modalContent = (
      <>
        <p>
          {t(
            "This stops the connection. The controller keeps its paired devices and the controlled device keeps its computer, so you can connect them again later. Running tasks continue.",
          )}
        </p>
        <button
          className="danger"
          onClick={() =>
            void perform(t("Disconnect"), async () => {
              await api(`/inputs/${dialog.device}/binding`, "DELETE");
              setDialog(null);
              return t("Disconnected. Both sides are kept.");
            })
          }
        >
          {t("Stop and disconnect")}
        </button>
      </>
    );
  } else if (dialog.kind === "delete") {
    const controller = activeDevice?.endpoint.kind === "in";
    modalTitle = t("Delete {0}", [activeDevice?.endpoint.display_name ?? ""]);
    modalContent = (
      <>
        <p>
          {controller
            ? t(
                "This stops its connection and revokes every paired device's access. The account sign-in and the controlled device are kept.",
              )
            : t(
                "This stops its connection and revokes its access to the computer. The account sign-in and the controller are kept.",
              )}
        </p>
        <p className="warning">
          {t(
            "If revocation fails, it stays stopped pending cleanup. Its name can't be reused until cleanup finishes.",
          )}
        </p>
        <button
          className="danger"
          onClick={() =>
            void perform(t("Delete"), async () => {
              const result = await api<{ status: string }>(
                `/endpoints/${dialog.device}`,
                "DELETE",
              );
              setDialog(null);
              return result.status === "complete"
                ? t("Access revoked and deleted.")
                : t(
                    "Stopped, but access revocation is incomplete. Retry cleanup on its page.",
                  );
            })
          }
        >
          {t("Stop and delete")}
        </button>
      </>
    );
  }

  return (
    <Modal
      title={modalTitle}
      busy={!!busy}
      disabled={!fresh}
      message={busy ? t("{0}…", [busy]) : notice}
      close={closeDialog}
    >
      {modalContent}
    </Modal>
  );
}
