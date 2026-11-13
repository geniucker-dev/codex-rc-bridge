BEGIN;
CREATE TABLE IF NOT EXISTS tenants(id TEXT PRIMARY KEY NOT NULL);
CREATE TABLE IF NOT EXISTS accounts(
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    workspace_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    sign_in_revision INTEGER NOT NULL DEFAULT 0,
    sign_in_required INTEGER NOT NULL DEFAULT 0 CHECK(sign_in_required IN (0,1)),
    PRIMARY KEY(tenant_id, workspace_id, user_id)
);
CREATE TABLE IF NOT EXISTS endpoints(
    tenant_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('in','out')),
    identity_name TEXT NOT NULL,
    display_name TEXT NOT NULL,
    lifecycle TEXT NOT NULL DEFAULT 'draft' CHECK(lifecycle IN ('draft','active','deleting')),
    device_checked_at INTEGER,
    PRIMARY KEY(tenant_id,device_id),
    UNIQUE(tenant_id,device_id,kind),
    UNIQUE(tenant_id,workspace_id,user_id,kind,identity_name),
    FOREIGN KEY(tenant_id,workspace_id,user_id) REFERENCES accounts(tenant_id,workspace_id,user_id)
);
CREATE TABLE IF NOT EXISTS bindings(
    tenant_id TEXT NOT NULL,
    in_id TEXT NOT NULL,
    out_id TEXT NOT NULL,
    in_kind TEXT NOT NULL DEFAULT 'in' CHECK(in_kind='in'),
    out_kind TEXT NOT NULL DEFAULT 'out' CHECK(out_kind='out'),
    PRIMARY KEY(tenant_id,in_id),
    UNIQUE(tenant_id,out_id),
    FOREIGN KEY(tenant_id,in_id,in_kind) REFERENCES endpoints(tenant_id,device_id,kind),
    FOREIGN KEY(tenant_id,out_id,out_kind) REFERENCES endpoints(tenant_id,device_id,kind)
);
CREATE TABLE secrets (
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    scope TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    PRIMARY KEY(tenant_id, scope)
);
CREATE TABLE oauth_transactions (
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    id TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    ciphertext BLOB NOT NULL,
    PRIMARY KEY(tenant_id, id)
);
-- Retain tombstones across endpoint deletion/recreation. No payload or raw client/stream IDs.
CREATE TABLE used_streams (
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    in_id TEXT NOT NULL,
    stream_hash TEXT NOT NULL,
    PRIMARY KEY(tenant_id,in_id,stream_hash)
);
CREATE TABLE targets (
    tenant_id TEXT NOT NULL,
    out_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    app_server_version TEXT NOT NULL,
    PRIMARY KEY(tenant_id,out_id),
    FOREIGN KEY(tenant_id,out_id) REFERENCES endpoints(tenant_id,device_id)
);
CREATE TABLE host_environments (
    tenant_id TEXT NOT NULL,
    in_id TEXT NOT NULL,
    environment_id TEXT NOT NULL UNIQUE,
    PRIMARY KEY(tenant_id,in_id),
    FOREIGN KEY(tenant_id,in_id) REFERENCES endpoints(tenant_id,device_id)
);
CREATE TABLE chain_settings (
    tenant_id TEXT NOT NULL,
    in_id TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
    PRIMARY KEY(tenant_id,in_id),
    FOREIGN KEY(tenant_id,in_id) REFERENCES bindings(tenant_id,in_id) ON DELETE CASCADE
);
-- A prepared intent reserves both the existing binding and the destination.
-- Deleting this record and changing the binding happen in one commit after join.
CREATE TABLE chain_switches (
    tenant_id TEXT NOT NULL,
    in_id TEXT NOT NULL,
    old_out_id TEXT NOT NULL,
    new_out_id TEXT NOT NULL,
    out_kind TEXT NOT NULL DEFAULT 'out' CHECK(out_kind='out'),
    PRIMARY KEY(tenant_id,in_id),
    UNIQUE(tenant_id,new_out_id),
    FOREIGN KEY(tenant_id,in_id) REFERENCES bindings(tenant_id,in_id),
    FOREIGN KEY(tenant_id,new_out_id,out_kind) REFERENCES endpoints(tenant_id,device_id,kind)
);
CREATE TRIGGER binding_reserved_insert BEFORE INSERT ON bindings
WHEN EXISTS(SELECT 1 FROM chain_switches WHERE tenant_id=NEW.tenant_id AND new_out_id=NEW.out_id)
BEGIN SELECT RAISE(ABORT,'OUT reserved for switch'); END;
CREATE TRIGGER binding_reserved_update BEFORE UPDATE ON bindings
WHEN EXISTS(SELECT 1 FROM chain_switches WHERE tenant_id=OLD.tenant_id AND (in_id=OLD.in_id OR new_out_id=NEW.out_id))
BEGIN SELECT RAISE(ABORT,'switch in progress'); END;
CREATE TRIGGER binding_reserved_delete BEFORE DELETE ON bindings
WHEN EXISTS(SELECT 1 FROM chain_switches WHERE tenant_id=OLD.tenant_id AND in_id=OLD.in_id)
BEGIN SELECT RAISE(ABORT,'switch in progress'); END;
CREATE TRIGGER target_reserved_update BEFORE UPDATE ON targets
WHEN EXISTS(SELECT 1 FROM chain_switches WHERE tenant_id=OLD.tenant_id AND new_out_id=OLD.out_id)
BEGIN SELECT RAISE(ABORT,'switch in progress'); END;
CREATE TRIGGER target_reserved_delete BEFORE DELETE ON targets
WHEN EXISTS(SELECT 1 FROM chain_switches WHERE tenant_id=OLD.tenant_id AND new_out_id=OLD.out_id)
BEGIN SELECT RAISE(ABORT,'switch in progress'); END;
CREATE TABLE enrollment_attempts (
    tenant_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    PRIMARY KEY(tenant_id,device_id),
    FOREIGN KEY(tenant_id,device_id) REFERENCES endpoints(tenant_id,device_id)
);
CREATE TRIGGER binding_deleting_insert BEFORE INSERT ON bindings
WHEN EXISTS(SELECT 1 FROM endpoints WHERE tenant_id=NEW.tenant_id AND device_id IN(NEW.in_id,NEW.out_id) AND lifecycle='deleting')
BEGIN SELECT RAISE(ABORT,'endpoint deleting'); END;
CREATE TRIGGER binding_deleting_update BEFORE UPDATE ON bindings
WHEN EXISTS(SELECT 1 FROM endpoints WHERE tenant_id=NEW.tenant_id AND device_id IN(NEW.in_id,NEW.out_id) AND lifecycle='deleting')
BEGIN SELECT RAISE(ABORT,'endpoint deleting'); END;
CREATE TABLE management_users (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL UNIQUE REFERENCES tenants(id),
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    deployment_admin INTEGER NOT NULL DEFAULT 0 CHECK(deployment_admin IN (0,1)),
    failures INTEGER NOT NULL DEFAULT 0,
    locked_until INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE management_sessions (
    token_hash TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES management_users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX management_sessions_user ON management_sessions(user_id,created_at);
CREATE TABLE deployment_settings(id INTEGER PRIMARY KEY CHECK(id=1), ciphertext BLOB NOT NULL);
-- Cached computer metadata and repair hints, scoped to the selected environment.
CREATE TABLE target_records (
    tenant_id TEXT NOT NULL,
    out_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    display_name TEXT,
    online INTEGER,
    last_seen_at INTEGER,
    error TEXT,
    PRIMARY KEY(tenant_id,out_id),
    FOREIGN KEY(tenant_id,out_id) REFERENCES endpoints(tenant_id,device_id) ON DELETE CASCADE
);
-- Presence blocks runtime restoration until a complete re-pair commits its target.
CREATE TABLE IF NOT EXISTS output_pairings (
    tenant_id TEXT NOT NULL,
    out_id TEXT NOT NULL,
    PRIMARY KEY(tenant_id,out_id),
    FOREIGN KEY(tenant_id,out_id) REFERENCES endpoints(tenant_id,device_id) ON DELETE CASCADE
);
CREATE TABLE input_devices (
    tenant_id TEXT NOT NULL,
    in_id TEXT NOT NULL,
    metadata TEXT NOT NULL,
    PRIMARY KEY(tenant_id,in_id),
    FOREIGN KEY(tenant_id,in_id) REFERENCES endpoints(tenant_id,device_id) ON DELETE CASCADE
);
CREATE TABLE input_pairings (
    tenant_id TEXT NOT NULL,
    in_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    pending INTEGER NOT NULL DEFAULT 1,
    expires_at INTEGER,
    error TEXT,
    PRIMARY KEY(tenant_id,in_id),
    FOREIGN KEY(tenant_id,in_id) REFERENCES endpoints(tenant_id,device_id) ON DELETE CASCADE
);
PRAGMA user_version=14;
COMMIT;
