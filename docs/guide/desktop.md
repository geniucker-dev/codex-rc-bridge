# macOS desktop app

English | [简体中文](desktop.zh.md)

The desktop app is a standalone macOS application, a good fit when the bridge runs on the same Mac as the computer you want to control. It needs no server and no management password. One app can hold several accounts, controllers and controlled devices.

## Install

Download the `.dmg` for your Mac from [Releases](https://github.com/geniucker-dev/codex-rc-bridge/releases): `macos-arm64` for Apple Silicon or `macos-x64` for Intel. Open the disk image, drag the app to Applications and open it from there. Running it needs no Rust, Bun or terminal.

The app uses ad-hoc signing, has no Apple developer certificate and is not notarized. If macOS blocks the first launch, open **System Settings → Privacy & Security**, choose **Open Anyway** and confirm. See [Apple’s instructions](https://support.apple.com/en-us/102445).

Each installer has a `.sha256` file. Run `shasum -a 256 -c FILENAME.dmg.sha256` in the download directory to check it.

Until a release is available, sign in to GitHub, open **Actions → Build and release** in the repository, select a successful run and download `release-desktop-arm64` or `release-desktop-x64`. Extract it to get the `.dmg`. Build artifacts are kept for 14 days. For source builds, see the [development guide](development.md).

On first launch, choose a direct connection or enter an HTTP proxy in **Settings**, then follow the [README](../../README.md#connect-your-first-computer) to add a controller and a controlled device. The proxy only affects this app.

## Running in the background

Closing the window keeps the bridge running in the background and hides the Dock icon. Use the two-arrow icon in the menu bar to open the window again or to choose **Quit and stop bridging**. Quitting waits for operations in progress to finish. Connections you started are remembered and resume the next time you open the app.

## Authorization callback

During authorization the app opens the sign-in page in your default browser, or you can copy the link into another browser. After you sign in, the browser goes to `http://localhost:1455/auth/callback` and the app picks it up. If port 1455 is taken, it uses 1457.

If both ports are taken by other programs, the app asks you to copy the browser's final, complete address and submit it by hand. The address works even when the browser says it cannot open the `localhost` page.

## Data and backups

The database is at:

```text
~/Library/Application Support/com.geniucker.codex-rc-bridge/bridge.sqlite
```

Its encryption key is in your login Keychain, under the service `com.geniucker.codex-rc-bridge` and the account `desktop-database-key`. The database only opens with that key, so copying it to another Mac is not enough.

To back up, quit the app from the menu bar, then copy the database file. As long as the original Keychain item is still there, putting the database back restores the original device identities. If the database is present but the key cannot be read, the app refuses to start instead of creating a new key.

There is no way yet to export the key to another Mac and import it there. The desktop app and the server keep separate data and do not import each other's accounts.

## Supported systems

The desktop app is macOS only for now. Windows and Linux versions are not finished.
