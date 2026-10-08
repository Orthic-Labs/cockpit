# Pulse

Pulse is a native macOS notch for system gauges and controls. An always-on notch sits on the screen edge and shows usage rings for Claude, Codex, System (memory and CPU) and Disks, plus a Send cell for nearby sharing. A hub window holds the details and controls: Overview, Storage, Cleanup, Monitor, Apps, Permissions and Settings.

Windows is planned; a native prototype lives in [`windows/`](windows/README.md).

## Features

- **Usage rings.** The outer ring shows the weekly limit, the external drive and memory pressure. The inner ring shows the five-hour session, the internal drive and CPU. Hover a ring for its details.
- **Storage.** A treemap of folder sizes per volume, with cleanup findings for items that are safe to clear.
- **Apps.** Uninstall an app together with its leftovers, which go to the Trash. Also shows update state for apps installed through Sparkle, Homebrew or the App Store.
- **Password-free uninstalls.** An optional privileged helper, approved once in Login Items, moves allowed items to the Trash without an administrator password. Without it, root-owned items take one Finder request.
- **Conveniences.** Fn as Command, Finder cut and paste, window maximizing from the green button, Dock click minimize, Auto Quit when an app's last window closes, and a Launcher panel. Most are off by default.
- **Disk images.** When a disk image holding one app mounts, a card offers to copy the app to Applications, check its signature and Gatekeeper status, and eject the image.
- **Drive health.** The Disks hover shows temperature, health and wear from `smartctl` when it is installed (for example through Homebrew). USB drives that block SMART show their last good reading.
- **Nearby sharing.** Replaces the LocalSend app. Pulse speaks the open LocalSend protocol, so the LocalSend app on an iPhone, Android phone or another computer sees this Mac as "<Mac name> (Pulse)" and can send to it, and Pulse can send back. The notch's Send cell shows transfer progress and lists nearby devices in its hover card. Click a device to make it the target, press ⌘V over the cell to send the clipboard (files, an image or text), or drop files on the cell. An incoming request shows a card with Accept and Decline; received files land in Downloads (or the folder you pick) with duplicate names kept apart. Settings, General, Nearby sharing turns it on or off, renames the device and can accept from known devices automatically (off by default). The hub runs the service in the background while Pulse runs.

## Requirements

- macOS 26 or later
- Apple Silicon

## Install

Download the signed, notarized build from the [GitHub Releases page](https://github.com/Orthic-Labs/pulse/releases), open the disk image and drag Pulse into Applications.

Upgrading from the previous name: see [Upgrading](docs/mac-app-delivery.md#upgrading).

## Permissions

Pulse asks only for what an enabled feature needs. Settings and the hub's Permissions section show each one.

- **Accessibility.** Keyboard and window conveniences (Fn as Command, window maximizing, Dock click, Auto Quit, Finder cut and paste). Pulse never prompts for it; the hub shows its state and opens System Settings.
- **Input Monitoring.** Finder cut and paste key shortcuts may also need it.
- **Automation of Finder.** Reads the Finder selection and destination folder for cut and paste.
- **Full Disk Access (optional).** Lets disk scans read protected folders.
- **Background helper (optional).** Approved in Login Items; moves root-owned apps to the Trash without an administrator password.
- **Local network.** Nearby sharing finds devices and talks to them on your network. macOS asks the first time a device connects; the hub's Permissions section shows its state and opens System Settings.
- **Launch at login (optional).** Starts Pulse when you sign in. The disk image installer needs no permission.

## Command line

The `pulse` CLI reads the same core as the notch and hub:

```text
pulse status --json
pulse scan /explicit/path --json
pulse findings --json
pulse explain <finding-id> --json
pulse history --json
pulse procs --sort ram --json
pulse usage --json
pulse send --list --json
pulse send photo.jpg notes/ --to "iPhone" --json
```

`pulse send` offers files to a nearby device over the LocalSend protocol; the device must accept on its own screen.

A default scan persists nothing. `--save` records private metadata snapshots in the platform's application-support storage. No file contents are read, links are not traversed and cloud placeholders are skipped. Reported allocation does not promise reclaimed bytes.

## Building

Compilation and tests run only in GitHub Actions. Local work is static. Signing, notarization and publication run through RightKit release workflows, not on developer machines. Toolchains are pinned in [`.rightgit.json`](.rightgit.json): Node 26.8.1, pnpm 11.24.0 and the `xcode-27` runner.

| Path | Contents |
| --- | --- |
| `mac/Notch/` | Swift notch app, forked from Codenotch ([FORK.md](mac/Notch/FORK.md)) |
| `hub/` | Hub window: Tauri 2 app (`src-tauri`) with a React and Vite UI (`src`) |
| `core/` | Shared Rust core library and the `pulse` CLI |
| `windows/` | Windows native prototype |
| `scripts/`, `docs/` | Gate, release and probe scripts; plans, delivery and helper notes |

Contributors follow [AGENTS.md](AGENTS.md). Plans are in [docs/plan.md](docs/plan.md) and [docs/implementation-plan.md](docs/implementation-plan.md).

## Credits

Pulse builds on these projects. Donor inventory and pins: [docs/donors.md](docs/donors.md).

| Project | Licence | Used for |
| --- | --- | --- |
| [Codenotch](https://github.com/vinzdg/codenotch) | MIT (copy in [`mac/Notch/LICENSE`](mac/Notch/LICENSE)) | The notch app that Pulse forks |
| [Petal](https://github.com/henrydennis/petal) | MIT | Storage treemap, path classes and findings ideas; bulk directory listing ported to the core |
| [Uninstally](https://github.com/gostonx/uninstally) | MIT | Leftover detection ported to Rust for Apps |
| [Vorssaint](https://github.com/vorssaint/vorssaint-utils) | GPL-3.0-or-later | Idea only: the disk image installer and other conveniences are independent implementations, no code used |
| [smartmontools](https://www.smartmontools.org/) | GPL-2.0 (notice in [`release/smartmontools-NOTICE.txt`](release/smartmontools-NOTICE.txt)) | `smartctl` for drive health, run as a separate program |

No Vorssaint code is in Pulse.

Licence: proprietary, all rights reserved — see [LICENSE](LICENSE). Third-party notices are in [NOTICE](NOTICE).
