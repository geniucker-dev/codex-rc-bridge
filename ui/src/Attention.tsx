import { t } from "./i18n";
import {
  accountLabel,
  operationMessage,
  sameAccount,
  type AccountInfo,
  type Chain,
  type EndpointInfo,
} from "./api";
import { open, pairController, showPairingCode, type Ops } from "./ops";

type Issue =
  | "deleting"
  | "unregistered"
  | "uncertain"
  | "sign-in"
  | "device-authorization"
  | "controller-registration"
  | "access-denied"
  | "unauthorized"
  | "no-computer"
  | "no-device"
  | "computer-changed"
  | "pairing-pending";

// A problem the stopped connection reported for this end.
function chainIssue(info: EndpointInfo, chain?: Chain): Issue | null {
  if (!chain || chain.status !== "failed") return null;
  const side = info.endpoint.kind === "in" ? "controller" : "device";
  if (chain.side !== side) return null;
  switch (chain.connection) {
    case "sign_in_required":
      return "sign-in";
    case "device_authorization_required":
      return "device-authorization";
    case "controller_registration_required":
      return "controller-registration";
    case "access_denied":
      return "access-denied";
    default:
      return null;
  }
}

// What stands between an endpoint and normal use. One issue at a time, the one
// whose fix unblocks the rest first.
export function endpointIssue(
  info: EndpointInfo,
  accounts: AccountInfo[],
  chain?: Chain,
): Issue | null {
  const controller = info.endpoint.kind === "in";
  const fromChain = chainIssue(info, chain);
  const account = accounts.find((a) => sameAccount(a, info.endpoint.account));
  // Every other fix needs a working sign-in, so this one comes first.
  if (fromChain === "sign-in" || account?.sign_in_required) return "sign-in";
  if (info.lifecycle === "deleting") return "deleting";
  if (controller && !info.registered) return "unregistered";
  if (!controller && info.registration_uncertain) return "uncertain";
  if (fromChain) return fromChain;
  if (controller)
    return info.pairing_error
      ? "pairing-pending"
      : info.paired_device
        ? null
        : "no-device";
  if (!info.registered) return "unauthorized";
  if (!info.target) return "no-computer";
  if (info.pairing_pending) return "pairing-pending";
  if (info.pairing_error) return "computer-changed";
  return null;
}

const labels = (): Record<Issue, string> => ({
  deleting: t("Removal pending"),
  unregistered: t("Registration unfinished"),
  uncertain: t("Authorization needs confirmation"),
  "sign-in": t("Sign-in expired"),
  "device-authorization": t("Device authorization expired"),
  "controller-registration": t("Controller registration lost"),
  "access-denied": t("Access denied"),
  unauthorized: t("Authorization required"),
  "no-computer": t("No computer paired"),
  "no-device": t("No device paired"),
  "computer-changed": t("Computer unavailable"),
  "pairing-pending": t("Re-pairing unfinished"),
});

export function Attention({
  info,
  accounts,
  chain,
  ops,
  detail = false,
}: {
  info: EndpointInfo;
  accounts: AccountInfo[];
  chain?: Chain;
  ops: Ops;
  detail?: boolean;
}) {
  const issue = endpointIssue(info, accounts, chain);
  if (!issue) return null;
  const id = info.endpoint.device_id;
  const retryRegistration = (
    <button
      className="primary"
      onClick={() => showPairingCode(ops, id, t("Retry registration"))}
    >
      {t("Retry registration")}
    </button>
  );
  const signIn = (primary: boolean) => (
    <button
      className={primary ? "primary" : ""}
      onClick={() => ops.authorize(undefined, info.endpoint.account)}
    >
      {primary ? t("Sign in again") : t("Sign in to associated account")}
    </button>
  );
  const pair = (label: string) => (
    <button
      className="primary"
      onClick={() => open(ops, { kind: "pair-out", device: id })}
    >
      {label}
    </button>
  );
  // A missing record blocks both recovery and cleanup until it is restored.
  const restore = info.endpoint.kind === "out" &&
    info.registration_uncertain &&
    !info.registration_recoverable &&
    !info.registered &&
    !chain &&
    info.lifecycle !== "active" && (
      <button onClick={() => open(ops, { kind: "restore-record", device: id })}>
        {t("Restore registration record")}
      </button>
    );

  const [text, actions]: [string, React.ReactNode] = (() => {
    switch (issue) {
      case "deleting":
        return [
          t(
            "Access is still being revoked. It stays stopped until cleanup finishes; its name can't be reused until then.",
          ),
          <>
            <button
              className="primary"
              onClick={() => open(ops, { kind: "delete", device: id })}
            >
              {t("Retry cleanup")}
            </button>
            {restore}
            {signIn(false)}
          </>,
        ];
      case "unregistered":
        return [
          t(
            "Registration didn't finish. Retry it, then enter the pairing code on the device you control from.",
          ),
          retryRegistration,
        ];
      case "uncertain":
        return info.registration_recoverable
          ? [
              t(
                "The last authorization didn't return a complete result. Recover it with the original identity.",
              ),
              <>
                <button
                  className="primary"
                  onClick={() =>
                    void ops.perform(t("Recover authorization"), async () => {
                      await ops.api(`/outputs/${id}/recover`, "POST");
                      return t(
                        "Authorization recovered using the original device identity and key.",
                      );
                    })
                  }
                >
                  {t("Recover authorization")}
                </button>
                {signIn(false)}
              </>,
            ]
          : [
              t(
                "The original registration record is missing. Restore it with the client ID from a backup, or create another controlled device with a different name.",
              ),
              <>
                {restore}
                {signIn(false)}
              </>,
            ];
      case "sign-in":
        return [
          t(
            "{0} can no longer be renewed. Sign in to it again; connections using it resume afterwards.",
            [accountLabel(info.endpoint.account, accounts)],
          ),
          signIn(true),
        ];
      case "device-authorization":
        return [
          t(
            "Remote control on this device is no longer authorized. Authorize it again with the account on the computer; the connection resumes afterwards.",
          ),
          <button className="primary" onClick={() => ops.authorize(id)}>
            {t("Authorize again")}
          </button>,
        ];
      case "controller-registration":
        return [
          t(
            "This controller's registration could not be restored. Retry it, then enter the new pairing code on its device; the connection resumes afterwards.",
          ),
          retryRegistration,
        ];
      case "access-denied":
        return [
          t(
            "This side was refused access. Check its account and permissions, then retry the connection.",
          ),
          null,
        ];
      case "unauthorized":
        return [
          t(
            "Sign in with the account used on the controlled computer to allow remote control.",
          ),
          <button
            className="primary"
            onClick={() =>
              ops.authorize(id, undefined, { kind: "pair-out", device: id })
            }
          >
            {t("Authorize")}
          </button>,
        ];
      case "no-computer":
        return [
          t(
            "Turn on remote control in Codex on the computer, then enter its pairing code.",
          ),
          pair(t("Pair computer")),
        ];
      case "computer-changed":
        return [
          operationMessage({ code: info.pairing_error ?? undefined }),
          pair(t("Pair computer again")),
        ];
      case "no-device":
        return [
          t(
            "Pair the device you control from: open remote control in ChatGPT on it and enter the code.",
          ),
          <button
            className="primary"
            onClick={() => showPairingCode(ops, id, t("Pair device"))}
          >
            {t("Pair device")}
          </button>,
        ];
      case "pairing-pending":
        if (info.endpoint.kind === "in")
          return [
            operationMessage({ code: info.pairing_error ?? undefined }),
            <button
              className="primary"
              onClick={() => pairController(ops, info, t("Pair device again"))}
            >
              {t("Pair device again")}
            </button>,
          ];
        return [
          t(
            "Pairing stopped partway, so the connection can't start. Pair the computer again with a new code.",
          ),
          pair(t("Pair computer again")),
        ];
    }
  })();
  const label =
    info.pairing_error === "in_multiple_devices" && issue === "pairing-pending"
      ? t("More than one device")
      : info.pairing_error === "out_multiple_environments" &&
          issue === "computer-changed"
        ? t("More than one computer")
        : labels()[issue];
  return (
    <div className={`attention${detail ? " attention-detail" : ""}`}>
      <p>
        <strong>{label}</strong> {text}
      </p>
      {actions && <div className="actions">{actions}</div>}
    </div>
  );
}
