; Windows installer for the packaged LocalSend app.
;
; Built by .github/workflows/release.yml after `nsh package --target windows`:
;
;   makensis /DAPP_VERSION=1.0.0 /DAPP_VERSION_NUM=1.0.0.0 ^
;     /DSOURCE_DIR=<repo>\.nsh\out\LocalSend ^
;     /DOUT_FILE=<repo>\dist\LocalSend-<version>-windows-x86_64-setup.exe ^
;     scripts\windows-installer.nsi
;
; Per-user install (no UAC): %LOCALAPPDATA%\Programs\LocalSend, with Start Menu
; and desktop shortcuts, an uninstaller, and an entry under Apps & features.
; The packaging directory is self-contained (LocalSend.exe plus its `nsh`
; assets), so the installer only copies it and creates the shortcuts.
;
; Kept ASCII-only: NSIS reads `.nsi` without a BOM as ANSI, and the standard
; page texts of both languages come from MUI's language files.

!include "MUI2.nsh"

!ifndef APP_VERSION
  !define APP_VERSION "1.0.0"
!endif
!ifndef APP_VERSION_NUM
  !define APP_VERSION_NUM "1.0.0.0"
!endif
!ifndef SOURCE_DIR
  !define SOURCE_DIR ".nsh\out\LocalSend"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "dist\LocalSend-windows-x86_64-setup.exe"
!endif

Unicode true

Name "LocalSend"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\LocalSend"
InstallDirRegKey HKCU "Software\LocalSend" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${APP_VERSION_NUM}"
VIAddVersionKey "ProductName" "LocalSend"
VIAddVersionKey "FileDescription" "LocalSend Desktop installer"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "ProductVersion" "${APP_VERSION}"
VIAddVersionKey "LegalCopyright" "MIT License"

; The packaged .exe carries the app icon as a PNG resource, which makensis
; cannot read as an installer icon; keep NSIS's default icon rather than
; failing the build.
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\LocalSend.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start LocalSend"
!define MUI_LANGDLL_REGISTRY_ROOT "HKCU"
!define MUI_LANGDLL_REGISTRY_KEY "Software\LocalSend"
!define MUI_LANGDLL_REGISTRY_VALUENAME "Installer Language"

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

Function .onInit
  !insertmacro MUI_LANGDLL_DISPLAY
FunctionEnd

Section "LocalSend" SEC_MAIN
  SetOutPath "$INSTDIR"
  File /r "${SOURCE_DIR}\*.*"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\LocalSend"
  CreateShortCut "$SMPROGRAMS\LocalSend\LocalSend.lnk" "$INSTDIR\LocalSend.exe"
  CreateShortCut "$SMPROGRAMS\LocalSend\Uninstall LocalSend.lnk" "$INSTDIR\uninstall.exe"
  CreateShortCut "$DESKTOP\LocalSend.lnk" "$INSTDIR\LocalSend.exe"

  WriteRegStr HKCU "Software\LocalSend" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "DisplayName" "LocalSend"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "Publisher" "starriv"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "DisplayIcon" "$INSTDIR\LocalSend.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "NoRepair" 1
SectionEnd

Section "Uninstall"
  RMDir /r "$SMPROGRAMS\LocalSend"
  Delete "$DESKTOP\LocalSend.lnk"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend"
  DeleteRegKey HKCU "Software\LocalSend"
  RMDir /r "$INSTDIR"
SectionEnd
