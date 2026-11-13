import { useEffect, useState, type ReactNode } from "react";
import { t } from "./i18n";
import { ApiError, type User } from "./api";
import { MoreMenu } from "./MoreMenu";
import { submit, type Api, type Perform } from "./ui-types";

export type UserDialog =
  | { kind: "add-user"; done: () => void }
  | { kind: "reset-password"; user: User; done: () => void }
  | { kind: "delete-user"; user: User; done: () => void }
  | { kind: "change-password" };

// Everyone on the deployment. Administrators are listed but only managed from
// the server's command line, so their rows carry no actions.
export function UsersPage({
  api,
  refreshed,
  open,
}: {
  api: Api;
  // Bumped by "Refresh status": other browsers may have changed the users.
  refreshed: number;
  open: (dialog: UserDialog) => void;
}) {
  const [users, setUsers] = useState<User[] | null>(null);
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);
  const done = () => setReload((n) => n + 1);
  useEffect(() => {
    let active = true;
    void api<{ users: User[] }>("/users")
      .then((value) => {
        if (!active) return;
        setUsers(value.users);
        setError("");
      })
      .catch((e) => {
        if (active) setError(e.message);
      });
    return () => {
      active = false;
    };
  }, [reload, refreshed]);
  if (error)
    return (
      <div role="alert">
        <p>{error}</p>
        <button onClick={done}>{t("Reload users")}</button>
      </div>
    );
  if (!users) return <p role="status">{t("Loading users…")}</p>;
  const sorted = [...users].sort(
    (a, b) =>
      Number(b.deployment_admin) - Number(a.deployment_admin) ||
      a.username.localeCompare(b.username),
  );
  return (
    <section className="users">
      <div className="toolbar">
        <span>{t("Users: {0}", [users.length])}</span>
        <div className="actions">
          <button
            className="primary"
            onClick={() => open({ kind: "add-user", done })}
          >
            {t("Add user")}
          </button>
        </div>
      </div>
      <ul className="user-list">
        {sorted.map((user) => (
          <li
            className={`user-row${user.locked ? " needs-attention" : ""}`}
            key={user.id}
          >
            <div>
              <h2 className="id-text">
                {user.username}
                {user.current && <span className="user-tag">{t("You")}</span>}
              </h2>
              <p className="hint">
                {user.deployment_admin
                  ? t("Administrator. Manage on the server's command line.")
                  : t("Has their own accounts and devices.")}
              </p>
              {user.locked && (
                <p className="account-alert">
                  {t(
                    "Sign-in is locked for a minute after too many wrong passwords. Resetting the password unlocks it.",
                  )}
                </p>
              )}
            </div>
            {!user.deployment_admin && (
              <div className="actions">
                <button
                  className={user.locked ? "primary" : ""}
                  onClick={() => open({ kind: "reset-password", user, done })}
                >
                  {t("Reset password")}
                </button>
                <MoreMenu>
                  <button
                    className="danger-text"
                    onClick={() => open({ kind: "delete-user", user, done })}
                  >
                    {t("Delete user")}
                  </button>
                </MoreMenu>
              </div>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

// A new password typed twice; mismatches are caught before anything is sent.
function newPassword(form: FormData) {
  const password = String(form.get("password") ?? "");
  if (password !== form.get("confirm"))
    throw new Error(t("The two new passwords don't match."));
  return password;
}
function PasswordFields({ label }: { label: string }) {
  return (
    <>
      <label>
        {label}
        <input
          name="password"
          type="password"
          autoComplete="new-password"
          required
          minLength={12}
        />
      </label>
      <p className="hint">{t("At least 12 characters.")}</p>
      <label>
        {t("Type it again")}
        <input
          name="confirm"
          type="password"
          autoComplete="new-password"
          required
        />
      </label>
    </>
  );
}

// A user the admin acted on may already be gone; refresh the list then.
async function onUser<T>(
  run: () => Promise<T>,
  dialog: { done: () => void },
  close: () => void,
) {
  try {
    return await run();
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      close();
      dialog.done();
    }
    throw error;
  }
}

function DeleteUser({
  dialog,
  api,
  perform,
  close,
}: {
  dialog: Extract<UserDialog, { kind: "delete-user" }>;
  api: Api;
  perform: Perform;
  close: () => void;
}) {
  const [typed, setTyped] = useState("");
  const name = dialog.user.username;
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (typed !== name) return;
        void perform(
          t("Delete user"),
          async () => {
            await onUser(
              () => api(`/users/${dialog.user.id}`, "DELETE"),
              dialog,
              close,
            );
            close();
            dialog.done();
            return t("Deleted {0} and all of their data.", [name]);
          },
          false,
        );
      }}
    >
      <p>
        {t(
          "This signs {0} out and deletes their ChatGPT sign-ins, controllers, controlled devices and connections from this server. Their connections stop. It can't be undone.",
          [name],
        )}
      </p>
      <p className="warning">
        {t(
          "Nothing is revoked with ChatGPT, so their ChatGPT apps may still list the old remote devices. Those no longer work and can be removed there.",
        )}
      </p>
      <label>
        {t("Type {0} to confirm", [name])}
        <input
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          autoComplete="off"
          autoCapitalize="none"
          spellCheck={false}
        />
      </label>
      <button className="danger" disabled={typed !== name}>
        {t("Delete user")}
      </button>
    </form>
  );
}

export function userDialog(
  dialog: UserDialog,
  api: Api,
  perform: Perform,
  close: () => void,
): [string, ReactNode] {
  if (dialog.kind === "add-user")
    return [
      t("Add user"),
      <form
        onSubmit={(e) =>
          submit(e, (form) => {
            const username = String(form.get("username") ?? "");
            void perform(
              t("Add user"),
              async () => {
                await api("/users", "POST", {
                  username,
                  password: newPassword(form),
                });
                close();
                dialog.done();
                return t(
                  "Added {0}. Give them the username and password to sign in.",
                  [username],
                );
              },
              false,
            );
          })
        }
      >
        <p>
          {t(
            "The new user gets a separate space with their own ChatGPT accounts and devices. They can change the password after signing in.",
          )}
        </p>
        <label>
          {t("Username")}
          <input
            name="username"
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            required
            maxLength={64}
            pattern="[a-z0-9_.\-]+"
          />
        </label>
        <p className="hint">
          {t("Lowercase letters, digits, underscores, dots and hyphens.")}
        </p>
        <PasswordFields label={t("Password")} />
        <button className="primary">{t("Add user")}</button>
      </form>,
    ];
  if (dialog.kind === "reset-password") {
    const name = dialog.user.username;
    return [
      t("Reset password for {0}", [name]),
      <form
        onSubmit={(e) =>
          submit(
            e,
            (form) =>
              void perform(
                t("Reset password"),
                async () => {
                  const password = newPassword(form);
                  await onUser(
                    () =>
                      api(`/users/${dialog.user.id}/password`, "POST", {
                        password,
                      }),
                    dialog,
                    close,
                  );
                  close();
                  dialog.done();
                  return t(
                    "Password reset. {0} has been signed out everywhere.",
                    [name],
                  );
                },
                false,
              ),
          )
        }
      >
        <p>
          {t(
            "{0} is signed out on every browser and needs the new password to sign in. Their connections keep running.",
            [name],
          )}
        </p>
        <PasswordFields label={t("New password")} />
        <button className="primary">{t("Reset password")}</button>
      </form>,
    ];
  }
  if (dialog.kind === "delete-user")
    return [
      t("Delete {0}", [dialog.user.username]),
      <DeleteUser dialog={dialog} api={api} perform={perform} close={close} />,
    ];
  return [
    t("Change password"),
    <form
      onSubmit={(e) =>
        submit(
          e,
          (form) =>
            void perform(
              t("Change password"),
              async () => {
                await api("/session/password", "POST", {
                  current: form.get("current"),
                  password: newPassword(form),
                });
                close();
                return t(
                  "Password changed. Other browsers signed in as you have been signed out.",
                );
              },
              false,
            ),
        )
      }
    >
      <p>
        {t(
          "You stay signed in here. Other browsers signed in as you are signed out.",
        )}
      </p>
      <label>
        {t("Current password")}
        <input
          name="current"
          type="password"
          autoComplete="current-password"
          required
        />
      </label>
      <PasswordFields label={t("New password")} />
      <button className="primary">{t("Change password")}</button>
    </form>,
  ];
}
