import { useEffect, useState } from "react";
import type { Page } from "./ui-types";

// Pages and the one object a detail page shows. Older links to the connection,
// input and output pages land on the overview; an old connection link opens its
// controller, which is the connection's owner.
export type View =
  | { page: Page }
  | { page: "home"; controller: string }
  | { page: "home"; device: string };
const pages: Page[] = ["home", "accounts", "users", "settings"];
const aliases: Record<string, Page> = {
  connections: "home",
  endpoints: "home",
  in: "home",
  out: "home",
};
function current(): View {
  const [head = "", id] = location.hash.replace(/^#\/?/, "").split("/");
  const key = id ? decodeURIComponent(id) : "";
  if (key && (head === "controllers" || head === "connections"))
    return { page: "home", controller: key };
  if (key && head === "devices") return { page: "home", device: key };
  const page = aliases[head] ?? (head as Page);
  return { page: pages.includes(page) ? page : "home" };
}
export const viewHash = (view: View) =>
  "controller" in view
    ? `/controllers/${encodeURIComponent(view.controller)}`
    : "device" in view
      ? `/devices/${encodeURIComponent(view.device)}`
      : `/${view.page}`;
export function useView(): [View, (view: View) => void] {
  const [value, update] = useState(current);
  // Old links keep working but show the address of the page they opened.
  useEffect(() => {
    const canonical = `#${viewHash(value)}`;
    if (location.hash && location.hash !== canonical)
      history.replaceState(history.state, "", canonical);
  }, [value]);
  useEffect(() => {
    const changed = () => update(current());
    window.addEventListener("hashchange", changed);
    return () => window.removeEventListener("hashchange", changed);
  }, []);
  return [
    value,
    (view) => {
      location.hash = viewHash(view);
      update(view);
    },
  ];
}
