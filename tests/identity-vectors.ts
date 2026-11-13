// Independent reference: Bun WebCrypto HKDF + OpenSSL EC import. Synthetic seed only.
const encoder = new TextEncoder();
const cat = (...parts: Uint8Array[]) => {
  const bytes = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let offset = 0;
  for (const part of parts) {
    bytes.set(part, offset);
    offset += part.length;
  }
  return bytes;
};
const u32 = (n: number) => {
  const b = new Uint8Array(4);
  new DataView(b.buffer).setUint32(0, n);
  return b;
};
const field = (v: Uint8Array) => cat(u32(v.length), v);
const seed = new Uint8Array(32).fill(7);
const tenant = new Uint8Array(16);
tenant[15] = 1;
const context = cat(
  ...[
    tenant,
    encoder.encode("workspace-example"),
    encoder.encode("user-example"),
    encoder.encode("desk"),
  ].map(field),
);
const key = await crypto.subtle.importKey("raw", seed, "HKDF", false, [
  "deriveBits",
]);
async function derive(label: string, length: number, counter?: number) {
  const info = cat(
    field(encoder.encode(label)),
    context,
    ...(counter === undefined ? [] : [u32(counter)]),
  );
  return new Uint8Array(
    await crypto.subtle.deriveBits(
      {
        name: "HKDF",
        hash: "SHA-256",
        salt: encoder.encode("codex-rc-bridge/identity"),
        info,
      },
      key,
      length * 8,
    ),
  );
}
const hex = (bytes: Uint8Array) =>
  Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
async function id(label: string) {
  const b = await derive(label, 16);
  b[6] = (b[6] & 15) | 128;
  b[8] = (b[8] & 63) | 128;
  const s = hex(b);
  return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20)}`;
}
const order = BigInt(
  "0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
);
let scalar: Uint8Array;
for (let counter = 0; ; counter++) {
  scalar = await derive("out/signing", 32, counter);
  const n = BigInt("0x" + hex(scalar));
  if (n > 0n && n < order) break;
}
const der = cat(
  new Uint8Array([0x30, 0x31, 2, 1, 1, 4, 32]),
  scalar,
  new Uint8Array([0xa0, 10, 6, 8, 0x2a, 0x86, 0x48, 0xce, 0x3d, 3, 1, 7]),
);
const result = Bun.spawnSync(
  ["openssl", "ec", "-inform", "DER", "-pubout", "-outform", "DER"],
  { stdin: der, stdout: "pipe", stderr: "pipe" },
);
if (result.exitCode !== 0)
  throw new Error("OpenSSL reference key import failed");
const vector = {
  seed_hex: hex(seed),
  tenant_id: "00000000-0000-0000-0000-000000000001",
  workspace_id: "workspace-example",
  user_id: "user-example",
  name: "desk",
  in_id: await id("in/id"),
  out_id: await id("out/id"),
  public_key_hex: hex(result.stdout.slice(-65)),
};
const path = new URL("./fixtures/identity.json", import.meta.url);
if (Bun.argv.includes("--generate"))
  await Bun.write(path, JSON.stringify(vector, null, 2) + "\n");
else if (JSON.stringify(await Bun.file(path).json()) !== JSON.stringify(vector))
  throw new Error("Identity compatibility vector changed");
console.log("Independent identity vector verified");

// Desktop device identity hashes the ordered camelCase identity, not the enrollment body.
const hashUrl = async (bytes: Uint8Array) =>
  Buffer.from(await crypto.subtle.digest("SHA-256", bytes)).toString(
    "base64url",
  );
const keyId = await hashUrl(result.stdout);
const deviceIdentity = {
  algorithm: "ecdsa_p256_sha256",
  keyId,
  protectionClass: "os_protected_nonextractable",
  publicKeySpkiDerBase64: result.stdout.toString("base64"),
};
if (
  keyId !== "DyYm9YYvBNO3c3lVz17Okfg348xMJ9GKIohEcHJ5Qh8" ||
  (await hashUrl(encoder.encode(JSON.stringify(deviceIdentity)))) !==
    "egDP_xDz292C7bQUh45gzOO58C7yckQNszYOPpqkG-M"
)
  throw new Error("Desktop device identity compatibility vector changed");
console.log("Independent device identity hash verified");
