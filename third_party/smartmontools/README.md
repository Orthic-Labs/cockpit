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
- Binary (to be uploaded, then pinned in `scripts/release/windows-payload.mjs`):
  `https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev/native-tools/smartmontools-7.5-1/smartctl-7.5-windows-x64.exe`.
  Fetched and SHA-256 verified at build time; never committed. Until it is uploaded and pinned the payload
  has no `smartctl.exe` and the build still passes.
- Corresponding source: the same `smartmontools-7.5.tar.gz` and `.asc` as above.
- Device naming: smartmontools addresses `\\.\PhysicalDriveN` as `/dev/pdN`; Pulse finds N from the drive letter with
  `IOCTL_STORAGE_GET_DEVICE_NUMBER` (core) or `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` (hub).
- Elevation: smartctl opens a physical drive only from an elevated process. From the normal, unelevated hub it
  reports a permission error, so the hub falls back to what needs no administrator: the NVMe SMART / Health
  log through `IOCTL_STORAGE_QUERY_PROPERTY`, then Storage Management reliability counters, then the drive
  temperature. SATA SMART attributes need an elevated smartctl.
- Signing: `smartctl.exe` is Authenticode-signed with the other payload executables (or its signer is
  verified when RightKit ships it already signed).
