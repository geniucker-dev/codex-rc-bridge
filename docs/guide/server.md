# Server deployment

English | [简体中文](server.zh.md)

The server version runs on an always-on host and is managed from a browser, including a phone's. When several people share one deployment, each gets a separate workspace (tenant) and cannot see anyone else's accounts or connections.

The bridge serves its management page and API itself, over plain HTTP on the host's loopback address. The reverse proxy you already run on the host, such as Caddy or nginx, handles the domain and HTTPS and forwards the whole site to it. You can install the bridge with Docker Compose or as a native package with systemd.

## Reverse proxy

Whatever proxy you use has to do three things:

- forward every path to the bridge, `127.0.0.1:8787` by default;
- pass the original `Host` header through, because the bridge only answers requests for the address in `BRIDGE_PUBLIC_ORIGIN`;
- stream responses without buffering, so the page's live status (server-sent events on `/api/events`) arrives as it happens.

`deploy/Caddyfile.example` and `deploy/nginx.conf.example` are ready-made configurations. Caddy passes `Host` through and streams by default, and obtains the certificate on its own. With nginx, keep `proxy_set_header Host $http_host` and `proxy_buffering off` from the example and point it at your certificate. `$http_host` preserves the public port when the address uses one, such as `https://bridge.example.com:8443`. If the host has no reverse proxy yet, installing Caddy and using the example is the shortest way to get HTTPS.

The bridge accepts plain HTTP only for a loopback address, so `BRIDGE_PUBLIC_ORIGIN` must be the `https://` address your proxy serves.

## Docker Compose

You need a host with Docker and Compose, and a reverse proxy with HTTPS for your domain as described above.

The image is published on Docker Hub as `geniucker/codex-rc-bridge` for x64 and arm64. Make a directory for the deployment and download the Compose file and configuration from the latest release:

```sh
mkdir codex-rc-bridge && cd codex-rc-bridge
curl -fsSLO https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.yaml
curl -fsSL -o .env https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.env.example
```

Run every later `docker compose` command in this directory.

Set the address your reverse proxy serves:

```dotenv
BRIDGE_PUBLIC_ORIGIN=https://bridge.example.com
```

The bridge listens on `127.0.0.1:8787`. To use another address or port, set `BRIDGE_HTTP_BIND` and `BRIDGE_HTTP_PORT` in `.env` and point the proxy there.

A release's `compose.yaml` names that release's image, so the deployment stays on its version until you upgrade.

Pull the image and initialize storage. Run `init` only on a new installation; an existing deployment refuses it.

```sh
docker compose pull
docker compose run --rm admin init
```

`init` prints a tenant UUID. Put it in place of `TENANT_UUID` below to create your management login. The first time, `add-user` asks how the bridge reaches OpenAI: press Enter for a direct connection or type an HTTP proxy address (see [Upstream proxy](#upstream-proxy)). Then it asks for the password twice; it needs at least 12 characters.

```sh
docker compose run --rm admin add-user TENANT_UUID admin
docker compose up -d server
```

Open your HTTPS domain and sign in as `admin` with that password. This login is only for the bridge's management page and has nothing to do with your ChatGPT accounts. The first management user is the deployment administrator and can set the upstream proxy in **Settings**.

The database and the encryption key live in two volumes, `bridge-data` and `bridge-key`. You need both: the database alone or the key alone cannot restore the original device identities.

### Use host directories (Linux)

The volumes work without any setup. If you would rather keep the data in directories on the host, for example so your existing backup tools can see it, create a `compose.override.yaml` next to `compose.yaml` before running `init`. Compose reads it automatically. Use two separate directories so the key never sits next to the database:

```yaml
services:
  server:
    volumes:
      - /srv/codex-rc-bridge/data:/var/lib/bridge:z
      - /srv/codex-rc-bridge/key:/run/bridge-secrets:ro,z
  admin:
    volumes:
      - /srv/codex-rc-bridge/data:/var/lib/bridge:z
      - /srv/codex-rc-bridge/key:/run/bridge-secrets:z
```

`z` lets the containers use these directories on hosts where SELinux is enforcing, such as Fedora or RHEL, by relabeling them for shared container access. Docker ignores it where SELinux is off. Because it relabels everything inside, use it only on directories that belong to the bridge.

The container runs as user and group `10001` and can't change ownership itself, so create both directories for that user, readable by nobody else:

```sh
sudo install -d -m 700 -o 10001 -g 10001 /srv/codex-rc-bridge/data /srv/codex-rc-bridge/key
```

Then continue with `init` as above. If `init` reports `Permission denied`, Docker created the directories as root because they didn't exist yet; run the `install` command again and retry.

To back up, stop the server and archive each directory separately. The archives are readable only by root:

```sh
docker compose stop --timeout -1 server
mkdir -p -m 700 ~/bridge-backup
for name in data key; do
  sudo sh -c "umask 077; tar cpf $HOME/bridge-backup/bridge-$name.tar -C /srv/codex-rc-bridge/$name ."
done
```

To restore, unpack each archive into an empty directory. `tar` running as root brings back the owner and permissions:

```sh
sudo mkdir -p /srv/codex-rc-bridge/data /srv/codex-rc-bridge/key
for name in data key; do
  sudo tar xpf ~/bridge-backup/bridge-$name.tar -C /srv/codex-rc-bridge/$name
done
```

To move an existing deployment from volumes to host directories, back up the volumes as in [Stop, upgrade and back up](#stop-upgrade-and-back-up), then unpack those two archives the same way.

On macOS, Docker Desktop and OrbStack pass host directories through a file-sharing layer that gives SQLite weaker guarantees for syncing and locking. Keep the volumes there.

### Upstream proxy

If the host needs a proxy to reach OpenAI, the deployment administrator sets it in **Settings**. The address has to be reachable from inside the container, where `127.0.0.1` is the container itself. Docker Desktop and OrbStack can reach a proxy on the host through `host.docker.internal`; on a plain Linux host, use an address the container can actually reach. The proxy applies to the bridge's traffic to the official service, not to the management page.

### Add other people

Each person gets a separate space with their own sign-in, ChatGPT accounts and devices. Nobody can see anyone else's.

As the deployment administrator, open **Users** on the management page and choose **Add user**. Pick a username and a starting password of at least 12 characters, and give both to the person. They can change the password later with **Change password** at the top of the page. The server keeps running the whole time.

If someone forgets their password, choose **Reset password** on their row. They are signed out everywhere, and their connections keep running. A reset also unlocks a sign-in that was locked after too many wrong passwords.

**Delete user** removes that person's space from the server: their ChatGPT sign-ins, controllers, controlled devices and connections. Connections that were running stop. The bridge doesn't revoke anything with ChatGPT, so their ChatGPT apps may still list the old remote devices. Those no longer work, and the person can remove them there. You can't undo a deletion. Restoring a backup from before it brings the person back, but it also rolls the rest of the deployment back to that point.

The page can't change administrator accounts, including your own. Those are managed with `bridge-admin` while the server is stopped. If you forget your own password, use the tenant UUID that `init` printed:

```sh
docker compose stop --timeout -1 server
docker compose run --rm admin reset-password TENANT_UUID admin
docker compose up -d server
```

The command line can also add people while the server is stopped: `new-tenant` prints a new tenant UUID, and `add-user NEW_TENANT_UUID alice` creates the login for it.

The deployment administrator runs the host and holds the key that protects everyone's ChatGPT sign-ins, so everyone sharing it has to trust that person. Only add people who are each connecting their own accounts.

### Stop, upgrade and back up

For planned maintenance, always stop the server like this. It waits for operations already in progress instead of being killed by Docker's default timeout.

```sh
docker compose stop --timeout -1 server
```

Then back up both volumes, keep the two archives apart, and label them as one set. With the default deployment name `codex-rc-bridge`, the volumes are `codex-rc-bridge_bridge-data` and `codex-rc-bridge_bridge-key`. The key archive is as sensitive as the key itself, so the commands write both archives readable only by you, into a private directory. They run `tar` from the image this deployment already uses:

```sh
mkdir -p -m 700 ~/bridge-backup
image=$(docker compose config --images server)
for name in data key; do
  docker run --rm --network none --user 0:0 \
    --mount type=volume,source=codex-rc-bridge_bridge-$name,target=/state,readonly \
    -v ~/bridge-backup:/backup --entrypoint sh "$image" \
    -ec "umask 077; tar cpf /backup/bridge-$name.tar -C /state ."
done
```

If you use [host directories](#use-host-directories-linux), archive those two directories instead.

To upgrade, download the new release's `compose.yaml`, then pull its image and start it. If the release notes mention new settings, compare `.env` with that release's `compose.env.example`. Do not run `init` again.

```sh
curl -fsSLO https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.yaml
docker compose pull
docker compose up -d server
```

An older version may not read a database that a newer one has written. To roll back, put back the previous release's `compose.yaml` and restore the backup you took before upgrading; changing the image alone is not enough.

To restore, create empty volumes under the original deployment name, unpack the database and key from the same backup set into them, and start the server without running `init`:

```sh
docker compose create server
image=$(docker compose config --images server)
for name in data key; do
  docker run --rm --network none --user 0:0 \
    --mount type=volume,source=codex-rc-bridge_bridge-$name,target=/state \
    -v ~/bridge-backup:/backup:ro --entrypoint sh "$image" \
    -ec "tar xpf /backup/bridge-$name.tar -C /state"
done
docker compose up -d server
```

Then check accounts, devices and connections: saved sign-ins or registrations may have expired since the backup, and the page tells you to sign in or authorize again where needed.

`docker compose down --volumes` deletes all data, including the key, so never run it on a real deployment. If one host runs several deployments, give each a fixed `-p name`, a different `BRIDGE_HTTP_PORT`, and use that same name for every later management, upgrade and backup command; the volume names then start with that name instead of `codex-rc-bridge`.

### Build the image yourself

To run your own build, build the image from the source checkout and point `BRIDGE_IMAGE` at it:

```sh
docker build --target server -t codex-rc-bridge-server:local .
```

Then set `BRIDGE_IMAGE=codex-rc-bridge-server:local` in `.env` and skip `docker compose pull`.

## Native package

Download the Linux `.tar.gz` and `.sha256` for your architecture from [Releases](https://github.com/geniucker-dev/codex-rc-bridge/releases). Verify it with `shasum -a 256 -c FILENAME.tar.gz.sha256` in the download directory, then extract it. Running it needs no Rust or Bun. The package includes the server, management tool, web page and deployment examples.

Release packages are built on Ubuntu 22.04 and run on Linux with glibc 2.35 or later, such as Ubuntu 22.04, Debian 12 and newer releases. For older systems, see the [development guide](development.md#build-packages).

A suggested layout on Linux:

| Path                              | Contents                                                               |
| --------------------------------- | ---------------------------------------------------------------------- |
| `/opt/codex-rc-bridge/bin`        | The server and the `bridge-admin` tool                                 |
| `/opt/codex-rc-bridge/ui`         | The management page, served by the bridge                              |
| `/var/lib/codex-rc-bridge`        | The database, mode 0700, owned by the service user                     |
| `/etc/codex-rc-bridge-secrets`    | The key directory, mode 0700, key file 0600, owned by the service user |
| `/etc/codex-rc-bridge/server.env` | Your copy of `deploy/server.env.example`                               |

Create a dedicated `codex-bridge` user and those directories. As that user, initialize storage and create a management login; as with Docker, the first `add-user` also asks for the upstream proxy:

```sh
bridge-admin /var/lib/codex-rc-bridge/bridge.sqlite /etc/codex-rc-bridge-secrets/master.key init
bridge-admin /var/lib/codex-rc-bridge/bridge.sqlite /etc/codex-rc-bridge-secrets/master.key add-user TENANT_UUID admin
```

The key must belong to the user that runs the service, not to root. An existing database keeps using its original key; never initialize it again.

In `server.env`, set `BRIDGE_PUBLIC_ORIGIN` to your HTTPS address; `BRIDGE_UI_DIR` points the bridge at the `ui` directory and `BRIDGE_BIND` sets where it listens. Install `deploy/codex-rc-bridge.service` as a systemd unit and set up your reverse proxy as described above.

Before upgrading or backing up, stop the service normally and confirm it has exited. Back up the database and the key file separately, and leave both in place when you replace the program files. Rollback works as with Docker: restore the program together with the backup from before the upgrade.

## Tested environments

The native package has been tested on macOS arm64, and the Docker Compose deployment in a Linux arm64 container. systemd on a standalone Linux host, amd64 and Windows have not been tested yet.
