import { test, expect } from "bun:test";
import { cp, mkdtemp, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { assemble } from "../scripts/package-server";

test("server bundle includes only release files and rejects linked assets", async () => {
  const temp = await mkdtemp(join(tmpdir(), "bridge-package-test-"));
  const root = join(temp, "repo");
  try {
    for (const name of [
      "target/release/bridge-server",
      "target/release/bridge-admin",
      "ui/dist/index.html",
      "ui/dist/assets/app.js",
      "deploy/codex-rc-bridge.service",
      "deploy/server.env.example",
      "deploy/Caddyfile.example",
      "deploy/nginx.conf.example",
      "LICENSE",
      "README.md",
      "README_zh.md",
      "docs/guide/server.md",
      "docs/guide/server.zh.md",
      "docs/guide/desktop.md",
      "docs/guide/desktop.zh.md",
      "docs/guide/development.md",
      "docs/guide/development.zh.md",
    ]) {
      await Bun.write(
        join(root, name),
        name.endsWith(".md")
          ? await Bun.file(new URL(`../${name}`, import.meta.url)).bytes()
          : `synthetic ${name}`,
      );
    }
    await cp(
      new URL("../docs/assets", import.meta.url),
      join(root, "docs/assets"),
      { recursive: true },
    );
    await Bun.write(
      join(root, "secrets/master.key"),
      "SYNTHETIC-NOT-FOR-PACKAGE",
    );
    await Bun.write(
      join(root, "local-state/state.sqlite"),
      "SYNTHETIC-PRIVATE-STATE",
    );
    const manifest = await assemble(root, join(temp, "package"));
    // Check the real guides, so adding an image cannot leave the bundle broken.
    let imageReferences = 0;
    for (const file of manifest.filter((file) => file.path.endsWith(".md"))) {
      const contents = await Bun.file(join(temp, "package", file.path)).text();
      for (const match of contents.matchAll(/!\[[^\]]*\]\(([^)]+)\)/g)) {
        imageReferences++;
        const image = join(temp, "package", dirname(file.path), match[1]!);
        expect(await Bun.file(image).exists()).toBe(true);
      }
    }
    expect(imageReferences).toBeGreaterThan(0);
    expect(
      manifest.some(
        (file) =>
          file.path.includes("secrets") || file.path.includes("local-state"),
      ),
    ).toBe(false);
    for (const file of manifest) {
      const bytes = await Bun.file(join(temp, "package", file.path)).bytes();
      expect(new TextDecoder().decode(bytes)).not.toContain(
        "SYNTHETIC-NOT-FOR-PACKAGE",
      );
      expect(new Bun.CryptoHasher("sha256").update(bytes).digest("hex")).toBe(
        file.sha256,
      );
    }
    await symlink(
      join(root, "secrets/master.key"),
      join(root, "ui/dist/assets/leaked.key"),
    );
    await expect(assemble(root, join(temp, "linked"))).rejects.toThrow(
      "symbolic links",
    );
    await rm(join(root, "ui/dist/assets/leaked.key"));
    await rm(join(root, "ui/dist/index.html"));
    await expect(assemble(root, join(temp, "incomplete"))).rejects.toThrow(
      "index.html",
    );
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
});
