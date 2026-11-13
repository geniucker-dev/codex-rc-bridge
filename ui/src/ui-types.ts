import type { FormEvent } from "react";
export type Page = "home" | "accounts" | "users" | "settings";
export type Api = <T>(
  path: string,
  method?: string,
  body?: unknown,
) => Promise<T>;
export type Perform = (
  label: string,
  action: () => Promise<string | void>,
  refresh?: boolean,
) => Promise<void>;
export const submit = (
  e: FormEvent<HTMLFormElement>,
  action: (form: FormData) => void,
) => {
  e.preventDefault();
  action(new FormData(e.currentTarget));
};
