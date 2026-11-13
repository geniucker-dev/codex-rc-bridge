import { t } from "./i18n";
import { desktop, desktopRequest } from "./desktop";
export type Account = { workspace_id: string; user_id: string };
export type AccountInfo = Account & {
  email?: string | null;
  name?: string | null;
  // Renewal was rejected; only signing in to this account again fixes it.
  sign_in_required: boolean;
};
export type Endpoint = {
  tenant_id: string;
  device_id: string;
  account: Account;
  kind: "in" | "out";
  identity_name: string;
  display_name: string;
};
export type EndpointInfo = {
  endpoint: Endpoint;
  lifecycle: "draft" | "active" | "deleting";
  registered: boolean;
  target: { environment_id: string; app_server_version: string } | null;
  target_name?: string | null;
  // The computer's last reported presence, kept even while no connection runs.
  target_online?: boolean | null;
  target_last_seen_at?: number | null;
  registration_uncertain: boolean;
  registration_recoverable: boolean;
  // An interrupted re-pair: the old device is gone or kept but cannot be started.
  pairing_pending?: boolean;
  // A controller's one device (phone, tablet or desktop ChatGPT); null until paired.
  paired_device?: PairedDevice | null;
  // Why the last pairing attempt left the controller without a device, as an error code.
  pairing_error?: string | null;
};
export type PairedDevice = {
  client_id: string;
  name: string | null;
  platform?: string | null;
  device_model?: string | null;
  device_type?: string | null;
  // Unix seconds of the device's last connection, when the service reports it.
  last_seen_at?: number | null;
};
export type ConnectionStatus =
  | "checking_target"
  | "target_offline"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "stopped"
  | "sign_in_required"
  | "device_authorization_required"
  | "controller_registration_required"
  | "access_denied"
  | "invalid_state"
  | "protocol_error"
  | "failed";
export type Chain = {
  binding: { in_id: string; out_id: string };
  enabled: boolean;
  status: "stopped" | "running" | "failed" | "switching";
  connection: ConnectionStatus;
  // The end that needs attention when the connection stopped on a problem.
  side: "controller" | "device" | null;
};
export type Snapshot = {
  configuration: {
    result: "snapshot";
    value: { accounts: AccountInfo[]; endpoints: EndpointInfo[] };
  };
  chains: Chain[];
};
// Users of this deployment as an administrator sees them.
export type User = {
  id: string;
  username: string;
  deployment_admin: boolean;
  // Sign-in is briefly locked after repeated wrong passwords.
  locked: boolean;
  // The user signed in here.
  current: boolean;
};
export type Session = {
  identity: {
    username: string;
    tenant_id: string;
    expires_at: number;
    deployment_admin?: boolean;
  };
  csrf: string;
};
// Web snapshots identify the exact management session that authorized this data.
export type ManagementSnapshot = Snapshot & { session: Session };
export type LoginStart = {
  id: string;
  url: string;
  expires_at: number;
  automatic_callback?: boolean;
};
export type Reply<T> = { result: string; value: T };
export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}
const messages = (): Record<string, string> => ({
  account_sign_in_required: t(
    "This account's sign-in has expired. Sign in to it again.",
  ),
  device_authorization_required: t(
    "The controlled device's remote-control authorization is no longer valid. Authorize it again.",
  ),
  out_target_missing: t(
    "This computer no longer allows this controlled device. Pair the computer again.",
  ),
  out_multiple_environments: t(
    "Another computer can also be reached through this controlled device. Pair again so only one computer remains.",
  ),
  in_multiple_devices: t(
    "More than one device can use this controller. Pair again to keep only one.",
  ),
  in_pairing_cleanup_pending: t(
    "The current device's access couldn't be fully removed yet, so no new code was issued. Try pairing again.",
  ),
  in_pairing_multiple_devices: t(
    "More than one device used the code, so all of them were removed. Pair again and enter the code on one device only.",
  ),
  out_pairing_cleanup_pending: t(
    "The previous computer's access couldn't be fully removed yet. The current computer is kept. Try pairing again.",
  ),
  out_pairing_no_environment: t(
    "The code was accepted, but no computer appeared. Check that remote control is on in Codex, then pair again with a new code.",
  ),
  out_pairing_multiple_environments: t(
    "More than one computer appeared after pairing. Pair again so only one computer remains.",
  ),
  deployment_admin_required: t(
    "Only deployment administrators can view or change this.",
  ),
  invalid_username: t(
    "Use up to 64 lowercase letters, digits, underscores, dots or hyphens for the username.",
  ),
  invalid_password: t("Use a password of at least 12 characters."),
  username_exists: t("That username is already taken. Choose another."),
  user_not_found: t("This user no longer exists. The list has been refreshed."),
  target_is_admin: t(
    "Administrators can only be managed from the command line on the server.",
  ),
  current_password_incorrect: t(
    "The current password is incorrect. Too many wrong attempts lock sign-in for a minute.",
  ),
  invalid_proxy: t(
    "Invalid proxy address. Use http://host:port without a path, query or fragment.",
  ),
  upstream_auth_network: t(
    "Cannot reach auth.openai.com. Check the server's network and proxy, then start a new authorization with a new callback.",
  ),
  upstream_auth_rejected: t(
    "The authorization service rejected the request. Check the server's proxy and authorize again.",
  ),
  upstream_auth_identity: t(
    "Credentials are missing, expired or belong to another user or workspace. Check the account and authorize again.",
  ),
  upstream_auth_transaction: t(
    "This authorization has expired, was used or no longer exists. Start again with a new callback.",
  ),
  upstream_auth_callback: t(
    "The callback does not match this authorization. Paste the complete callback URL from this attempt.",
  ),
  upstream_auth_declined: t(
    "Authorization was not approved. Start again and complete the confirmation.",
  ),
  upstream_auth_response: t(
    "The authorization response could not be read. Try again; contact your administrator if this continues.",
  ),
  out_record_identity_mismatch: t(
    "The registration could not be verified as belonging to this output. Check its original client ID.",
  ),
  login_required: t(
    "Your session expired or the username or password is incorrect. Sign in again.",
  ),
  login_rate_limited: t("Too many sign-in attempts. Try again in a minute."),
  invalid_request: t(
    "Some information is missing or invalid. Check it and try again.",
  ),
  request_origin_or_csrf_rejected: t(
    "Request verification failed. Refresh the page; if this continues, check the management URL.",
  ),
  temporarily_unavailable: t(
    "Management is temporarily unavailable. Refresh status shortly.",
  ),
  operation_failed_check_authorization_or_state: t(
    "The operation did not complete. Refresh status and check account authorization, device status and bindings.",
  ),
});
export const operationMessage = (error?: { code?: string; message?: string }) =>
  (error?.code && messages()[error.code]) ||
  (error?.message && t(error.message)) ||
  t(
    "The service could not complete the request. Refresh status and check again.",
  );
export async function request<T>(
  path: string,
  method = "GET",
  body?: unknown,
  csrf?: string,
  signal?: AbortSignal,
): Promise<T> {
  if (desktop) {
    try {
      return await desktopRequest<T>(path, method, body);
    } catch (error) {
      const failure = error as {
        status?: number;
        code?: string;
        message?: string;
      };
      throw new ApiError(failure.status ?? 0, operationMessage(failure));
    }
  }
  let response: Response;
  try {
    response = await fetch(`/api${path}`, {
      method,
      credentials: "same-origin",
      cache: "no-store",
      headers: {
        ...(body === undefined ? {} : { "content-type": "application/json" }),
        ...(csrf ? { "x-csrf-token": csrf } : {}),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.any([
        AbortSignal.timeout(method === "GET" ? 15000 : 120000),
        ...(signal ? [signal] : []),
      ]),
    });
  } catch {
    throw new ApiError(
      0,
      method === "GET"
        ? t("Cannot read service status. Check your connection and refresh.")
        : t(
            "No result was received. The service may still be processing. Refresh status before submitting again.",
          ),
    );
  }
  if (!response.ok) {
    const error = await response.json().catch(() => ({}));
    throw new ApiError(
      response.status,
      messages()[error.error] ??
        t(
          "The service could not complete the request. Refresh status and check again.",
        ),
    );
  }
  return response.status === 204 ? (undefined as T) : response.json();
}
export const sameAccount = (a: Account, b: Account) =>
  a.user_id === b.user_id && a.workspace_id === b.workspace_id;
export const accountLabel = (account: Account, accounts: AccountInfo[]) => {
  const info = accounts.find((a) => sameAccount(a, account));
  const label = info?.email || info?.name;
  if (!label)
    return t("Account {0} · Workspace {1}", [
      account.user_id,
      account.workspace_id,
    ]);
  const duplicates = accounts.filter((a) => (a.email || a.name) === label);
  return duplicates.length > 1
    ? t("{0} · Workspace {1}", [label, account.workspace_id])
    : label;
};
export const accountIdentity = ({
  workspace_id,
  user_id,
}: Account): Account => ({
  workspace_id,
  user_id,
});
