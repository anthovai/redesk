; ReDesk agent installer for Windows (NSIS 3, per-user: no administrator rights needed).
;
; Build:   deploy/windows/build-installer.sh            (see that script for options)
; Install: double-click, or silently for mass deployment (GPO / Intune / RMM):
;   ReDesk-Agent-Setup.exe /S /SERVER=https://redesk.example.com /KEY=<server key> [/TOKEN=rdk_...]
; Uninstall silently: "%LOCALAPPDATA%\Programs\ReDesk\uninstall.exe" /S   (keeps device identity)

Unicode true
!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef SRC
  !define SRC "..\..\agent\target\x86_64-pc-windows-gnu\release"
!endif
!ifndef ASSETS
  !define ASSETS "..\..\agent\assets"
!endif
; Pre-configured installers carry the organisation's server so users only click "Next".
!ifndef SERVER_URL
  !define SERVER_URL ""
!endif
!ifndef SERVER_KEY
  !define SERVER_KEY ""
!endif
!ifndef OUTFILE
  !define OUTFILE "ReDesk-Agent-Setup-${VERSION}.exe"
!endif

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ReDesk"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
; redesk://connect/<id> links open the app (per user, like RustDesk's rustdesk:// scheme).
!define URL_KEY "Software\Classes\redesk"

Name "ReDesk"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\ReDesk"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "ReDesk ${VERSION}"
ShowInstDetails nevershow

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "ReDesk"
VIAddVersionKey /LANG=1033 "FileDescription" "ReDesk agent setup"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "ReDesk"

!define MUI_ICON "${ASSETS}\redesk.ico"
!define MUI_UNICON "${ASSETS}\redesk.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\redesk.exe"
!define MUI_FINISHPAGE_RUN_TEXT "$(RunNow)"

!insertmacro MUI_PAGE_WELCOME
Page custom ServerPage ServerPageLeave
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "Thai"
!insertmacro MUI_LANGUAGE "English"

LangString ServerTitle ${LANG_THAI} "เชื่อมต่อกับเซิร์ฟเวอร์ ReDesk"
LangString ServerTitle ${LANG_ENGLISH} "Connect to your ReDesk server"
LangString ServerSub ${LANG_THAI} "เว้นว่างไว้ได้ แล้วเข้าสู่ระบบด้วยบัญชี ReDesk ในโปรแกรม (ข้อมูลกุญแจอยู่ในคอนโซล หน้า ตั้งค่าเซิร์ฟเวอร์)"
LangString ServerSub ${LANG_ENGLISH} "Optional: leave empty and sign in with your ReDesk account in the app instead (keys are under Server settings in the console)."
LangString LblServer ${LANG_THAI} "ที่อยู่เซิร์ฟเวอร์"
LangString LblServer ${LANG_ENGLISH} "Server address"
LangString LblKey ${LANG_THAI} "กุญแจสาธารณะของเซิร์ฟเวอร์"
LangString LblKey ${LANG_ENGLISH} "Server public key"
LangString LblToken ${LANG_THAI} "Enrollment token (ถ้าผู้ดูแลให้มา)"
LangString LblToken ${LANG_ENGLISH} "Enrollment token (if your administrator gave you one)"
LangString ErrKey ${LANG_THAI} "กรุณาใส่กุญแจสาธารณะของเซิร์ฟเวอร์"
LangString ErrKey ${LANG_ENGLISH} "Please enter the server public key."
LangString ErrChars ${LANG_THAI} "ข้อมูลมีอักขระที่ไม่อนุญาต (ช่องว่างหรือเครื่องหมายคำพูด)"
LangString ErrChars ${LANG_ENGLISH} "The values contain characters that are not allowed (spaces or quotes)."
LangString ErrScheme ${LANG_THAI} "ที่อยู่เซิร์ฟเวอร์ต้องขึ้นต้นด้วย https://"
LangString ErrScheme ${LANG_ENGLISH} "The server address must start with https://"
LangString InitFailed ${LANG_THAI} "ตั้งค่าการเชื่อมต่อไม่สำเร็จ สามารถตั้งค่าใหม่ได้ในโปรแกรม ReDesk:"
LangString InitFailed ${LANG_ENGLISH} "Could not configure the connection. You can set it up in the ReDesk app:"
LangString RunNow ${LANG_THAI} "เปิด ReDesk ตอนนี้"
LangString RunNow ${LANG_ENGLISH} "Start ReDesk now"
LangString RemoveData ${LANG_THAI} "ลบการตั้งค่าและตัวตนของเครื่องนี้ด้วยหรือไม่?$\r$\n(ถ้าลบ เครื่องจะได้ ID ใหม่เมื่อติดตั้งอีกครั้ง)"
LangString RemoveData ${LANG_ENGLISH} "Also remove settings and this device's identity?$\r$\n(If removed, the device gets a new ID when reinstalled.)"

Var Server
Var Key
Var Token
Var Configured
Var hServer
Var hKey
Var hToken

; Sets $0 to 1 if $1 contains a space, tab or double quote (values end up on a command line).
Function HasBadChars
  StrCpy $0 0
  StrCpy $2 0
  loop:
    StrCpy $3 $1 1 $2
    StrCmp $3 "" done
    StrCmp $3 " " bad
    StrCmp $3 "$\t" bad
    StrCmp $3 '"' bad
    IntOp $2 $2 + 1
    Goto loop
  bad:
    StrCpy $0 1
  done:
FunctionEnd

Function .onInit
  StrCpy $Server "${SERVER_URL}"
  StrCpy $Key "${SERVER_KEY}"
  StrCpy $Token ""
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/SERVER=" $R1
  ${IfNot} ${Errors}
    StrCpy $Server $R1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/KEY=" $R1
  ${IfNot} ${Errors}
    StrCpy $Key $R1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/TOKEN=" $R1
  ${IfNot} ${Errors}
    StrCpy $Token $R1
  ${EndIf}
  StrCpy $Configured 0
  ${If} ${FileExists} "$APPDATA\ReDesk\agent.toml"
    StrCpy $Configured 1
  ${EndIf}
FunctionEnd

Function ServerPage
  ; Upgrades keep the existing identity and server settings.
  ${If} $Configured == 1
    Abort
  ${EndIf}
  !insertmacro MUI_HEADER_TEXT "$(ServerTitle)" "$(ServerSub)"
  nsDialogs::Create 1018
  Pop $R0
  ${NSD_CreateLabel} 0 0 100% 12u "$(LblServer)"
  ${NSD_CreateText} 0 13u 100% 13u "$Server"
  Pop $hServer
  ${NSD_CreateLabel} 0 34u 100% 12u "$(LblKey)"
  ${NSD_CreateText} 0 47u 100% 13u "$Key"
  Pop $hKey
  ${NSD_CreateLabel} 0 68u 100% 12u "$(LblToken)"
  ${NSD_CreateText} 0 81u 100% 13u "$Token"
  Pop $hToken
  nsDialogs::Show
FunctionEnd

Function ServerPageLeave
  ${NSD_GetText} $hServer $Server
  ${NSD_GetText} $hKey $Key
  ${NSD_GetText} $hToken $Token
  ${If} $Server == ""
    Return ; configure later in the app
  ${EndIf}
  StrCpy $1 $Server 8
  ${If} $1 != "https://"
    MessageBox MB_ICONEXCLAMATION "$(ErrScheme)"
    Abort
  ${EndIf}
  ${If} $Key == ""
    MessageBox MB_ICONEXCLAMATION "$(ErrKey)"
    Abort
  ${EndIf}
  ${ForEach} $R9 1 3 + 1
    ${If} $R9 == 1
      StrCpy $1 $Server
    ${ElseIf} $R9 == 2
      StrCpy $1 $Key
    ${Else}
      StrCpy $1 $Token
    ${EndIf}
    Call HasBadChars
    ${If} $0 == 1
      MessageBox MB_ICONEXCLAMATION "$(ErrChars)"
      Abort
    ${EndIf}
  ${Next}
FunctionEnd

Section "ReDesk" SecMain
  ; Replace a running copy (upgrade).
  nsExec::Exec 'taskkill /IM redesk.exe /F'
  Pop $0
  Sleep 800

  SetOutPath "$INSTDIR"
  File "${SRC}\redesk.exe"
  File "${SRC}\redesk-agent.exe"
  File "${ASSETS}\redesk.ico"

  ; First install with server details: create this device's identity now.
  ${If} $Configured != 1
  ${AndIf} $Server != ""
    StrCpy $1 $Server
    Call HasBadChars
    StrCpy $R5 $0
    StrCpy $1 $Key
    Call HasBadChars
    IntOp $R5 $R5 | $0
    StrCpy $1 $Token
    Call HasBadChars
    IntOp $R5 $R5 | $0
    ${If} $R5 == 0
      StrCpy $R2 ""
      ${If} $Token != ""
        StrCpy $R2 '--enroll-token "$Token"'
      ${EndIf}
      nsExec::ExecToStack '"$INSTDIR\redesk-agent.exe" init --server "$Server" --key "$Key" $R2'
      Pop $0
      Pop $1
      ${If} $0 != 0
        DetailPrint "init failed: $1"
        MessageBox MB_ICONEXCLAMATION "$(InitFailed)$\r$\n$\r$\n$1" /SD IDOK
      ${EndIf}
    ${EndIf}
  ${EndIf}

  ; Start in the tray at logon (per user: screen capture needs the user's desktop session).
  WriteRegStr HKCU "${RUN_KEY}" "ReDesk" '"$INSTDIR\redesk.exe" --background'

  WriteRegStr HKCU "${URL_KEY}" "" "URL:ReDesk"
  WriteRegStr HKCU "${URL_KEY}" "URL Protocol" ""
  WriteRegStr HKCU "${URL_KEY}\DefaultIcon" "" '"$INSTDIR\redesk.exe",0'
  WriteRegStr HKCU "${URL_KEY}\shell\open\command" "" '"$INSTDIR\redesk.exe" "%1"'

  CreateShortcut "$SMPROGRAMS\ReDesk.lnk" "$INSTDIR\redesk.exe" "" "$INSTDIR\redesk.ico"
  CreateShortcut "$DESKTOP\ReDesk.lnk" "$INSTDIR\redesk.exe" "" "$INSTDIR\redesk.ico"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "ReDesk"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "ReDesk"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\redesk.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" $0

  ; Silent installs have no finish page: start the agent in the tray right away.
  ${If} ${Silent}
    Exec '"$INSTDIR\redesk.exe" --background'
  ${EndIf}
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM redesk.exe /F'
  Pop $0
  Sleep 800
  DeleteRegValue HKCU "${RUN_KEY}" "ReDesk"
  DeleteRegKey HKCU "${URL_KEY}"
  Delete "$SMPROGRAMS\ReDesk.lnk"
  Delete "$DESKTOP\ReDesk.lnk"
  Delete "$INSTDIR\redesk.exe"
  Delete "$INSTDIR\redesk-agent.exe"
  Delete "$INSTDIR\redesk.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINST_KEY}"
  MessageBox MB_YESNO|MB_ICONQUESTION "$(RemoveData)" /SD IDNO IDNO keep
    RMDir /r "$APPDATA\ReDesk"
    RMDir /r "$LOCALAPPDATA\ReDesk"
  keep:
SectionEnd
