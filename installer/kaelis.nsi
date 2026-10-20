Unicode true
ManifestDPIAware true

!include "MUI2.nsh"
!include "FileFunc.nsh"

!define APP_NAME "Kaelis"
!define APP_VERSION "0.1.0"
!define APP_PUBLISHER "Kaelis Labs"
!define APP_URL "https://github.com/epufk/kaelis"
!define APP_EXE "kaelis.exe"
!define REG_UNINSTALL "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kaelis"

Name "${APP_NAME}"
OutFile "..\dist\Kaelis-Setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Kaelis"
InstallDirRegKey HKCU "Software\Kaelis" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 64

VIProductVersion "0.1.0.0"
VIAddVersionKey /LANG=1033 "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "CompanyName" "${APP_PUBLISHER}"
VIAddVersionKey /LANG=1033 "FileDescription" "Kaelis Minecraft launcher installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Copyright (c) 2026 Kaelis Labs"
VIAddVersionKey /LANG=1033 "OriginalFilename" "Kaelis-Setup.exe"

!define MUI_ABORTWARNING
!define MUI_ICON "..\assets\kaelis.ico"
!define MUI_UNICON "..\assets\kaelis.ico"
!define MUI_WELCOMEPAGE_TITLE "Install Kaelis"
!define MUI_WELCOMEPAGE_TEXT "A native Rust workspace for Minecraft instances, mods, resource packs, shaders, and worlds.$\r$\n$\r$\nThis per-user install needs no administrator rights. Your instances and accounts stay in your own Windows profile."
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Open Kaelis"
!define MUI_FINISHPAGE_LINK "Kaelis on GitHub"
!define MUI_FINISHPAGE_LINK_LOCATION "${APP_URL}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "Kaelis" SecMain
  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "..\dist\Kaelis.exe"
  File "..\LICENSE"
  File "..\README.md"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\Kaelis"
  CreateShortcut "$SMPROGRAMS\Kaelis\Kaelis.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
  CreateShortcut "$SMPROGRAMS\Kaelis\Uninstall Kaelis.lnk" "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "Software\Kaelis" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "Publisher" "${APP_PUBLISHER}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKCU "${REG_UNINSTALL}" "QuietUninstallString" "$\"$INSTDIR\Uninstall.exe$\" /S"
  WriteRegStr HKCU "${REG_UNINSTALL}" "URLInfoAbout" "${APP_URL}"
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "NoModify" 1
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "NoRepair" 1

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "EstimatedSize" "$0"
SectionEnd

Section "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\Kaelis.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\Kaelis.lnk"
  Delete "$SMPROGRAMS\Kaelis\Kaelis.lnk"
  Delete "$SMPROGRAMS\Kaelis\Uninstall Kaelis.lnk"
  RMDir "$SMPROGRAMS\Kaelis"
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Kaelis"
  DeleteRegKey HKCU "${REG_UNINSTALL}"
SectionEnd
