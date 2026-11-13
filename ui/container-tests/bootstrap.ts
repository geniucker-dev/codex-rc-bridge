// Synthetic test fixture only. Never mounted into the deployed server or an existing volume.
import { Database } from "bun:sqlite";
const tenant = Bun.argv[2];
if (!/^[0-9a-f-]{36}$/.test(tenant ?? ""))
  throw new Error("Missing test tenant");
const db = new Database("/data/bridge.sqlite", {
  readwrite: true,
  create: false,
});
db.exec("PRAGMA foreign_keys=ON");
const hash = await Bun.password.hash("container-synthetic-password", {
  algorithm: "argon2id",
});
db.query(
  "INSERT INTO management_users(id,tenant_id,username,password_hash) VALUES(?,?,?,?)",
).run(crypto.randomUUID(), tenant, "container-demo", hash);
db.close();
