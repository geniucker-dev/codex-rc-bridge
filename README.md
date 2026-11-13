# Codex RC Bridge

English | [简体中文](README_zh.md)

Use Codex Remote Control across different ChatGPT accounts.

Say your phone is signed in to your personal account, while Codex on your work computer uses another one. ChatGPT's own remote control only reaches computers on the same account. Codex RC Bridge relays between the two accounts, so ChatGPT on your phone, tablet or computer can open that computer as if it were your own.

## Unofficial project

Codex RC Bridge is an independent open-source project. OpenAI doesn't make, endorse or support it. It talks to ChatGPT through the remote control interfaces that the official apps use, and OpenAI hasn't published those for other software. OpenAI can change or block them at any time, so the bridge may stop working without warning. You use it at your own risk and remain bound by OpenAI's terms.

Use it only to connect ChatGPT accounts that are yours. Don't use it to let someone else use your account or subscription: OpenAI's account sharing policy doesn't allow that.

The bridge keeps the sign-ins of every account you add, encrypted with a key stored apart from the database. With them it registers remote control devices for those accounts and relays their sessions. Whoever runs the bridge controls that key, so on a server the deployment administrator could in principle use the sign-ins of everyone on it. Run the bridge yourself, or only use a server run by someone you trust.

When you delete a controller or controlled device, the bridge revokes its access with ChatGPT; if that fails, it stays stopped until cleanup succeeds. Deleting a user from a server doesn't revoke anything, so that user's ChatGPT may still list remote devices that no longer work.

## How it works

The bridge has two kinds of things, and a connection joins them:

- A controller is the device you control from: a phone, a tablet, or ChatGPT on a computer. Each controller pairs with exactly one device and uses the account signed in to ChatGPT there.
- A controlled device is the computer you want to control, running Codex with remote control turned on. Each controlled device stands for exactly one computer and uses the account signed in to Codex there.

Connect a controller to a controlled device in the management page, and that computer shows up as a remote device in ChatGPT on the controller's device. The two sides can use different accounts.

You can set up as many pairs as you like. To control two computers from one phone, create two controllers and connect each to its own controlled device. Two controlled devices can also point to the same computer.

The bridge has to keep running, and Codex on the controlled computer needs remote control turned on. The device you control from only needs ChatGPT; it does not run the bridge.

## Choose where to run it

|                   | Desktop app                                                   | Self-hosted server                              |
| ----------------- | ------------------------------------------------------------- | ----------------------------------------------- |
| Good for          | Running the bridge on the same Mac as the controlled computer | An always-on host, or sharing with other people |
| Managed from      | A macOS app                                                   | A browser, including a phone's                  |
| Users             | One person, with any number of accounts and connections       | A separate workspace for each person            |
| In the background | Keeps running when you close the window; quitting stops it    | Keeps running when you close the browser        |

The interface comes in English and Simplified Chinese. It follows your system language and falls back to English. You can switch it in the web page header or in the app's **Settings**.

## Get started

### Desktop app (macOS)

Download a `.dmg` from [Releases](https://github.com/geniucker-dev/codex-rc-bridge/releases): choose `macos-arm64` for Apple Silicon or `macos-x64` for Intel. Open it and drag the app to Applications. Rust and Bun are not needed.

The app uses ad-hoc signing and is not notarized by Apple. If macOS blocks the first launch, choose **Open Anyway** in **System Settings → Privacy & Security**; see the [installation guide](docs/guide/desktop.md#install). Until a release is available, use a test build from GitHub Actions or build from source with the [development guide](docs/guide/development.md).

On first launch, choose a direct connection or enter an HTTP proxy in **Settings**, then follow the steps below. The [desktop guide](docs/guide/desktop.md) covers running in the background, the authorization callback, where data is stored and how to back it up.

### Server (Docker Compose)

Docker Hub has a ready-made image for x64 and arm64. You need a host with Docker and Compose, and a reverse proxy such as Caddy or nginx that serves your domain over HTTPS and forwards it to the bridge on `127.0.0.1:8787`:

```sh
mkdir codex-rc-bridge && cd codex-rc-bridge
curl -fsSLO https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.yaml
curl -fsSL -o .env https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.env.example
# Set BRIDGE_PUBLIC_ORIGIN in .env to your HTTPS address
docker compose pull
docker compose run --rm admin init
docker compose run --rm admin add-user TENANT_UUID admin
docker compose up -d server
```

Replace `TENANT_UUID` with the value `init` prints. `add-user` first asks for an upstream proxy (press Enter for a direct connection), then for a password of at least 12 characters. Then open your HTTPS domain and sign in as `admin`. This login is only for the bridge's management page and has nothing to do with your ChatGPT accounts.

The [server guide](docs/guide/server.md) covers reverse proxy settings, adding other people, the upstream proxy, upgrades, backups and installing without Docker.

## Connect your first computer

The desktop app and the web page work the same way. The **Remote control** page lists controllers on the left and controlled devices on the right.

1. **New controlled device:** choose the account Codex uses on the computer and give it a name. Authorization starts right away; once it's saved, enter the pairing code Codex shows on the computer.
2. **New controller:** choose the account ChatGPT uses on the device you control from and give it a name. A pairing code appears; open remote control in ChatGPT on that device and enter it. When pairing succeeds, the dialog shows the device's name.
3. **Connect:** on the free controller, choose **Connect to a device** and pick the controlled device. The connection starts at once. Then open the remote device in ChatGPT on the controller's device.

If an account isn't signed in yet, both dialogs let you add it.

During authorization, open or copy the link the page gives you and sign in with the account that step asks for. The desktop app receives the result automatically. With the server, paste the browser's final, complete address back into the management page, even if the browser says it cannot open the `localhost` page.

## Everyday use

On the **Remote control** page, each connection is drawn as a cable labeled with its state. When something goes wrong, the card on the side that has the problem is marked with the cause and the fix: sign in to the account again, authorize the controlled device again, retry the controller's registration, or finish an incomplete setup. Use the button on the card; the connection resumes once it's fixed.

- **Stop** pauses a connection. Both pairings are kept, and starting again needs no new pairing.
- **Switch device** moves a controller to another computer, disconnecting the old one first. On a phone, refresh the device view or open it again after switching.
- **Pair device again** on a controller's page replaces its device, for example after you change or lose it. The current device is removed first, then a new code appears.
- **Pair computer again** on a controlled device's page replaces its computer after a reinstall or a new machine. The old computer's access is removed first.
- A controlled device shown as offline means the computer can't be reached right now. The bridge reconnects when it comes back.

The **Accounts** page lists accounts that need signing in again first, along with the controllers and controlled devices that use them. **Check all sign-ins** checks every account at once.

## Controlling from ChatGPT on a computer

Give each controlled computer its own controller instead of switching one back and forth. ChatGPT on a computer remembers the remote projects and folder paths you opened for each controller, so after switching a controller to another computer, an old project's path may be used on the new one.

## Backups

Before upgrading or moving the bridge, back up the database together with its encryption key. Either one alone cannot restore the original device identities. See the [desktop guide](docs/guide/desktop.md#data-and-backups) and the [server guide](docs/guide/server.md#stop-upgrade-and-back-up).

## Development

Building, testing and local development are covered in the [development guide](docs/guide/development.md).

## License

Codex RC Bridge is released under the [Mozilla Public License 2.0](LICENSE).
