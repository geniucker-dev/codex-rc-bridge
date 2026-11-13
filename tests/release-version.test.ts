import { expect, test } from "bun:test";
import { validateVersion } from "../scripts/check-version";

test("ordinary builds and matching stable or prerelease tags are accepted", () => {
  expect(validateVersion("0.1.0", "0.1.0")).toBe("0.1.0");
  expect(validateVersion("0.1.0", "0.1.0", "v0.1.0")).toBe("0.1.0");
  expect(validateVersion("0.2.0-beta.1", "0.2.0-beta.1", "v0.2.0-beta.1")).toBe(
    "0.2.0-beta.1",
  );
});

test("mismatched manifests or release tags cannot label a different version", () => {
  expect(() => validateVersion("0.1.0", "0.2.0")).toThrow("differs");
  expect(() => validateVersion("0.1.0", "0.1.0", "v0.2.0")).toThrow(
    "must match",
  );
  expect(() => validateVersion("0.1.0", "0.1.0", "0.1.0")).toThrow(
    "must match",
  );
});
