# smartmontools (smartctl)

Pulse bundles the unmodified `smartctl` from smartmontools and runs it as a
separate program (command line only; no linking).

- Licence: GPL-2.0. Full text: `GPL-2.0.txt` in this folder. Notice: `NOTICE-smartmontools.txt`.
- Upstream version: smartmontools 7.5 (r5714), macOS arm64, Developer ID signed by Orthic Labs (Team 6KLGD3LLKF).
- Binary: https://github.com/Orthic-Labs/rightkit-native-tools/releases/download/smartmontools-7.5-1/smartctl-7.5-macos-arm64
  (SHA-256 be345ce931c2e03e96e282076e92ef2eebf65eb7e9e782902762d09215a653eb). Fetched and verified at build time; never committed.
- Corresponding source (GPL-2.0 tarball and .asc): https://github.com/Orthic-Labs/rightkit-native-tools/releases/tag/smartmontools-7.5-1
- Upstream GPG key fingerprint: 0C95 77FD 2C4C FCB4 B9A5 9964 0A30 812E FF3A EFF5

In the app this folder lives at `Pulse.app/Contents/Resources/ThirdParty/smartmontools/`;
the binary is at `Pulse.app/Contents/Helpers/smartctl`.
