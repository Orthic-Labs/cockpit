; Pulse per-user installer (NSIS). Built by scripts/release/windows-payload.mjs from the
; Authenticode-signed payload; the installer itself is signed afterwards by RightKit's signer.
; Defines (from makensis /D): STAGE (payload dir), OUT (installer path), VERSION (x.y.z).
; No elevation. The Start-up Run value belongs to the app (autostart.rs); the installer never
; writes it, and the uninstaller removes it.
Unicode true
ManifestDPIAware true
Name "Pulse"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\Pulse"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails nevershow
ShowUninstDetails nevershow

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Pulse"
VIAddVersionKey "CompanyName" "Damned Ventures LLC"
VIAddVersionKey "FileDescription" "Pulse installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Pulse"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Pulse"
  SetOutPath "$INSTDIR"
  ; Upgrade in place: release file locks held by a running notch or hub.
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM Pulse.exe /IM pulse-hub.exe'
  Pop $0
  Sleep 500
  File /r "${STAGE}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Pulse.lnk" "$INSTDIR\Pulse.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "Pulse"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Damned Ventures LLC"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\Pulse.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM Pulse.exe /IM pulse-hub.exe'
  Pop $0
  Sleep 500
  ; The app's launch-at-login entry (HKCU Run) must not outlive the app.
  DeleteRegValue HKCU "${RUN_KEY}" "Pulse"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  Delete "$SMPROGRAMS\Pulse.lnk"
  ; Only what the installer laid down; user data lives outside the install directory.
  Delete "$INSTDIR\Pulse.exe"
  Delete "$INSTDIR\pulse-hub.exe"
  Delete "$INSTDIR\NOTICE.txt"
  Delete "$INSTDIR\LICENSE.txt"
  RMDir /r "$INSTDIR\Helpers"
  RMDir /r "$INSTDIR\ThirdParty"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
