import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { t } from "./i18n";
export const desktop = isTauri();
export const desktopRequest = <T>(
  path: string,
  method: string,
  body?: unknown,
) => invoke<T>("desktop_request", { path, method, body });
export const openAuthorization = (url: string) =>
  invoke<void>("desktop_open_authorization", { url }).catch(
    (error: unknown) => {
      throw new Error(
        t(
          typeof error === "string"
            ? error
            : "Cannot open the system browser. Copy the authorization link instead.",
        ),
      );
    },
  );
export const listenDesktop = (
  event: "bridge-changed" | "desktop-exiting",
  callback: () => void,
) => listen(event, callback);

export const setDesktopLanguage = (language: string) =>
  invoke<void>("desktop_set_language", { language });
