import { t } from "./i18n";
import type { Chain, ConnectionStatus } from "./api";
import type { View } from "./navigation";

export type Span = "live" | "pending" | "offline" | "fault" | "idle";
export type RouteState = ConnectionStatus | "switching";

export const routeState = (chain: Chain): RouteState =>
  chain.status === "switching" ? "switching" : chain.connection;

// The cable between a controller and its controlled device carries the state:
// solid when connected, dashed while connecting, dotted when the computer is
// offline, broken on a fault, hairline when stopped. Text always says the same.
export const routeTone = (state: RouteState): Span => {
  switch (state) {
    case "connected":
      return "live";
    case "connecting":
    case "reconnecting":
    case "switching":
    case "checking_target":
      return "pending";
    case "target_offline":
      return "offline";
    case "stopped":
      return "idle";
    default:
      return "fault";
  }
};

export function connectionCopy(state: RouteState): [string, string] {
  const copy: Record<RouteState, [string, string]> = {
    checking_target: [
      t("Checking controlled device"),
      t(
        "Checking whether the controlled device is online. The bridge will connect automatically once confirmed.",
      ),
    ],
    target_offline: [
      t("Controlled device offline"),
      t(
        "Open Codex on the controlled device and enable remote control. The bridge will reconnect when it is online.",
      ),
    ],
    connecting: [
      t("Connecting"),
      t("The controlled device is online. Connecting both sides."),
    ],
    connected: [
      t("Connected"),
      t(
        "The connection is ready. Open the remote device in ChatGPT on your controller.",
      ),
    ],
    reconnecting: [
      t("Reconnecting"),
      t(
        "The connection failed temporarily. Retrying automatically; the connection is kept.",
      ),
    ],
    stopped: [
      t("Stopped"),
      t("The connection is kept. Start it to reconnect."),
    ],
    sign_in_required: [
      t("Sign-in expired"),
      t(
        "The account on the marked side can no longer be renewed. Sign in to it again and the connection resumes.",
      ),
    ],
    device_authorization_required: [
      t("Device authorization expired"),
      t(
        "The controlled device's remote-control authorization is no longer valid. Authorize it again and the connection resumes.",
      ),
    ],
    controller_registration_required: [
      t("Controller registration lost"),
      t(
        "The controller's registration could not be restored. Retry it, pair your devices again, and the connection resumes.",
      ),
    ],
    access_denied: [
      t("Access denied"),
      t("The marked side was refused access. Fix it there, then retry."),
    ],
    invalid_state: [
      t("Configuration needs attention"),
      t(
        "The controller or controlled device setup is incomplete. Check both, then retry.",
      ),
    ],
    protocol_error: [
      t("Connection verification failed"),
      t(
        "Unexpected protocol data or device proof stopped the connection. Retry; check diagnostic logs if this continues.",
      ),
    ],
    failed: [
      t("Connection stopped"),
      t("Retry. Check diagnostic logs if this continues."),
    ],
    switching: [
      t("Switching"),
      t(
        "Disconnecting the old controlled device and connecting the new one. You may need to reopen the remote device in ChatGPT.",
      ),
    ],
  };
  return copy[state] ?? copy.failed;
}

// These stop until fixed on the marked side, then resume on their own.
export const fixedOnSide = (state: RouteState) =>
  state === "sign_in_required" ||
  state === "device_authorization_required" ||
  state === "controller_registration_required";

// The page of the end that needs attention; the fix itself is on its card.
export function fixAction(chain: Chain): [string, View] | null {
  if (chain.status !== "failed" || !chain.side) return null;
  return chain.side === "controller"
    ? [t("Check controller"), { page: "home", controller: chain.binding.in_id }]
    : [
        t("Check controlled device"),
        { page: "home", device: chain.binding.out_id },
      ];
}

export const presenceText = (state: RouteState) =>
  state === "connected"
    ? t("Online")
    : state === "target_offline"
      ? t("Offline")
      : state === "checking_target"
        ? t("Checking")
        : null;

// The drawn cable. Purely visual: the state is always also written next to it.
export function Cable({ tone }: { tone: Span }) {
  return (
    <span className={`cable cable-${tone}`} aria-hidden="true">
      <span className="jack" />
      <span className={`span span-${tone}`} />
      <span className="jack" />
    </span>
  );
}
