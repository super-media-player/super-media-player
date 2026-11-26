; Windows installer of Super Media Player (built by scripts/package-windows.sh
; with makensis). Installs for all users into Program Files, adds Start menu
; and optional desktop shortcuts, "Open with" for media files, and an
; uninstaller in Settings → Apps.
;
; makensis -DVERSION=0.59.0 -DSTAGE=<folder with the app> -DOUT=<setup.exe> installer.nsi

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel admin
!include "MUI2.nsh"
!include "x64.nsh"

!define APP "Super Media Player"
!define EXE "super-media-player.exe"
!define UNINST "Software\Microsoft\Windows\CurrentVersion\Uninstall\SuperMediaPlayer"

Name "${APP}"
OutFile "${OUT}"
InstallDir "$PROGRAMFILES64\${APP}"
InstallDirRegKey HKLM "Software\SuperMediaPlayer" "InstallDir"
!define MUI_ICON "${STAGE}\smp.ico"
!define MUI_UNICON "${STAGE}\smp.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE}\LICENSE.txt"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"

; Offer the player in "Open with" for one file type.
!macro OpenWith EXT
  WriteRegStr HKLM "Software\Classes\${EXT}\OpenWithProgids" "SuperMediaPlayer.Media" ""
  WriteRegStr HKLM "Software\Classes\Applications\${EXE}\SupportedTypes" "${EXT}" ""
!macroend

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "${APP} needs 64-bit Windows."
    Abort
  ${EndIf}
  SetRegView 64
FunctionEnd

Section "${APP}" SecApp
  SectionIn RO
  SetOutPath "$INSTDIR"
  File /r "${STAGE}\*.*"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\smp.ico"
  WriteRegStr HKLM "Software\SuperMediaPlayer" "InstallDir" "$INSTDIR"
  ; Settings → Apps
  WriteRegStr HKLM "${UNINST}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINST}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST}" "DisplayIcon" "$INSTDIR\smp.ico"
  WriteRegStr HKLM "${UNINST}" "Publisher" "Super Media Player"
  WriteRegStr HKLM "${UNINST}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINST}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "${UNINST}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST}" "NoRepair" 1
  ; "Open with" for media files (Windows asks before it becomes the default).
  WriteRegStr HKLM "Software\Classes\Applications\${EXE}" "FriendlyAppName" "${APP}"
  WriteRegStr HKLM "Software\Classes\Applications\${EXE}\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'
  WriteRegStr HKLM "Software\Classes\SuperMediaPlayer.Media" "" "Media file"
  WriteRegStr HKLM "Software\Classes\SuperMediaPlayer.Media\DefaultIcon" "" "$INSTDIR\smp.ico"
  WriteRegStr HKLM "Software\Classes\SuperMediaPlayer.Media\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'
  !insertmacro OpenWith ".mp4"
  !insertmacro OpenWith ".mkv"
  !insertmacro OpenWith ".webm"
  !insertmacro OpenWith ".avi"
  !insertmacro OpenWith ".mov"
  !insertmacro OpenWith ".m4v"
  !insertmacro OpenWith ".ts"
  !insertmacro OpenWith ".mpg"
  !insertmacro OpenWith ".wmv"
  !insertmacro OpenWith ".flv"
  !insertmacro OpenWith ".mp3"
  !insertmacro OpenWith ".flac"
  !insertmacro OpenWith ".ogg"
  !insertmacro OpenWith ".opus"
  !insertmacro OpenWith ".m4a"
  !insertmacro OpenWith ".aac"
  !insertmacro OpenWith ".wav"
  !insertmacro OpenWith ".wma"
  !insertmacro OpenWith ".m3u"
  !insertmacro OpenWith ".m3u8"
  !insertmacro OpenWith ".pls"
  !insertmacro OpenWith ".iso"
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\smp.ico"
SectionEnd

Section "Uninstall"
  SetRegView 64
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKLM "${UNINST}"
  DeleteRegKey HKLM "Software\SuperMediaPlayer"
  DeleteRegKey HKLM "Software\Classes\Applications\${EXE}"
  DeleteRegKey HKLM "Software\Classes\SuperMediaPlayer.Media"
  RMDir /r "$INSTDIR"
SectionEnd
