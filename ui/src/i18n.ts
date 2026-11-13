import { useSyncExternalStore } from "react";
import { zh } from "./locales/zh";
export type Language = "system" | "en" | "zh";
const storageKey = "bridge.language";
const listeners = new Set<() => void>();
function saved(): Language {
  try {
    const value = localStorage.getItem(storageKey);
    if (value === "en" || value === "zh") return value;
  } catch {
    /* Preferences remain available for this session. */
  }
  return "system";
}
let preference = saved();
export function resolveLocale(
  value: Language,
  languages: readonly string[],
): "en" | "zh" {
  if (value !== "system") return value;
  // The first preferred language defines the system UI language; unsupported languages use English.
  return /^zh(?:-|$)/i.test(languages[0] ?? "") ? "zh" : "en";
}
export const locale = () =>
  resolveLocale(preference, navigator.languages ?? [navigator.language]);
export function t(english: string, values: unknown[] = []): string {
  const message = locale() === "zh" ? (zh[english] ?? english) : english;
  return message.replace(/\{(\d+)\}/g, (_, index) =>
    String(values[Number(index)] ?? ""),
  );
}
function notify() {
  document.documentElement.lang = locale() === "zh" ? "zh-CN" : "en";
  listeners.forEach((listener) => listener());
}
export function setLanguage(value: Language) {
  preference = value;
  try {
    localStorage.setItem(storageKey, value);
  } catch {
    /* Session preference still works. */
  }
  notify();
}
export function useLanguage() {
  useSyncExternalStore(
    (callback) => {
      listeners.add(callback);
      return () => {
        listeners.delete(callback);
      };
    },
    () => `${preference}:${locale()}`,
  );
  return preference;
}
window.addEventListener("languagechange", notify);
window.addEventListener("storage", (event) => {
  if (event.key === storageKey) {
    preference = saved();
    notify();
  }
});
notify();
