# smartmontools (smartctl)

macOS first; the Windows section is at the end.

Pulse bundles the unmodified `smartctl` from smartmontools and runs it as a
separate program (command line only; no linking).

- Licence: GPL-2.0. Full text: `GPL-2.0.txt` in this folder. Notice: `NOTICE-smartmontools.txt`.
- Upstream version: smartmontools 7.5 (r5714), macOS arm64, Developer ID signed by Orthic Labs (Team 6KLGD3LLKF).
- Binary: https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev/native-tools/smartmontools-7.5-1/smartctl-7.5-macos-arm64
  (SHA-256 be345ce931c2e03e96e282076e92ef2eebf65eb7e9e782902762d09215a653eb). Fetched and verified at build time; never committed.
- Corresponding source (GPL-2.0 tarball and .asc): https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev/native-tools/smartmontools-7.5-1/ (smartmontools-7.5.tar.gz, .asc, SHA256SUMS)
- Upstream GPG key fingerprint: 0C95 77FD 2C4C FCB4 B9A5 9964 0A30 812E FF3A EFF5

In the app this folder lives at `Pulse.app/Contents/Resources/ThirdParty/smartmontools/`;
the binary is at `Pulse.app/Contents/Helpers/smartctl`.

## Windows

Pulse for Windows bundles the same unmodified smartmontools 7.5 `smartctl.exe` (x64) and runs it as a
separate program (command line only; no linking). Same licence (GPL-2.0, `GPL-2.0.txt`) and notice.

- Where it lives: `%LOCALAPPDATA%\Programs\Pulse\Helpers\smartctl.exe` (the notch also looks beside
  `Pulse.exe` and in `ThirdParty\smartmontools\bin`); this folder is at `...\Pulse\ThirdParty\smartmontools\`.
- Binary: the official smartmontools 7.5 Windows installer,
  https://github.com/smartmontools/smartmontools/releases/download/RELEASE_7_5/smartmontools-7.5.win32-setup.exe
  (also https://sourceforge.net/projects/smartmontools/files/smartmontools/7.5/), SHA-256
  896337fcc253220614cf8cdbd5cf2321c5aa326a37a04160a672a281e6104c70. The build fetches it, verifies that hash,
  extracts only `bin\smartctl.exe` (the x64 build) with 7-Zip, verifies that file's SHA-256
  b5db94e5082c042be44994b7a4fa8f7b5c8e713b2ab1c9a560d8f7a7995ea27d, and stages it as `Helpers\smartctl.exe`
  (`scripts/release/windows-payload.mjs`). Never committed; a failed download or hash fails the build.
- Corresponding source: the same `smartmontools-7.5.tar.gz` and `.asc` as above, or upstream's
  https://github.com/smartmontools/smartmontools/releases/tag/RELEASE_7_5 and https://sourceforge.net/projects/smartmontools/files/smartmontools/7.5/.
- Device naming: smartmontools addresses `\\.\PhysicalDriveN` as `/dev/pdN`; Pulse finds N from the drive letter with
  `IOCTL_STORAGE_GET_DEVICE_NUMBER` (core) or `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` (hub).
- Elevation: smartctl opens a physical drive only from an elevated process. From the normal, unelevated hub it
  reports a permission error, so the hub falls back to what needs no administrator: the NVMe SMART / Health
  log through `IOCTL_STORAGE_QUERY_PROPERTY`, then Storage Management reliability counters, then the drive
  temperature. SATA SMART attributes need an elevated smartctl.
- Signing: upstream's `smartctl.exe` is unsigned; `Helpers/smartctl.exe` is in `sign.prePackageFiles`
  (`right-release.config.mjs`), so it is Authenticode-signed with the other payload executables.
