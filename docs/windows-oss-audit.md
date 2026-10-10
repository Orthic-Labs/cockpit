# Windows open-source audit: where Pulse's patterns match, trail or lead

Date: 2026-10-10. Pulse state read at `732fd76` (docs/plan.md, docs/parity.md, windows/README.md, AGENTS.md, docs/donors.md, then the code the claims rest on). Research only: no product code changed, nothing built or run. Nothing in Pulse's Windows build has run on a desktop (parity.md), so every "Pulse does X" below is read from source, not observed.

## How this was researched

- External facts come from opened sources: GitHub API metadata (licence, last push, archived flag, latest release, read 2026-10-10), the repository files named in each row, vendor docs, and a few web pages. Where a fact came only from a machine-summarised page or a search snippet it says "(summary)" or "(unverified)".
- The `legion:research` skill was loaded and its evidence rule followed (a hit is a lead; a claim needs an opened source and a located passage). I did not run its native `legion research` router.
- Licences: only OSI-approved licences count as "open source" here. Non-OSI or closed tools are flagged **NON-OSI**. GPL/AGPL code is reference-only unless run as a separate program (docs/donors.md rule); "pattern" below always means reimplementation from the technique, never a code copy.
- Verdict words: **Matches**, **Pulse behind** (with the thing to adopt and where), **Pulse ahead**.

## Headline findings

1. **The 7-minute home walk is Pulse's own doing, and the fix is already in-house.** On Windows the scanner lists a directory through `GetFileInformationByHandleEx` but keeps only the names (`core/src/platform/win_native.rs`, `list_with_class` returns `Vec<PathBuf>`), then calls `fs::symlink_metadata` plus a second attribute-only `CreateFileW` and three more queries per file (`core/src/scan.rs` `inspect_with`, `win_native.rs` `inspect`). Every directory record already carries size, allocation size, attributes, times, file id and reparse tag. The Mac path avoids exactly this with `mac_bulk.rs` (a Petal pattern). Rust's own `DirEntry::metadata` is free on Windows (no extra system call, [std docs](https://doc.rust-lang.org/std/fs/struct.DirEntry.html)), so walkers built on `read_dir` get this metadata at no cost; I did not read dua's, gdu's or diskus's Windows code paths.
2. **Toasts: the missing piece is one property on a shortcut Pulse already creates.** `scripts/release/windows/pulse.nsi` line 47 creates `Pulse.lnk` in the Start menu but never sets `System.AppUserModel.ID`, so `windows/src/notify.rs` borrows PowerShell's id and spawns `powershell.exe` per toast. Tauri's own NSIS template ships the macro that sets it, and an OSS Claude usage tray registers the identity with two HKCU values and no shortcut at all.
3. **Launch at login ignores Task Manager's override.** `autostart.rs` reads and writes only `HKCU\...\Run`. A user who disabled Pulse under Task Manager > Startup apps stays disabled (`StartupApproved\Run`) while Pulse's own toggle says "on". The `auto-launch` crate and EcoPaste handle that key.
4. **The Alt-as-Ctrl hook has no guardrails that every comparable tool has.** No per-app exclusion (Alt+C becomes Ctrl+C inside a console, an interrupt), no pause in D3D full screen, no way to notice that Windows silently removed a slow hook, and modifier state tracked only from hook events. PowerToys, AutoHotkey and Microsoft's own docs describe each of these.
5. **The Windows cleanup pack is 23 rules; Kudu's Windows pack is about 250 targets (MIT).** Kudu also has the retention and "performance reset" semantics Pulse lacks. Pulse's safety model (Recycle Bin only, re-validation, identity binding) is stricter than the cleaners audited (BleachBit and Winapp2-based tools delete outright), which is the right trade, but one Recycle Bin edge case needs a verification journey (see 3).
6. **Elevation is the universal wall.** CPU temperature and fans (LibreHardwareMonitor, TrafficMonitor), MFT/USN scanning (WinDirStat 2.5+, WizTree, Everything), SATA SMART (CrystalDiskInfo, smartctl) all need an elevated process or a kernel driver. The proven pattern is a **separate, optional elevated helper** (Everything 1.5's "indexing process as administrator", WizTree's scheduled task "highest privileges"), which is what the Mac already has in `docs/helper.md`. Pulse has no Windows equivalent. This is the largest structural gap, and also the most expensive.
7. **Where Pulse leads:** a live-updating index from `ReadDirectoryChangesW` with overflow handling, unelevated NVMe health, hard-link-safe sizing, `(pid, start_time)` process identity, a native no-focus layered notch (the other "dynamic island" clones found are Electron/WPF), and the Claude Desktop cache account-matching.

---

## 1. Always-on readings, widgets and sensors

| Tool | Licence | Activity (GitHub, 2026-10-10) | Technique that matters | Source |
| --- | --- | --- | --- | --- |
| TrafficMonitor | Anti-996 (**NON-OSI**) | v1.86 2026-03-29, pushed 2026-09-27 | Taskbar/floating readout; temperature, GPU and disk via LibreHardwareMonitor; README table: standard build needs administrator, Lite does not (Lite drops temperature) | [repo](https://github.com/zhongyang219/TrafficMonitor), [LICENSE](https://github.com/zhongyang219/TrafficMonitor/blob/master/LICENSE) |
| Rainmeter | GPL-2.0 | v4.5.27 2026-10-06 | Skin engine with plugins; GPL, reference only | [repo](https://github.com/rainmeter/rainmeter) |
| ModernFlyouts | MIT | **Archived** 2025-11-15 | Replaces OS volume/brightness flyouts; no overlap with Pulse | [repo](https://github.com/ModernFlyouts-Community/ModernFlyouts) |
| EarTrumpet | MIT plus a clause excluding named companies (**not a clean OSI licence**) | pushed 2026-10-04 | Per-app volume mixer tray flyout; no overlap | [LICENSE](https://github.com/File-New-Project/EarTrumpet/blob/master/LICENSE) |
| Windows "dynamic island" apps | DynamicWin CC-BY-SA-4.0 (**NON-OSI** for software), WinIsland GPL-3.0, eIsland GPL-3.0 (described as Electron), NetSpeed-Dynamic MIT, EchoIsland MIT | all pushed 2025-10 to 2026-10 | Notch-style overlays; EchoIsland says it unifies Codex/Claude Code sessions; its code was not inspected | [DynamicWin](https://github.com/FlorianButz/DynamicWin), [eIsland](https://github.com/JNTMTMTM/eIsland), [EchoIsland](https://github.com/FunplayAI/EchoIsland) |
| LibreHardwareMonitor | MPL-2.0 | v0.9.6 2026-02-14, pushed 2026-10-09 | Ring-0 access through the **PawnIO** driver and signed Pawn modules (Intel MSR, Ryzen SMU, LPC/SuperIO for fans); README says some sensors need administrator rights (it does not say which); the running GUI publishes a WMI namespace `root\LibreHardwareMonitor` and an HTTP `data.json` | [README](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor), [PawnIo.cs](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/master/LibreHardwareMonitorLib/PawnIo/PawnIo.cs), [IntelMsr.cs](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/master/LibreHardwareMonitorLib/PawnIo/IntelMsr.cs), [HttpServer.cs](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/master/LibreHardwareMonitor.Windows.Forms/Utilities/HttpServer.cs), [basicwmi.py](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/master/LibreHardwareMonitor.Windows.Forms/TestScripts/basicwmi.py) |
| OpenHardwareMonitor | MPL-2.0 (Licenses/License.html) | last push 2024-07 | Same idea on the **WinRing0** driver | [Ring0.cs](https://github.com/openhardwaremonitor/openhardwaremonitor/blob/master/Hardware/Ring0.cs) |
| PawnIO | GPL-2.0 with a linking exception; modules LGPL-2.1 | pushed 2026-08 | Signed driver exposing only module IOCTLs; device made with plain `IoCreateDevice` (default security); LHM opens it read/write, and every consumer I found tells users to run elevated. I found no documented non-admin route (unverified beyond that) | [driver.cpp](https://github.com/namazso/PawnIO/blob/master/PawnIO/src/driver.cpp), [modules](https://github.com/namazso/PawnIO.Modules), [pawnio.eu](https://pawnio.eu) |
| WinRing0 | n/a | n/a | Known-vulnerable driver (CVE-2020-14979); Microsoft Defender flags it, and LHM/FanControl moved to PawnIO | [Microsoft](https://support.microsoft.com/en-us/windows/security/threat-malware-protection/microsoft-defender-antivirus-alert-vulnerabledriver-winnt-winring0), [LHM release notes](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases) |
| sysinfo (what Pulse's core uses) | MIT | active | Its Windows temperature source is WMI `MSAcpi_ThermalZoneTemperature`, the same dead end Pulse measured | [component.rs](https://github.com/GuillaumeGomez/sysinfo/blob/main/src/windows/component.rs) |
| HWiNFO | **NON-OSI** closed freeware | n/a | Shared-memory interface to other apps; free edition limits it to 12 h per activation (forum announcement) | [HWiNFO forum](https://www.hwinfo.com/forum/threads/important-changes-to-hwinfo64-coming-soon.7092/post-29168) |
| PresentMon | MIT | v2.6.0 2026-09-21 | ETW frame timing; user must be in "Performance Log Users" or elevated; FPS is not a Pulse feature | [README](https://github.com/GameTechDev/PresentMon) |
| NVML | NVIDIA driver library (**NON-OSI**); `nvml-wrapper` Apache-2.0/MIT | wrapper v0.13.0 2026-08-31 | `nvml.dll` loaded dynamically; header exposes fan speed and power draw as well as temperature | [nvml-wrapper](https://github.com/Cldfire/nvml-wrapper), [nvml.h](https://github.com/Cldfire/nvml-wrapper/blob/main/nvml-wrapper-sys/nvml.h) |
| AMD ADLX / Intel IGCL | vendor SDK licences (**NON-OSI**) | active | Vendor GPU telemetry (temperature, fan, power) from user mode | [ADLX](https://github.com/GPUOpen-LibrariesAndSDKs/ADLX), [IGCL](https://github.com/intel/drivers.gpu.control-library) |

**Pulse today:** rings from `GetSystemTimes`, `GlobalMemoryStatusEx`, fixed drives, PDH "GPU Engine" and `GetIfTable2`; NVML GPU temperature; no CPU temperature or fans (README states why).

**Verdicts**
- Readings engine (CPU, memory, network, GPU busy, drives): **Matches** TrafficMonitor/Rainmeter-class tools, and no Electron overhead.
- Native layered no-focus window vs the Electron/WPF island clones: **Pulse ahead**.
- CPU temperature and fans unelevated: **Matches the ecosystem's limit**, not behind. Every tool that shows them needs a kernel driver and elevation (TrafficMonitor's own README splits builds on exactly this). Do **not** bundle a driver (WinRing0 history, signing, Defender).
- NVIDIA sensors: **Pulse behind, cheaply.** `nvmlDeviceGetFanSpeed` and `nvmlDeviceGetPowerUsage` are in the same `nvml.dll` Pulse already loads (`windows/src/sensors.rs` `Nvml`). Add GPU fan % and power to the System card.
- Optional bridge to LibreHardwareMonitor: if the user already runs LHM elevated, its HTTP `data.json` (or WMI namespace) gives CPU temperature and fans to an unelevated Pulse, as a separate program with no driver or licence entanglement. Pulse already has WinHTTP in `windows/src/http.rs`. Treat as opt-in, off by default, and say "from LibreHardwareMonitor" on the card.

---

## 2. Disk usage, scanning, live refresh, search

| Tool | Licence | Activity | Technique | Source |
| --- | --- | --- | --- | --- |
| WinDirStat | GPL-3.0(+) (changed in 2.9.2) | v2.9.2 2026-10-06 | 2.5.0 added direct NTFS MFT scanning with elevation detection/prompt; 2.9.0 added MFT-load cancellation, NTFS and FAT undelete, a File Watcher that uses `ReadDirectoryChangesW` | [CHANGELOG](https://github.com/windirstat/windirstat/blob/master/CHANGELOG.md), [FileWatcherControl.cpp](https://github.com/windirstat/windirstat/blob/master/windirstat/Controls/FileWatcherControl.cpp) |
| WizTree | **NON-OSI** closed | n/a | MFT reading; its own guide: fast MFT scan only when administrator, and a Task Scheduler task with "highest privileges" avoids the UAC prompt | [guide](https://diskanalyzer.com/guide) |
| SpaceSniffer | **NON-OSI** closed freeware | n/a | Treemap | [site](http://www.uderzo.it/main_products/space_sniffer/) |
| Everything | **NON-OSI** closed freeware | n/a | Filename index from the NTFS MFT plus USN journal. A standard user needs the Everything Service; Everything 1.5 can instead run only the indexing process elevated while the UI stays standard. The SDK is a thin IPC wrapper that needs the Everything client running | [service](https://www.voidtools.com/support/everything/everything_service/), [SDK](https://www.voidtools.com/support/everything/sdk/) |
| ntfs, mft, ntfs-reader, usn-journal-rs | ntfs Apache-2.0, mft Apache-2.0, others not checked | ntfs pushed 2026-01, mft 2026-10 | MFT/USN readers. ntfs-reader's docs: needs an elevated process to open `\\.\C:`; its timing example is about 12 s for a full MFT iteration | [ntfs](https://github.com/ColinFinck/ntfs), [mft](https://github.com/omerbenamram/mft), [ntfs-reader](https://docs.rs/crate/ntfs-reader/0.4.6), [usn-journal-rs](https://docs.rs/usn-journal-rs) |
| dua-cli | MIT | v2.45.1 2026-09-30 | Parallel walk aimed at saturating an SSD | [README](https://github.com/Byron/dua-cli) |
| gdu | MIT | v5.38.0 2026-10-05 | Parallel scan for SSD, `--sequential` for HDD, hard links counted once | [README](https://github.com/dundee/gdu) |
| diskus | Apache-2.0 / MIT | v0.9.0 2025-12 | Parallel `du -sh`; its README says it counts Windows hard links and junctions multiple times | [README](https://github.com/sharkdp/diskus) |
| Czkawka / Krokiet | MIT core and CLI; Krokiet GUI GPL-3.0-only | 12.0.2 2026-09-09 | Duplicate finder (name, size, hash); README lists cache support so repeat scans are faster | [README](https://github.com/qarmin/czkawka) |
| Spacedrive | Apache-2.0 | pushed 2026-10-09 | Rust daemon, SQLite plus FTS5 index kept current by watchers | [README](https://github.com/spacedriveapp/spacedrive) |
| Windows Search index (OS) | OS component | n/a | PowerToys Run's Indexer plugin queries it through `ISearchQueryHelper`; Flow Launcher lists Everything and the Windows index as supported sources | [PowerToys Indexer](https://github.com/microsoft/PowerToys/tree/main/src/modules/launcher/Plugins/Microsoft.Plugin.Indexer), [Flow Launcher](https://github.com/Flow-Launcher/Flow.Launcher) |

**Pulse today:** a descriptor-pinned, no-follow walk (2 to 6 read-ahead threads) of one root; sizes with allocation and file identity; hub `watch_windows.rs` re-reads changed folders from `ReadDirectoryChangesW`, bounded re-walk on overflow; there is **no whole-disk filename index on Windows** (`hub/src-tauri/src/disk_index.rs` has `#[cfg(not(target_os = "macos"))]` stubs that return "unsupported", so Windows search falls back to the scan).

**Verdicts**
- **Pulse behind, in-house fix (highest value):** per-file opens. See headline 1. Pointer: add `core/src/platform/win_bulk.rs` mirroring `mac_bulk.rs`. Use the class already requested in `list_with_class` (`FILE_ID_EXTD_DIR_INFO`, `FILE_ID_BOTH_DIR_INFO` fallback) to return name, attributes, reparse tag, allocation size, end of file, times and file id per entry; take the volume serial once from the directory handle; classify a regular file with no reparse bit without opening it; keep the pinned handle only for directories you recurse into. Also look at `ancestor_identity_snapshot` (opens every ancestor on each directory listing; a per-directory cost that can be cached per walk). Expect the largest gain here, since Defender and other minifilters inspect every `CreateFile`. Not measured (no local runs allowed).
- Live refresh via `ReadDirectoryChangesW`: **Pulse ahead** of the OSS disk tools. WinDirStat's File Watcher uses the same API for a change list; dua, gdu, diskus and Czkawka re-scan or use a cache. USN-journal replay (changes while the hub was closed) is what Pulse lacks, and it needs the elevation wall.
- Unelevated MFT/USN: **Matches the ecosystem.** Nobody reads the MFT unelevated; WinDirStat prompts for elevation, WizTree and Everything document it. If Pulse wants MFT speed it needs the optional elevated helper (priority list, item 10), not a clever API.
- Whole-disk filename search on Windows: **Pulse behind.** Cheap interim: query the Windows Search index for filename hits (the PowerToys Indexer pattern: `ISearchQueryHelper` over COM), unelevated and instant for indexed locations, clearly labelled "indexed locations only". Longer term, Spacedrive's SQLite+FTS5 design is the pattern if a Windows index is built (plan.md already allows SQLite "if a feature needs queries JSON can't serve").
- Hard-link and allocation accounting: **Pulse ahead** of diskus (documented double counting on Windows) and level with WinDirStat 2.5's hard-link tracking.
- Duplicates: **Matches** on safety (Pulse bounds reads at 100 KB minimum, 1 GiB total, 30 s; hashes only after size and a 4 KiB sample); **behind** Czkawka on scale and a persistent hash cache. Note parity.md still says no Windows duplicate reader; `core/src/duplicates.rs` has a `cfg(windows)` reader.

---

## 3. Cleanup

| Tool | Licence | Activity | Technique | Source |
| --- | --- | --- | --- | --- |
| Kudu | MIT | v3.7.0 2026-10-10 | Declarative JSON rules per OS. `rules/win32/*.json`: 143 app entries, 13 Chromium browsers, 4 Firefox forks, 21 database targets, 18 gaming, 5 GPU-cache, 54 system targets (my count). Fields Pulse lacks: `minAgeDays`, `deepRecencyCheck` (a recent descendant protects a stale directory), `cacheReset` (visible but unselected, "slower next launch"), `needsAdmin`, `cleanupAction` that delegates to Windows-managed cleanup (Delivery Optimization, component cleanup). Windows evidence doc `WINDOWS_APP_CACHE_SAFETY.md` shows its safety reasoning (exact Chromium cache leaves only, no wildcard app discovery) | [repo](https://github.com/AdventDevInc/kudu), [RULES.md](https://github.com/AdventDevInc/kudu/blob/main/rules/RULES.md), [safety doc](https://github.com/AdventDevInc/kudu/blob/main/rules/WINDOWS_APP_CACHE_SAFETY.md) |
| BleachBit / CleanerML | GPL-3.0(+) | v6.0.4 2026-09-08 | XML cleaners with `<running type="exe" same_user="true">` guards, an "AI models" option that deletes Chrome's on-device model folders; Preview then Delete; imports Winapp2 | [README](https://github.com/BleachBit/BleachBit), [google_chrome.xml](https://github.com/BleachBit/BleachBit/blob/master/cleaners/google_chrome.xml) |
| Winapp2.ini | CC-BY-SA-4.0 data (**NON-OSI**) | pushed 2026-10-09 | Thousands of declarative entries for CCleaner/BleachBit/FluentCleaner; its own disclaimer: deletion is "potentially irreversible" | [Winapp2](https://github.com/MoscaDotTo/Winapp2) |
| FluentCleaner | MIT | 26.09.01 2026-09-17 | WinUI 3 front end over a Winapp2.ini parser | [repo](https://github.com/builtbybel/FluentCleaner) |
| CleanmgrPlus | custom EULA (**NON-OSI**), stale since 2024 | pushed 2024-05 | Disk Cleanup replacement | [repo](https://github.com/builtbybel/CleanmgrPlus) |
| trash-rs (what Pulse uses) | MIT | pushed 2026-09 | Windows delete = `IFileOperation` with `FOF_NO_UI | FOF_ALLOWUNDO | FOF_WANTNUKEWARNING`, and it reports aborted operations | [windows.rs](https://github.com/Byron/trash-rs/blob/master/src/windows.rs) |

**Pulse today:** `rules/cleanup.json` has 46 rules, 23 of them Windows-capable (browser caches, npm/pnpm/yarn/pip/go/NuGet/Maven, .NET obj/bin, installers, old downloads, temp, crash dumps, WER, thumbnail cache); Recycle Bin only; per-item re-check; history with restore; Windows-managed and administrator targets deliberately excluded.

**Verdicts**
- Safety model (Recycle Bin only, re-validation, identity binding, never claim freed): **Pulse ahead** of BleachBit and Winapp2-based cleaners, which delete outright, and of Kudu's rule-based cleaning (Kudu calls Electron's `trashItem` for user-chosen files such as duplicates and large files, [for example](https://github.com/AdventDevInc/kudu/blob/main/src/main/ipc/duplicate-finder.ipc.ts); I did not confirm a restore path for its rule-based cleaning).
- Rule coverage: **Pulse behind.** Adopt data, not code: re-implement from Kudu's `rules/win32` (MIT, keep the notice in `rules/cleanup.json` `sources`) the unelevated, user-profile entries that matter on a developer PC: Electron app caches (exact `Cache/Cache_Data`, `Code Cache`, `GPUCache` leaves, never profile roots), `%LOCALAPPDATA%\D3DSCache` and vendor shader caches (as `cacheReset`), Chrome/Edge on-device model folders, `CrashDumps`, `INetCache`. Port three schema ideas: `min_age_days` already exists, so add `deep_recency` (descendant recency), a `cache_reset` flag (unselected group) and `needs_admin` (listed, not offered). Winapp2.ini is **not** to be imported: CC-BY-SA data is not OSI and its "irreversible" model conflicts with Pulse's rules; treat it as reference.
- BleachBit's `same_user` running check: **Pulse behind, small.** `cleanup_scan::running_process_names` (sysinfo) is name-only; scoping the "app is running" guard to the current user's session avoids false blocks from other sessions.
- **Verify, do not assume (Recycle Bin edge cases).** trash-rs passes `FOF_NO_UI`, so a delete the shell cannot recycle should abort rather than prompt; Microsoft documents `FOF_WANTNUKEWARNING` as partly overriding silent mode ([SHFILEOPSTRUCTW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-shfileopstructw), [SetOperationFlags](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ifileoperation-setoperationflags)). I could not confirm from sources what happens for an item larger than the bin's cap, on a drive with no Recycle Bin (USB, network) or with "don't move files to the Recycle Bin" set for a volume. `hub/src-tauri/src/files.rs` and `duplicates.rs` call the same `cleanup::move_to_trash` for user-chosen files, which can sit on any drive, not only under the profile. Add one E2E journey per case (oversize item, removable drive, bin disabled) that must end "refused", never "deleted", and consider a preflight with `SHQueryRecycleBinW` / drive type.
- Throughput: moving a cache of millions of small files into the bin is slow and frees nothing until emptied; the other cleaners delete directly. That is a deliberate Pulse trade-off; if it hurts, a per-rule "rebuildable cache: delete now (explicit confirm)" is the only change that stays inside the spirit of the safety rules and needs an owner decision.

---

## 4. Apps, uninstall, updates

| Tool | Licence | Activity | Technique | Source |
| --- | --- | --- | --- | --- |
| Bulk Crap Uninstaller | Apache-2.0 | v6.3 2026-09-07; README: "Looking for maintainers" | Uninstall-registry, Store apps, Steam, Windows features, portable apps; quiet-uninstall detection; junk finders with a 5-level confidence enum (Unknown, Bad, Questionable, Good, VeryGood) over install location, Prefetch, WER, installer folders, shortcuts, startup, COM, firewall rules, UserAssist | [README](https://github.com/Klocman/Bulk-Crap-Uninstaller), [ConfidenceLevel.cs](https://github.com/Klocman/Bulk-Crap-Uninstaller/blob/master/source/UninstallTools/Junk/Confidence/ConfidenceLevel.cs) |
| UniGetUI | MIT | v2026.3.1 2026-10-08; Devolutions-maintained | Front end for winget, Scoop, Chocolatey and others; winget through the **COM API** with a CLI fallback that parses the table by header layout in many languages | [repo](https://github.com/Devolutions/UniGetUI), [NativeWinGetHelper.cs](https://github.com/Devolutions/UniGetUI/blob/main/src/UniGetUI.PackageEngine.Managers.WinGet/ClientHelpers/NativeWinGetHelper.cs), [WinGetTableLayout.cs](https://github.com/Devolutions/UniGetUI/blob/main/src/UniGetUI.PackageEngine.Managers.WinGet/ClientHelpers/WinGetTableLayout.cs) |
| winget | MIT | v1.29.380 2026-09-21 | Source of truth for updates | [repo](https://github.com/microsoft/winget-cli) |
| Scoop / Chocolatey | Scoop Unlicense or MIT; Chocolatey CLI Apache-2.0 | active | Scoop installs under `~/scoop`, not the Uninstall registry | [Scoop](https://github.com/ScoopInstaller/Scoop), [choco](https://github.com/chocolatey/choco) |
| `PackageManager` (OS API) | OS | n/a | `FindPackagesForUser("")` lists the current user's packages without administrator; `RemovePackageAsync` works at medium integrity for the current user | [FindPackagesForUser](https://learn.microsoft.com/en-us/uwp/api/windows.management.deployment.packagemanager.findpackagesforuser), [RemovePackageAsync](https://learn.microsoft.com/en-us/uwp/api/windows.management.deployment.packagemanager.removepackageasync) |

**Pulse today:** registry inventory (HKLM 64/32, HKCU) plus `Get-AppxPackage` for Store apps; uninstall starts the app's own uninstaller (or `Remove-AppxPackage`), then moves publisher/product-matched AppData/ProgramData folders to the Recycle Bin; registry keys and startup entries are listed but never deleted; winget table parsing by 2+ space runs; last used from UserAssist; running state from process image paths.

**Verdicts**
- Uninstall safety (own uninstaller first, leftovers only after the registration is gone, registry read-only): **Pulse ahead** of BCU's broader but riskier registry cleanup, and consistent with its confidence idea at a coarser grain (2 tiers: publisher selected, name-only unchecked).
- Leftover coverage: **Pulse behind** on *what is listed*. BCU also finds Prefetch, WER, `C:\Windows\Installer` folders, shortcuts, firewall rules. Adopt as read-only "background entries" in the existing list rather than new delete paths. Move from 2 tiers to BCU-style graded confidence only if false positives show up in real use.
- Store apps: **Pulse behind, small.** `PackageManager.FindPackagesForUser("")` and `RemovePackageAsync` replace two PowerShell spawns (about a second of cold start each) in `core/src/apps_windows/appx.rs`, no elevation either way. Medium effort in Rust (WinRT calls through the `windows` crate).
- winget parsing: **Matches** the field. UniGetUI also falls back to a table parser, and goes the other way (header-name layout) where Pulse goes column-gap based; both are fragile by nature. The step up is the winget COM API (UniGetUI's native helper), which gives structured results and avoids spawn latency; high effort, defer unless parsing breaks.
- Portable and Scoop apps: **Pulse behind.** Not in the Uninstall registry, so invisible to Pulse; BCU finds portable apps in configurable folders. Low priority for a personal tool unless the owner uses Scoop (`scoop export`).
- Maturity note: BCU asks for maintainers; treat it as a source of ideas, not a dependency.

---

## 5. Processes and monitor

| Tool | Licence | Activity | Technique | Source |
| --- | --- | --- | --- | --- |
| System Informer (Process Hacker) | MIT | v4.0.26241.138 2026-08-29, pushed 2026-10-10 | Per-process GPU and NPU from `D3DKMTQueryStatistics` (`plugins/ExtendedTools/gpumon.c`, `npumon.c`); ships an optional kernel driver (`KSystemInformer`) | [repo](https://github.com/winsiderss/systeminformer), [gpumon.c](https://github.com/winsiderss/systeminformer/blob/master/plugins/ExtendedTools/gpumon.c) |
| TaskExplorer | GPL-3.0 | v2.0.0 2026-09-09 | Qt UI over the Process Hacker library and a custom-built System Informer driver; its socket panel can show pseudo-UDP connections from ETW data | [README](https://github.com/DavidXanatos/TaskExplorer) |
| btop4win / bottom | Apache-2.0 / MIT | btop4win last release 2025-10; bottom 0.14.9 2026-08 | Terminal monitors; temperatures through sysinfo (same WMI limit) | [btop4win](https://github.com/aristocratos/btop4win), [bottom](https://github.com/ClementTsang/bottom) |
| Task Manager (OS) | OS | n/a | Per-process "GPU" and "GPU engine" columns from the WDDM GPU performance counters (Fall Creators Update and later), the same "GPU Engine" counters Pulse already queries for the total | [DirectX blog](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/) |

**Pulse today:** sysinfo process list; groups by verified parent chain with PID-reuse and ambiguity guards (`core/src/processes.rs`); the hub groups by executable name on Windows; Quit sends a graceful close, Force Quit ends the tree and never happens on its own; each action is bound to `(pid, start_time)`; `pulse procs --sort gpu` returns "per-process GPU is unavailable" (`core/src/main.rs`).

**Verdicts**
- Identity-bound, never-auto-escalating process actions: **Pulse ahead** of taskkill-based flows.
- Per-process GPU: **Pulse behind.** The PDH counters (`\GPU Engine(pid_<pid>_*)\Utilization Percentage`) include the process id in the instance name, need no elevation, and `sensors.rs` already samples the same counter set for the total. Parse the instance name, sum per pid per engine type, sort. Low-medium effort. System Informer's D3DKMT route is richer (memory per segment) and also unelevated for the user's own processes, but heavier to reimplement.
- Per-process network and disk: **Matches the unelevated limit.** TaskExplorer needs ETW and a driver for sockets; not worth it.
- Grouping: **Matches** the Task Manager "apps" idea where it matters (Chrome/Electron trees) and is stricter than name grouping. Verify on real Windows that explorer-launched trees do not collapse into one `explorer.exe` group.
- Optional later: Windows 11's Efficiency mode is power throttling set by `SetProcessInformation(ProcessPowerThrottling)`; System Informer carries the constant in `phlib/nativeprocess.c`. Not needed now.

---

## 6. Drive health

| Tool | Licence | Activity | Technique | Source |
| --- | --- | --- | --- | --- |
| smartmontools | GPL-2.0 (run as a separate program, as Pulse does) | release 7.5 2025-05-12; `drivedb.h` has 74 commits since | `smartctl -j`; `-d sat` and NVMe-over-USB bridge types `sntasmedia`, `sntjmicron`, `sntrealtek`; `-B FILE` loads a newer drive database (Windows: `EXEDIR\drivedb-add.h`) | [smartctl.8.in](https://github.com/smartmontools/smartmontools/blob/main/src/smartctl.8.in), [drivedb.h](https://github.com/smartmontools/smartmontools/blob/main/drivedb/drivedb.h) |
| CrystalDiskInfo | MIT | pushed 2026-07 | ATA SMART, partial NVMe/USB/RAID; `DiskInfo.vcxproj` sets `UACExecutionLevel` to `RequireAdministrator` | [repo](https://github.com/hiyohiyo/CrystalDiskInfo), [vcxproj](https://github.com/hiyohiyo/CrystalDiskInfo/blob/master/DiskInfo.vcxproj) |
| Scrutiny | MIT | v0.9.5 2026-09-30 | smartctl collector plus history/thresholds from real-world failure rates, webhook alerts | [README](https://github.com/AnalogJ/scrutiny) |

**Pulse today:** hub `health_windows.rs` tries bundled `smartctl.exe -a -j /dev/pdN`, then the **unelevated** NVMe health log through `IOCTL_STORAGE_QUERY_PROPERTY`, then PowerShell reliability counters, then temperature only; keeps timestamped history; the notch's `windows/src/drive_health.rs` uses smartctl only.

**Verdicts**
- Unelevated NVMe health: **Pulse ahead.** CrystalDiskInfo and smartctl both need administrator.
- SATA SMART unelevated: **Matches the limit** (all three need an elevated process). The helper in the priority list would also solve this.
- **Pulse behind, small, in-house:** the notch Disks card reads smartctl only, so on an unelevated notch it can say "n/a" while the hub shows NVMe data from the IOCTL path. Share the hub's result (or the IOCTL code) with the notch.
- **Pulse behind, small:** bundled smartctl 7.5 embeds a May 2025 drive database. Ship a pinned newer `drivedb.h` beside `smartctl.exe` (it is data in the GPL repo; keep the notice) and pass `-B`; use `smartctl --scan-open` instead of assuming `pdN`; for USB NVMe enclosures retry with `-d sntjmicron|sntasmedia|sntrealtek`. That addresses the "health unavailable through this connection" cases the plan describes for the Mac too.
- Alerting and history: **Matches** Scrutiny in spirit (timestamped readings, alerts on change). Scrutiny's idea worth borrowing later is thresholds from failure-rate data rather than vendor pass/fail only.

---

## 7. Screenshots and keyboard

| Tool | Licence | Activity | Technique | Source |
| --- | --- | --- | --- | --- |
| ShareX | GPL-3.0 | v21.0.0 2026-07-03 | Hotkeys through `RegisterHotKey`; Windows.Graphics.Capture in its recorder | [WindowsHotkeyHost.cs](https://github.com/ShareX/ShareX/blob/develop/ShareX.HelpersLib/Input/WindowsHotkeyHost.cs), [GraphicsCapture.cs](https://github.com/ShareX/ShareX/blob/develop/ShareX.ScreenRecordingLib/Video/GraphicsCapture.cs) |
| Greenshot | GPL-3.0 | v1.3.323 2026-10-08 | `GraphicsCaptureBackend`: its own comment says hardware accelerated, captures windows without what covers them, HDR aware, with a GDI fallback setting | [GraphicsCaptureBackend.cs](https://github.com/greenshot/greenshot/blob/main/src/Greenshot.Base/Capturing/GraphicsCaptureBackend.cs) |
| Flameshot | GPL-3.0 | v14.0.0 2026-06-19 | Qt in-place annotation | [repo](https://github.com/flameshot-org/flameshot) |
| PowerToys (Keyboard Manager, FancyZones, Peek, Run, Command Palette) | MIT | v0.101.2362.0 2026-08-25 | KBM can map `Alt+C` to `Ctrl+C`, per app by process name; docs list limits: elevated windows need an elevated PowerToys, AltGr/Ctrl+Alt issues, not for games. `detect_game_mode()` = `SHQueryUserNotificationState` is `QUNS_RUNNING_D3D_FULL_SCREEN`, called by several modules to stand down | [KBM docs](https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager), [game_mode.h](https://github.com/microsoft/PowerToys/blob/main/src/common/utils/game_mode.h), [modules](https://learn.microsoft.com/en-us/windows/powertoys/) |
| AutoHotkey v2 | GPL-2.0 | v2.0.30 2026-10-09 | `A_MenuMaskKey`: default is the left Ctrl key; docs recommend `vkE8` (unassigned) to stop Alt/Win release opening menus. Elevated windows: recommended answer is "run with UI access", which requires installing under Program Files. Physical key state via hook is accurate only for keys pressed while the hook was installed | [MenuMaskKey](https://www.autohotkey.com/docs/v2/lib/_MenuMaskKey.htm), [FAQ](https://www.autohotkey.com/docs/v2/FAQ.htm), [GetKeyState](https://www.autohotkey.com/docs/v2/lib/GetKeyState.htm) |
| kanata | LGPL-3.0 | v1.12.0 2026-07-05 | Rust remapper with layers and tap-hold; an optional Interception-driver build (`--features interception_driver`) exists for Windows, the plain build does not use it | [README](https://github.com/jtroo/kanata) |
| Microsoft `LowLevelKeyboardProc` | OS doc | n/a | Hook procedures must return within `LowLevelHooksTimeout` (at most 1000 ms on Windows 10 1709+); on timeout Windows removes the hook silently and the app cannot tell; advice: dedicated thread handing off work | [docs](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc) |
| Windows Terminal | MIT | n/a | Ctrl+C is overloaded between copy and interrupt (a maintainer says the copy handler should decline when nothing is selected) | [issue 2210](https://github.com/microsoft/terminal/issues/2210) |

**Pulse today (`windows/src/keys.rs`, `shot.rs`):** one `WH_KEYBOARD_LL` hook on its own thread; Alt+A/C/V/X/Z to Ctrl and Alt+Shift+Z to Ctrl+Y by swallowing and injecting one `SendInput` batch with `VK_E8` masking; AltGr, Alt+Tab, Alt+F4, Alt+Space untouched; own injected events tagged; Alt+Shift+4/5 screenshot region and toolbar using `BitBlt` from the screen DC; PNG by WIC plus `CF_DIB`.

**Verdicts**
- Masking with `VK_E8`, dedicated hook thread, tagging own input, AltGr exclusion: **Matches** AutoHotkey's and Microsoft's guidance.
- vs PowerToys KBM feature for the same remap: **Pulse behind** on guardrails, **ahead** on being built in (no PowerToys process, no settings round trip). Gaps, all verifiable in `keys.rs`:
  - *No per-app exclusion.* In `WindowsTerminal.exe`, `cmd.exe`, `pwsh.exe`, `powershell.exe`, `conhost.exe`, Alt+C becomes an interrupt. parity.md G9 proposed sending Ctrl+Shift+C/V there; it was not implemented. Pattern: KBM's per-process targeting; cache the foreground process with `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` so the hook callback never queries it.
  - *No game guard.* `SHQueryUserNotificationState == QUNS_RUNNING_D3D_FULL_SCREEN` is what PowerToys modules use (`game_mode.h`). `visibility.rs` has a geometry heuristic for the notch but nothing consults it for the hook, and it does not use this API either.
  - *No liveness check.* Because Windows removes a slow hook silently, reinstall on resume (`WM_POWERBROADCAST`), session unlock and a coarse timer, and log `keys_hook_reinstalled`.
  - *Stale modifier state.* `CTRL_DOWN`, `SHIFT_DOWN`, `WIN_DOWN` come only from hook events. A Ctrl release on the secure desktop or while the hook is absent is never seen, after which `handle()` treats Alt+C as "Ctrl down" and declines. Resync from `GetAsyncKeyState` at lock/unlock and when idle.
  - *Elevated windows:* **Matches KBM's limit.** The only fix is UI access (signed, installed under Program Files), which a per-user installer cannot do; say so in Settings next to the toggle.
- Screenshot capture: **Pulse behind** on engine. Greenshot's primary backend is Windows.Graphics.Capture with GDI fallback, for HDR, hardware-accelerated and occluded windows; Pulse's `BitBlt` captures whatever is on screen and can return black for protected or overlay content (window pick includes whatever covers the window). Adopt Graphics Capture for the Window mode first, keep `BitBlt` for region. Medium effort; the design is already written in parity.md G10.
- Screenshot hotkeys: **Pulse behind, small.** ShareX uses `RegisterHotKey`, which is not subject to the hook timeout and reports "already registered" cleanly. Alt+Shift+4/5 can use it while Alt-as-Ctrl keeps the hook, so a removed hook no longer takes screenshots down with it.
- FancyZones/Win+Arrow vs G4 window management: **not needed**; the OS covers it (parity.md already marks G4 "uncertain").
- Launcher (none on Windows): **do not build one.** Command Palette (the PowerToys Run successor, MIT, extensible) and Flow Launcher (MIT, Everything and Windows-index search, JSON-RPC plugins) exist; `pulse` already emits JSON for `status`, `apps`, `find`, `usage`, `send`, so a thin Command Palette extension or Flow plugin is a few hundred lines. [Command Palette](https://github.com/microsoft/PowerToys/tree/main/src/modules/cmdpal), [Flow Launcher](https://github.com/Flow-Launcher/Flow.Launcher).
- Annotation/editing, scrolling capture, OCR, upload (ShareX, Flameshot): **out of scope** for a "match the Mac" screenshot; Peek (Quick Look) likewise.

---

## 8. Nearby sharing

| Tool | Licence | Activity | Notes | Source |
| --- | --- | --- | --- | --- |
| LocalSend | Apache-2.0 | v1.18.2 2026-08-21 | Release notes: do not follow peer HTTP redirects, ignore system proxies, restart server when it dies or on resume, fallback when multicast is missing, a CLI, "Receive via link" | [repo](https://github.com/localsend/localsend) |
| LocalSend protocol | no licence file (docs/donors.md already uses facts only) | 2.2 added 2026-08-07; a `v3` directory with HTTP and WebRTC diagrams added 2026-07-27 | v2.2 adds `422` on SHA-256 mismatch; v2.1 added PIN | [protocol](https://github.com/localsend/protocol), [CHANGELOG](https://github.com/localsend/protocol/blob/main/CHANGELOG.md) |
| Alternatives | PairDrop GPL-3.0, rquickshare GPL-3.0, croc MIT, Tailscale BSD-3-Clause, Warpinator ports GPL-3.0, KDE Connect | all active | Browser-based, Quick Share, relay-based or phone-integration designs; none is a drop-in for the LocalSend peers the owner uses | [PairDrop](https://github.com/schlagmichdoch/PairDrop), [rquickshare](https://github.com/Martichou/rquickshare), [croc](https://github.com/schollz/croc), [Tailscale](https://github.com/tailscale/tailscale), [warpinator-windows](https://github.com/slowscript/warpinator-windows), [KDE Connect](https://github.com/KDE/kdeconnect-kde) |

**Verdicts**
- Protocol v2.2 (SHA-256 verified on receive, 422 path), per-interface multicast join with virtual-adapter skipping (`skips_interface` handles vEthernet/WSL/Hyper-V names), HTTP subnet sweep, certificate fingerprint: **Matches** LocalSend and covers its Windows pitfalls.
- Redirects/proxies: **Probably matches.** Pulse speaks HTTP over its own rustls code (`core/src/localsend/net.rs`) and I found no redirect handling; verify it also ignores the system proxy, which LocalSend 1.18 had to fix.
- Not implemented: PIN (the send path says "needs a PIN" and refuses), the download/"Receive via link" API, the legacy scan. **Pulse behind, low priority**; PIN matters only if a peer enforces it.
- Server resilience (restart on death or resume): **check.** LocalSend 1.18 added it after real failures; confirm `hub/src-tauri/src/share.rs` re-binds after sleep/resume and network changes.
- The `v3` spec directory (HTTP and WebRTC diagrams): watch, do not implement.

---

## 9. AI usage trackers on Windows

| Tool | Licence | Activity | Sources and technique | Source |
| --- | --- | --- | --- | --- |
| ccusage | MIT | v20.0.28 2026-10-10 | Reads local usage data for Claude Code, Codex and many other agent CLIs and reports daily, weekly, monthly and session token/cost totals; it does not read the server-side limit percentages | [README](https://github.com/ryoppippi/ccusage) |
| Claude-Code-Usage-Monitor | MIT | v4.0.0 2026-06-27 | Terminal monitor over local sessions: P90-based custom limits from the last 192 hours, realtime and burn-rate views | [repo](https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor) |
| CodexBar | MIT | v0.73.0 2026-10-07 (macOS app, Linux app, CLI; separate Windows tray apps named after it exist, for example [CodexBar-Win](https://github.com/babakarto/CodexBar-Win), MIT) | Claude: OAuth, then CLI PTY, then web cookies; Codex: PAT, OAuth, then CLI RPC `codex app-server`; delegates token renewal to the CLI that owns the file and never publishes refreshed tokens itself; guards against suspicious weekly resets; recorded burn-down history; local cost scans | [claude.md](https://github.com/steipete/CodexBar/blob/main/docs/claude.md), [codex.md](https://github.com/steipete/CodexBar/blob/main/docs/codex.md) |
| usage-monitor-for-claude | MIT | pushed 2026-09-22 | Windows tray (about 12.5 MB exe, no install); OAuth token from Claude Code; adaptive polling (slower when idle or locked, aligned to imminent resets, back-off on rate limit); "time-aware" alerts only when usage outpaces elapsed time; event commands; runs `claude update` to renew an expired session; toast identity via HKCU registry | [README](https://github.com/jens-duttke/usage-monitor-for-claude) |
| Others (ClaudeBar, claude-tray, nek0der/CodexBarWin, codex-minibar) | MIT/Apache-2.0 | small, active | Variations on the same OAuth/usage polling | [search](https://github.com/search?q=claude+usage+windows+tray&type=repositories) |

**Pulse today:** OAuth usage endpoint plus Claude Desktop's own cached `/usage` response (account-matched, zstd decoded, 30-minute freshness), Codex `wham/usage` with rate-limit-reset credits; 5-minute polling with back-off; never refreshes or writes credentials; "Sign-in expired; use the app once" when expired; not ported: `claude /usage`, Claude reset credits/spend windows.

**Verdicts**
- Account correctness (Desktop vs Code account, cache matching, "No reading for X yet" instead of another account's numbers): **Pulse ahead**; none of the trackers above handle two Claude accounts this carefully.
- Expired token: **Pulse behind (policy call).** The other tools recover (renew through the owning CLI). CodexBar's rule is the compatible one: Pulse itself never writes the file, it asks the owning CLI to refresh. A button or one-shot "refresh via Claude Code" fits Pulse's "never write credentials" rule; the owner decides.
- Fallback sources: **Pulse behind.** CodexBar's Codex CLI RPC (`codex app-server`) gives limits with no token handling in Pulse. Reasonable as a fallback when the HTTP read fails.
- Burn-down history and pace/"will I run out" (CodexBar, Claude-Code-Usage-Monitor, usage-monitor's time-aware alerts): **Pulse behind** (the Mac side already has UsagePace; parity row A8 lists it as not started on Windows). Alerts that only fire when usage outpaces elapsed time are the cheapest useful version.
- Adaptive polling (idle/locked, align to the reset time): **Pulse behind, small.** Pulse polls every 5 minutes regardless.
- Local cost/token scans (ccusage): **not needed** unless the owner wants spend; Pulse's mission is limits.

---

## 10. App plumbing

| Topic | Others | Pulse today | Verdict |
| --- | --- | --- | --- |
| Toast for an unpackaged app | Microsoft's desktop-toast guide: a desktop app cannot raise a toast without a Start-screen/All Programs shortcut, and the shortcut carries the AUMID (and, for activation, a toast activator CLSID). Windows App SDK `AppNotificationManager.Register` for unpackaged apps registers the process as COM server and takes name/icon from the shell (docs excerpt). `usage-monitor-for-claude` registers `HKCU\Software\Classes\AppUserModelId\<id>` (`DisplayName`, `IconUri`) and calls `SetCurrentProcessExplicitAppUserModelID`, a comment there says no Start Menu shortcut is required. Tauri's NSIS template has `SetLnkAppUserModelId`. `tauri-winrt-notification` (Apache-2.0) shows toasts in-process through WinRT and documents the PowerShell id as the stand-in for uninstalled apps. | PowerShell WinRT toast spawned per alert, attributed to "Windows PowerShell"; installer already creates `Pulse.lnk` without the property | **Pulse behind (cheap).** (1) In `pulse.nsi` set `System.AppUserModel.ID` on `Pulse.lnk` (port the macro idea from [utils.nsh](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-bundler/src/bundle/windows/nsis/utils.nsh)); (2) write the HKCU AppUserModelId name/icon at start as insurance; (3) show the toast in-process with `Windows.UI.Notifications` through the existing `windows` crate (the pattern in [tauri-winrt-notification](https://github.com/tauri-apps/winrt-notification)) and drop `powershell.exe`. Sources: [Microsoft: desktop toasts through an AppUserModelID](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/legacy/hh802762(v=vs.85)), [Register](https://learn.microsoft.com/en-us/windows/windows-app-sdk/api/winrt/microsoft.windows.appnotifications.appnotificationmanager.register), [win32.py](https://github.com/jens-duttke/usage-monitor-for-claude/blob/main/usage_monitor_for_claude/platforms/win32.py). Test on a clean VM: docs disagree on whether a shortcut is mandatory. |
| Single instance | `tauri-plugin-single-instance` pattern; named mutex | Per-user SID-scoped mutex `Local\Pulse.Pill.v1.<sid>`; hub claims its own mutex and signals a section event (`win_bridge.rs`) | **Matches/ahead.** windows/README "Hub side" paragraph is stale (it says the hub needs a guard). |
| Autostart | `auto-launch` crate (MIT) writes `Run` and the `StartupApproved\Run` override (enabled bytes `02 00 ...`); EcoPaste uses it | `HKCU\...\Run` only; `startup_status()` never reads the override | **Pulse behind (cheap, correctness).** On enable write the enabled override; on status read it so a Task Manager disable is reported as "off by Windows". [auto-launch](https://github.com/zzzgydi/auto-launch/blob/main/src/windows.rs). |
| Updater | Velopack (MIT, written in Rust, delta packages; its README's testimonials report updates with no UAC prompt; rollback not verified); WinSparkle (MIT; uses the Sparkle appcast format, as the Mac's Sparkle does) | GitHub latest release, SHA-256 from the API digest, Authenticode check, silent NSIS install, exit; full download each time; no rollback | **Matches** on integrity, **behind** on deltas. Velopack would replace the RightKit NSIS lane, so it is a release-engineering decision, not a quick win. [Velopack](https://github.com/velopack/velopack), [WinSparkle](https://github.com/vslavik/winsparkle). |
| Installer | NSIS per-user (zlib licence, what Tauri also uses), WiX (MS-RL, OSI-approved, [repo](https://github.com/wixtoolset/wix)), MSIX | Per-user NSIS, Authenticode signed, `taskkill /F` of running Pulse and hub before copying | **Matches.** MSIX would give package identity (toast, startup task, clean uninstall) but file-system/registry virtualisation could split the `pulse` CLI's view of `%LOCALAPPDATA%\Pulse` from the notch's if the CLI ever ran outside the package (unverified, check [Microsoft's packaged-app behaviour docs](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes) before considering). Consider a graceful `WM_CLOSE` first in the installer instead of `/F`. |
| Per-monitor DPI v2, layered windows | Same API family in Rainmeter and the island clones | PMv2, `UpdateLayeredWindow` per-pixel alpha, `WS_EX_NOACTIVATE`, size via per-monitor DPI multiplier | **Matches.** |
| Virtual desktops | Documented `IVirtualDesktopManager` can only move windows the calling process owns ([Microsoft blog](https://learn.microsoft.com/en-us/archive/blogs/winsdk/virtual-desktop-switching-in-windows-10)); pinning other windows needs undocumented interfaces ([VirtualDesktopAccessor](https://github.com/Ciantic/VirtualDesktopAccessor), MIT, Windows 11 24H2 build 26100.2605 or later) | Re-homes its own cloaked panel to the current desktop (`1b82ab3`) | **Matches the documented route**; do not depend on the undocumented pin interfaces. |
| Fullscreen/game detection for the notch | PowerToys modules and Rainmeter's `GameMode.cpp` ([game_mode.h](https://github.com/microsoft/PowerToys/blob/main/src/common/utils/game_mode.h), [Rainmeter GameMode.cpp](https://github.com/rainmeter/rainmeter/blob/master/Library/GameMode.cpp)) | Own geometry rule (`visibility.rs`), tested on borderless full-monitor windows | **Matches** on borderless; add `SHQueryUserNotificationState` as a second signal for exclusive D3D full screen and presentation mode. |
| Localisation | WinDirStat ships translations (changelog lists Swedish, Japanese, Turkish additions), WinSparkle via Crowdin, Velopack in 39 languages, LocalSend i18n | English only | **Pulse behind, low priority** for a personal tool. When it matters, keep notch strings in one table (they are mostly in `card.rs`/`layout.rs`) and use i18next on the hub side. |

---

## Licence and donor-rule summary

| Tool | Licence | Use under docs/donors.md |
| --- | --- | --- |
| Kudu rules | MIT | Reimplement data; keep MIT notice and the pinned commit in `docs/donors.md` (add a row; plan.md already lists Kudu) |
| Tauri NSIS template, tauri-winrt-notification, nvml-wrapper | Apache-2.0 (Tauri: Apache-2.0 or MIT) | Pattern or small snippet with notice; prefer reimplementation |
| auto-launch, trash-rs, UniGetUI, PowerToys, System Informer, Velopack, WinSparkle, Scrutiny, CrystalDiskInfo, sysinfo, dua, gdu | MIT | Pattern/reference; any copied snippet keeps notice |
| BCU, ntfs, mft, diskus (Apache-2.0 or MIT), Scoop (Unlicense or MIT), btop4win | Apache-2.0/MIT | Pattern/reference |
| LibreHardwareMonitor | MPL-2.0 | Run as a separate program (read its HTTP/WMI); file-level copyleft otherwise |
| WinDirStat, BleachBit, ShareX, Greenshot, Flameshot, AutoHotkey, Rainmeter, TaskExplorer, PairDrop, rquickshare | GPL-2.0/3.0 | **Reference only**, no code |
| smartmontools | GPL-2.0 | Separate program (as now); drive database is GPL data, ship with notice |
| PawnIO | GPL-2.0 with exception; modules LGPL-2.1 | Do not bundle or load; mention only as the reason temperatures need elevation |
| kanata | LGPL-3.0 | Reference only |
| TrafficMonitor | Anti-996 | **NON-OSI**: ideas only, no code |
| EarTrumpet | MIT plus named-entity exclusion | **Not a clean OSI licence**: ideas only |
| Winapp2.ini | CC-BY-SA-4.0 | **NON-OSI** data: do not import |
| DynamicWin | CC-BY-SA-4.0 | **NON-OSI** for software: ideas only |
| ADLX, IGCL, NVML | vendor licences | **NON-OSI**: load the user's driver DLL dynamically as Pulse does for NVML; do not vendor SDK headers without reading the licence |
| WizTree, HWiNFO, SpaceSniffer, Everything, CleanmgrPlus | closed or custom EULA | **NON-OSI**: observe behaviour from their docs only |

## Where Pulse is ahead (consolidated)

- Cleanup safety model and re-validation (BleachBit and Winapp2-based cleaners delete outright, and Winapp2's own disclaimer says "potentially irreversible"; Kudu trashes only user-chosen files as far as I checked).
- Process actions bound to `(pid, start_time)`, never auto-escalating.
- Live-updating storage index with lost-event handling; hard-link/allocation-aware sizing (diskus documents the opposite on Windows).
- Unelevated NVMe health (CrystalDiskInfo requires administrator by manifest setting).
- Native, no-focus, per-monitor-DPI layered notch (the other island clones found are Electron or WPF).
- Claude account-correct usage via Desktop's cache; per-interface LocalSend multicast with Windows virtual adapters skipped.
- Single-instance and settings writes (SID-scoped mutex, write-then-rename with restricted DACL).

## Unverified or needs a desktop

- Every Pulse behaviour above is from source; none has run on Windows hardware.
- The scan speed-up (item 1) and the Windows Search interim are predictions from API behaviour, not measurements.
- Recycle Bin edge cases (section 3) and whether toasts need the shortcut or only the HKCU identity (section 10) must be tested on a clean VM.
- PawnIO's device ACL and non-admin access: I could not confirm either way.
- ntfs-reader's licence and activity were not checked; "Everything" SDK licence was not checked.
- TrafficMonitor's README, Kudu's rule counts and PowerToys' KBM page were read directly; the WinDirStat 2.5 MFT note comes from its CHANGELOG (read directly).
- Doc drift noticed while reading (not edited): parity.md says no Windows duplicate reader (one exists), windows/README says the hub lacks a single-instance guard (`win_bridge.rs` has one), plan.md says 22 cleanup rules (46 in the file, 23 Windows-capable).

---

## Prioritised adoption list (user impact vs effort)

| # | Adopt | Impact | Effort | Where / pattern |
| --- | --- | --- | --- | --- |
| 1 | **Directory-record scan on Windows** (no per-file opens; volume serial once per directory; cache ancestor snapshots) | High: every Storage scan, search fallback, and live re-read; likely minutes to seconds on the 2M-entry walk (unmeasured) | Medium, in-house | New `core/src/platform/win_bulk.rs` like `mac_bulk.rs`; `list_with_class` already reads the right class; Rust `DirEntry::metadata` precedent |
| 2 | **Alt-as-Ctrl guardrails**: terminal exclusion (Ctrl+Shift+C/V or off), `SHQueryUserNotificationState` game guard, hook re-install on resume/unlock plus `GetAsyncKeyState` resync, `RegisterHotKey` for Alt+Shift+4/5 | High for daily use (silent failures and Ctrl+C interrupts) | Low to medium | `windows/src/keys.rs`; KBM docs, `game_mode.h`, `LowLevelKeyboardProc` docs, ShareX `WindowsHotkeyHost.cs`; keep default decision (parity Finding 4) with the owner |
| 3 | **Toast identity and in-process toasts**: AUMID on `Pulse.lnk`, HKCU identity, WinRT in-process | Medium to high: alerts say "Pulse", faster, no PowerShell window flashes or AV hits | Low | `scripts/release/windows/pulse.nsi` line 47, `windows/src/notify.rs`; Tauri `utils.nsh`, `tauri-winrt-notification` |
| 4 | **Launch-at-login honours Task Manager** (`StartupApproved\Run`) | Medium: removes a "setting says on, not running" trap | Low | `windows/src/autostart.rs`; `auto-launch` `windows.rs` |
| 5 | **Grow the Windows cleanup pack** from Kudu's `rules/win32` (Electron caches, shader caches, Chrome on-device models, dumps) with `deep_recency`, `cache_reset`, `needs_admin`; plus the three Recycle Bin edge-case journeys | High on a developer PC; safety-neutral if the journeys pass | Medium (data and schema) | `rules/cleanup.json`, `core/src/cleanup_scan.rs`; Kudu `RULES.md`, `WINDOWS_APP_CACHE_SAFETY.md` |
| 6 | **Windows Search index as the interim filename search** (labelled "indexed locations") | Medium to high: search without waiting for a scan | Medium | `hub/src-tauri/src/disk_index.rs` non-macOS branch; PowerToys `Microsoft.Plugin.Indexer` (`ISearchQueryHelper`) |
| 7 | **Per-process GPU** in Monitor and `pulse procs --sort gpu` | Medium | Low to medium | Parse PDH `GPU Engine(pid_*)` instances in `windows/src/sensors.rs`/core; System Informer `gpumon.c` for the alternative |
| 8 | **Drive health polish**: refreshed `drivedb.h` with `-B`, `--scan-open`, USB NVMe `-d snt*` retry, and share the hub's unelevated NVMe path with the notch Disks card | Medium | Low to medium | `windows/src/drive_health.rs`, `hub/src-tauri/src/health_windows.rs`, `scripts/release` smartctl step; `smartctl.8.in` |
| 9 | **Window screenshot through Windows.Graphics.Capture** (BitBlt fallback), plus GPU fan and power from NVML on the System card | Medium | Medium (capture), low (NVML) | `windows/src/shot.rs`, `windows/src/sensors.rs`; Greenshot `GraphicsCaptureBackend.cs`, `nvml.h` |
| 10 | **Optional elevated helper** (spike): Windows twin of `docs/helper.md`, enabling MFT/USN scan and replay, SATA SMART, system-owned cleanup, and a reader for LibreHardwareMonitor data; ship the LHM `data.json` reader first as the no-driver half | High ceiling, but high cost and a new trust boundary | High (design, signing, ACLs); LHM reader low | Everything 1.5 "indexing process as administrator", WizTree "highest privileges" task, WinDirStat 2.5 MFT mode, `docs/helper.md` boundary rules |

Below the line (worth doing when adjacent work is open): Store apps through `PackageManager` instead of PowerShell; BCU-style read-only leftover categories; Claude token renewal delegated to the owning CLI and the Codex `app-server` fallback; time-aware usage alerts and adaptive polling; Velopack-style delta updates; eject veto naming the holding process (File Locksmith pattern: [PowerToys File Locksmith](https://github.com/microsoft/PowerToys/tree/main/src/modules/FileLocksmith)); a Command Palette extension instead of a Windows launcher; localisation scaffolding.
