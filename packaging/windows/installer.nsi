; Implements docs/16: per-user desktop installation, optional separate GPL helper.
Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
Name "Planyrith ${VERSION}"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\Planyrith"
InstallDirRegKey HKCU "Software\Planyrith" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "${ICON}"
UninstallIcon "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\planyrith-gui.exe"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${PAYLOAD}\licenses\INDEX.md"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Italian"

Section "Planyrith" Core
  SectionIn RO
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File "${PAYLOAD}\planyrith-gui.exe"
  File "${PAYLOAD}\planyrith.exe"
  File "${PAYLOAD}\INSTALL.md"
  File "${PAYLOAD}\CHANGELOG.md"
  File "${PAYLOAD}\BUILD-INFO.txt"
  File "${PAYLOAD}\Planyrith.ico"
  SetOutPath "$INSTDIR\licenses"
  File /r "${PAYLOAD}\licenses\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Planyrith"
  CreateShortcut "$SMPROGRAMS\Planyrith\Planyrith.lnk" "$INSTDIR\planyrith-gui.exe" "" "$INSTDIR\Planyrith.ico"
  CreateShortcut "$SMPROGRAMS\Planyrith\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Planyrith" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith" "DisplayName" "Planyrith"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith" "Publisher" "KvasFan"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith" "DisplayIcon" "$INSTDIR\Planyrith.ico"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith" "NoRepair" 1
SectionEnd

Section "SKP / SKB support (separate GPL-3.0-only module)" Skp
  SetOutPath "$INSTDIR"
  File "${PAYLOAD}\planyrith-skp.exe"
SectionEnd

Section "Native PLYR file association" Association
  WriteRegStr HKCU "Software\Classes\Planyrith.Model" "" "Planyrith model"
  WriteRegStr HKCU "Software\Classes\Planyrith.Model\DefaultIcon" "" "$INSTDIR\Planyrith.ico,0"
  WriteRegStr HKCU "Software\Classes\Planyrith.Model\shell\open\command" "" '"$INSTDIR\planyrith-gui.exe" "%1"'
  WriteRegStr HKCU "Software\Classes\.plyr\OpenWithProgids" "Planyrith.Model" ""
  ; Use OpenWith, leaving an existing default association untouched.
  ReadRegStr $0 HKCU "Software\Classes\.plyr" ""
  ${If} $0 == ""
    WriteRegStr HKCU "Software\Classes\.plyr" "" "Planyrith.Model"
  ${EndIf}
SectionEnd

Section "Desktop shortcut" Desktop
  SetShellVarContext current
  CreateShortcut "$DESKTOP\Planyrith.lnk" "$INSTDIR\planyrith-gui.exe" "" "$INSTDIR\Planyrith.ico"
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\planyrith-gui.exe"
  Delete "$INSTDIR\planyrith.exe"
  Delete "$INSTDIR\planyrith-skp.exe"
  Delete "$INSTDIR\INSTALL.md"
  Delete "$INSTDIR\CHANGELOG.md"
  Delete "$INSTDIR\BUILD-INFO.txt"
  Delete "$INSTDIR\Planyrith.ico"
  RMDir /r "$INSTDIR\licenses"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Planyrith\Planyrith.lnk"
  Delete "$SMPROGRAMS\Planyrith\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Planyrith"
  Delete "$DESKTOP\Planyrith.lnk"
  ReadRegStr $0 HKCU "Software\Classes\.plyr" ""
  ${If} $0 == "Planyrith.Model"
    DeleteRegValue HKCU "Software\Classes\.plyr" ""
  ${EndIf}
  DeleteRegValue HKCU "Software\Classes\.plyr\OpenWithProgids" "Planyrith.Model"
  DeleteRegKey HKCU "Software\Classes\Planyrith.Model"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Planyrith"
  DeleteRegKey HKCU "Software\Planyrith"
SectionEnd
