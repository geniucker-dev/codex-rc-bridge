import {
  chmod,
  lstat,
  mkdir,
  mkdtemp,
  readdir,
  rename,
  rm,
} from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { checkVersion } from "./check-version";

const files = [
  ["target/release/bridge-server", "bin/bridge-server"],
  ["target/release/bridge-admin", "bin/bridge-admin"],
  ["ui/dist", "ui"],
  ["deploy/codex-rc-bridge.service", "deploy/codex-rc-bridge.service"],
  ["deploy/server.env.example", "deploy/server.env.example"],
  ["deploy/Caddyfile.example", "deploy/Caddyfile.example"],
  ["deploy/nginx.conf.example", "deploy/nginx.conf.example"],
  // Operators get the human guides, not the agent working notes.
  ["LICENSE", "LICENSE"],
  ["README.md", "README.md"],
  ["README_zh.md", "README_zh.md"],
  ["docs/guide/server.md", "docs/guide/server.md"],
  ["docs/guide/server.zh.md", "docs/guide/server.zh.md"],
  // The READMEs link to these, so ship them to keep every link working.
  ["docs/guide/desktop.md", "docs/guide/desktop.md"],
  ["docs/guide/desktop.zh.md", "docs/guide/desktop.zh.md"],
  ["docs/guide/development.md", "docs/guide/development.md"],
  ["docs/guide/development.zh.md", "docs/guide/development.zh.md"],
] as const;
export async function assemble(root: string, destination: string) {
  const manifest: { path: string; bytes: number; sha256: string }[] = [];
  async function copy(source: string, name: string) {
    const stat = await lstat(source);
    if (stat.isSymbolicLink())
      throw new Error("Packaging refuses symbolic links");
    if (stat.isDirectory()) {
      await mkdir(join(destination, name), { recursive: true });
      for (const entry of (await readdir(source)).sort())
        await copy(join(source, entry), `${name}/${entry}`);
      return;
    }
    if (!stat.isFile()) throw new Error("Packaging accepts regular files only");
    const bytes = await Bun.file(source).bytes();
    await Bun.write(join(destination, name), bytes);
    await chmod(
      join(destination, name),
      name.startsWith("bin/") ? 0o755 : 0o644,
    );
    manifest.push({
      path: name,
      bytes: bytes.length,
      sha256: new Bun.CryptoHasher("sha256").update(bytes).digest("hex"),
    });
  }
  for (const [source, packagedPath] of files)
    await copy(join(root, source), packagedPath);
  if (!manifest.some((file) => file.path === "ui/index.html"))
    throw new Error("Built frontend is missing index.html");
  await Bun.write(
    join(destination, "manifest.json"),
    JSON.stringify(
      { platform: process.platform, arch: process.arch, files: manifest },
      null,
      2,
    ) + "\n",
  );
  return manifest;
}
async function run(command: string[], cwd: string) {
  const child = Bun.spawn(command, {
    cwd,
    stdout: "inherit",
    stderr: "inherit",
  });
  if ((await child.exited) !== 0)
    throw new Error(`Build step failed: ${command[0]}`);
}
async function main() {
  const root = resolve(import.meta.dir, "..");
  const version = await checkVersion(root);
  if (process.platform === "win32" || process.env.CARGO_BUILD_TARGET)
    throw new Error(
      "This bundle requires a native Unix build; build Linux bundles on Linux",
    );
  await run(["bun", "run", "build"], root);
  await run(
    [
      "cargo",
      "build",
      "--release",
      "--locked",
      "--target-dir",
      join(root, "target"),
      "-p",
      "bridge-server",
      "--bins",
    ],
    root,
  );
  const name = `codex-rc-bridge-${version}-${process.platform}-${process.arch}`;
  const stage = await mkdtemp(join(tmpdir(), "bridge-package-"));
  try {
    await assemble(root, join(stage, name));
    const temporaryArchive = join(stage, `${name}.tar.gz`);
    const tar = Bun.spawn(
      ["tar", "-czf", temporaryArchive, "-C", stage, name],
      {
        env: { ...process.env, COPYFILE_DISABLE: "1" },
        stdout: "inherit",
        stderr: "inherit",
      },
    );
    if ((await tar.exited) !== 0) throw new Error("Archive creation failed");
    const output = join(root, "dist", "server");
    await mkdir(output, { recursive: true });
    const archive = join(output, `${name}.tar.gz`);
    // Copy before rename: system temporary storage may live on a different filesystem.
    const partial = join(output, `${name}.${crypto.randomUUID()}.partial`);
    await Bun.write(partial, Bun.file(temporaryArchive));
    await rename(partial, archive);
    const hash = new Bun.CryptoHasher("sha256")
      .update(await Bun.file(archive).bytes())
      .digest("hex");
    await Bun.write(`${archive}.sha256`, `${hash}  ${name}.tar.gz\n`);
    console.log(`Created ${archive}`);
    console.log(
      "Includes binaries, web assets and deployment examples. No account state or keys are bundled.",
    );
  } finally {
    await rm(stage, { recursive: true, force: true });
  }
}
if (import.meta.main) await main();
