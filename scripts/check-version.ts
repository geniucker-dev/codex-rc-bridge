import { resolve } from "node:path";

export function validateVersion(workspace: string, app: string, tag = "") {
  if (workspace !== app)
    throw new Error(
      `Workspace version ${workspace} differs from app version ${app}`,
    );
  if (tag && tag !== `v${workspace}`)
    throw new Error(`Release tag ${tag} must match v${workspace}`);
  return workspace;
}

export async function checkVersion(root = resolve(import.meta.dir, "..")) {
  const cargo = Bun.TOML.parse(await Bun.file(`${root}/Cargo.toml`).text()) as {
    workspace: { package: { version: string } };
  };
  const app = await Bun.file(
    `${root}/crates/bridge-desktop/tauri.conf.json`,
  ).json();
  return validateVersion(
    cargo.workspace.package.version,
    app.version,
    process.env.RELEASE_TAG,
  );
}

if (import.meta.main) console.log(await checkVersion());
