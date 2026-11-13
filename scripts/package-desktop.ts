import { copyFile, mkdir, readdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { checkVersion } from "./check-version";

if (process.platform !== "darwin" || process.env.CARGO_BUILD_TARGET)
  throw new Error("Build desktop packages natively on macOS");
const root = resolve(import.meta.dir, "..");
const version = await checkVersion(root);
async function run(args: string[]) {
  const child = Bun.spawn(args, {
    cwd: root,
    env: { ...process.env, CARGO_TARGET_DIR: join(root, "target") },
    stdout: "inherit",
    stderr: "inherit",
  });
  if ((await child.exited) !== 0) throw new Error(`Failed: ${args[0]}`);
}
await run([
  "bun",
  "run",
  "desktop:build",
  "--bundles",
  "app,dmg",
  "--ci",
  "--",
  "--locked",
]);
await run([
  "codesign",
  "--verify",
  "--deep",
  "--strict",
  join(root, "target/release/bundle/macos/Codex RC Bridge.app"),
]);
const directory = join(root, "target/release/bundle/dmg");
const suffix = process.arch === "arm64" ? "aarch64" : "x64";
const source = `Codex RC Bridge_${version}_${suffix}.dmg`;
if (!(await readdir(directory)).includes(source))
  throw new Error(`Missing ${source}`);
const output = join(root, "dist/desktop");
await mkdir(output, { recursive: true });
const name = `codex-rc-bridge-${version}-macos-${process.arch}.dmg`;
const target = join(output, name);
await copyFile(join(directory, source), target);
const hash = new Bun.CryptoHasher("sha256")
  .update(await Bun.file(target).bytes())
  .digest("hex");
await Bun.write(`${target}.sha256`, `${hash}  ${name}\n`);
console.log(`Created ${target}`);
