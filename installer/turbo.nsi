Unicode True
!include "MUI2.nsh"

Name "Turbo"
OutFile "..\dist\TurboApp-Setup.exe"
VIProductVersion "1.1.0.0"
VIFileVersion "1.1.0.0"
VIAddVersionKey "ProductName" "Turbo App"
VIAddVersionKey "CompanyName" "NoShaar"
VIAddVersionKey "FileVersion" "1.1.0"
VIAddVersionKey "FileDescription" "Instalador do Turbo App"
VIAddVersionKey "OriginalFilename" "TurboApp-Setup.exe"
VIAddVersionKey "LegalCopyright" "NoShaar"
InstallDir "$LOCALAPPDATA\TurboApp\app"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "PortugueseBR"

Function .onInit
  ; Dá tempo do processo anterior soltar o exe numa atualização.
  Sleep 1500
FunctionEnd

Section "Turbo" SecMain
  SetOutPath "$INSTDIR"
  File "..\dist\turbo-app.exe"
  ; A pasta de dados (backup.json e log.jsonl) fica em $LOCALAPPDATA\TurboApp,
  ; fora desta pasta app. Atualizar ou desinstalar não apaga o backup.
  SetOutPath "$LOCALAPPDATA\TurboApp"
  File /nonfatal "..\dist\update-source.url"

  CreateShortcut "$DESKTOP\Turbo.lnk" "$INSTDIR\turbo-app.exe"
  CreateDirectory "$SMPROGRAMS\Turbo"
  CreateShortcut "$SMPROGRAMS\Turbo\Turbo.lnk" "$INSTDIR\turbo-app.exe"
  CreateShortcut "$SMPROGRAMS\Turbo\Desinstalar Turbo.lnk" "$INSTDIR\Uninstall.exe"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\turbo-app.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$DESKTOP\Turbo.lnk"
  Delete "$SMPROGRAMS\Turbo\Turbo.lnk"
  Delete "$SMPROGRAMS\Turbo\Desinstalar Turbo.lnk"
  RMDir "$SMPROGRAMS\Turbo"
  ; De propósito: não apaga $LOCALAPPDATA\TurboApp\backup.json nem log.jsonl.
SectionEnd
