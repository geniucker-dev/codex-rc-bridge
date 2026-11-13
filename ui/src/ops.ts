import type { Account, EndpointInfo, Reply } from "./api";
import type { Api, Perform } from "./ui-types";
import type { Dialog } from "./OperationDialog";
import type { View } from "./navigation";
import { t } from "./i18n";

// Everything a page needs to act: requests, feedback, dialogs and navigation.
export type Ops = {
  api: Api;
  perform: Perform;
  setDialog: (dialog: Dialog | null) => void;
  setNotice: (notice: string) => void;
  go: (view: View) => void;
  authorize: (output?: string, expected?: Account, next?: Dialog) => void;
};

export const open = (ops: Ops, dialog: Dialog) => {
  ops.setNotice("");
  ops.setDialog(dialog);
};

// Ask the backend to check each end's device or computer against the service. It
// skips recent checks itself and announces changes, so callers neither wait nor time it.
export const syncDevices = (api: Ops["api"], endpoints: EndpointInfo[]) => {
  for (const info of endpoints) {
    if (!info.registered || info.lifecycle === "deleting") continue;
    const id = info.endpoint.device_id;
    if (info.endpoint.kind === "in")
      void api(`/inputs/${id}/device/sync`, "POST").catch(() => {});
    else if (info.target)
      void api(`/outputs/${id}/device/sync`, "POST").catch(() => {});
  }
};

// A controller's pairing code also retries an unfinished registration. Pairing
// again removes the current device first, so that case asks before acting.
export const pairController = (ops: Ops, info: EndpointInfo, label: string) =>
  info.paired_device
    ? open(ops, { kind: "pair-in", device: info.endpoint.device_id })
    : showPairingCode(ops, info.endpoint.device_id, label);

export const showPairingCode = (
  ops: Pick<Ops, "api" | "perform" | "setDialog">,
  input: string,
  label: string,
) =>
  void ops.perform(label, async () => {
    const result = await ops.api<Reply<{ code: string; expires_at: number }>>(
      `/inputs/${input}/pair`,
      "POST",
    );
    ops.setDialog({
      kind: "pair-code",
      device: input,
      code: result.value.code,
      expires: result.value.expires_at,
    });
    return t("Pairing code generated.");
  });
