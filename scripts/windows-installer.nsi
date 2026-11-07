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
;
; In-app updates run this installer as `setup.exe /S /UPDATE` and then quit
; (src/update/install.rs): /UPDATE waits for the running LocalSend.exe to
; exit, keeps the shortcuts the user removed removed, and starts the app again
; when the files are in place - unless /NORUN is passed too (the update is
; installed as the user quits the app).
;
; Kept ASCII-only: NSIS reads `.nsi` without a BOM as ANSI, and the standard
; page texts of both languages come from MUI's language files.

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

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

Var Updating
Var NoRun

Function .onInit
  StrCpy $Updating 0
  StrCpy $NoRun 0
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    StrCpy $Updating 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/NORUN" $R1
  ${IfNot} ${Errors}
    StrCpy $NoRun 1
  ${EndIf}
  !insertmacro MUI_LANGDLL_DISPLAY
FunctionEnd

; Waits up to 30 s for a running LocalSend.exe to exit: a running image cannot
; be opened for writing, so the loop ends once the app has quit.
Function WaitForApp
  StrCpy $R2 0
  ${DoWhile} ${FileExists} "$INSTDIR\LocalSend.exe"
    ClearErrors
    FileOpen $R3 "$INSTDIR\LocalSend.exe" a
    ${IfNot} ${Errors}
      FileClose $R3
      ${Break}
    ${EndIf}
    IntOp $R2 $R2 + 1
    ${If} $R2 > 150
      ${Break}
    ${EndIf}
    Sleep 200
  ${Loop}
FunctionEnd

Section "LocalSend" SEC_MAIN
  ${If} $Updating == 1
    Call WaitForApp
  ${EndIf}
  SetOutPath "$INSTDIR"
  File /r "${SOURCE_DIR}\*.*"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\LocalSend"
  CreateShortCut "$SMPROGRAMS\LocalSend\LocalSend.lnk" "$INSTDIR\LocalSend.exe"
  CreateShortCut "$SMPROGRAMS\LocalSend\Uninstall LocalSend.lnk" "$INSTDIR\uninstall.exe"
  ${If} $Updating == 0
    CreateShortCut "$DESKTOP\LocalSend.lnk" "$INSTDIR\LocalSend.exe"
  ${EndIf}

  WriteRegStr HKCU "Software\LocalSend" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "DisplayName" "LocalSend"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "Publisher" "starriv"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "DisplayIcon" "$INSTDIR\LocalSend.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend" "NoRepair" 1

  ; A silent install shows no finish page (and no "Start LocalSend" box).
  ${If} $Updating == 1
  ${AndIf} $NoRun == 0
    Exec '"$INSTDIR\LocalSend.exe"'
  ${EndIf}
SectionEnd

Section "Uninstall"
  RMDir /r "$SMPROGRAMS\LocalSend"
  Delete "$DESKTOP\LocalSend.lnk"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalSend"
  DeleteRegKey HKCU "Software\LocalSend"
  RMDir /r "$INSTDIR"
SectionEnd
