; SPDX-License-Identifier: MPL-2.0
;
; The Windows installer (docs/features/linux/NIGHTLY.rev2.md §12, N18 and N19):
; the zip's files, installed for this user only, with a Start-menu and a
; desktop shortcut and an entry in Settings -> Apps. It is for the first
; install only. Koru updates itself from the zip in the feed afterwards, which
; works because the directory is the user's own and writable.
;
; Not run by hand: `koru-release` (crates/koru-release/src/installer.rs) writes
; a wrapper that defines the values below and includes this file, then signs
; the result. The defines:
;
;   PRODUCT     "Koru Nightly": the name shown everywhere, one per channel
;   VERSION     the release version, as the zip's VERSION file says it
;   VERSION4    its four-part Windows spelling (koru-winres::package_version)
;   STAGE       the staged package directory, the zip's contents
;   OUTFILE     where setup.exe is written
;   ICON        the channel's icon
;   IDENTITY    the channel's notification identity, "Koru.Desktop.Nightly";
;               also the name of the uninstall key
;   AUTOSTART   the channel's start-at-sign-in value, "Koru (nightly)"
;   UNINSTALL_SIGN_COMMAND  optional: signs the uninstaller, "%1" the file

Unicode true
ManifestDPIAware true
; **This user only** (N19): no administrator prompt, and a directory the
; updater can write.
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "nsDialogs.nsh"

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${IDENTITY}"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define UNINSTALLER "Uninstall ${PRODUCT}.exe"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
; One directory per channel, so a nightly and a beta install side by side.
; The person may choose another (CheckDirectory says which), and Setup run
; again finds the one chosen last time, so an upgrade lands on the install
; rather than beside it.
InstallDir "$LOCALAPPDATA\Programs\${PRODUCT}"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
BrandingText "${PRODUCT} ${VERSION}"
ShowInstDetails nevershow
ShowUninstDetails nevershow

VIProductVersion "${VERSION4}"
VIFileVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${PRODUCT} Setup"
VIAddVersionKey "CompanyName" "Koru Works, Inc."
VIAddVersionKey "LegalCopyright" "Koru Works, Inc. Mozilla Public License 2.0."

; The uninstaller is written into setup.exe while it is compiled, so it can
; only be signed here, before it is packed; setup.exe itself is signed after.
!ifdef UNINSTALL_SIGN_COMMAND
  !uninstfinalize `${UNINSTALL_SIGN_COMMAND}` = 0
!endif

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\koru.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${PRODUCT}"

; **Nothing happens until the person says Install** (rev2 §12): a page
; that says what Setup does, then the folder, whose button reads Install
; because it is the last page before the files.
Var Situation
!define MUI_WELCOMEPAGE_TITLE "Install ${PRODUCT}"
!define MUI_WELCOMEPAGE_TEXT "Setup will install ${PRODUCT} ${VERSION} for you only. No administrator rights are needed, and Koru keeps itself up to date after this.$\r$\n$\r$\n$Situation$\r$\n$\r$\nClick Next to choose where it goes."

Function .onInit
  ${If} ${FileExists} "$INSTDIR\koru.exe"
    StrCpy $Situation "${PRODUCT} is already installed in $INSTDIR, so Setup will upgrade it. If it is running, Setup closes it first; your workspaces are not touched."
  ${Else}
    StrCpy $Situation "If ${PRODUCT} is running from somewhere else, Setup closes it first; your workspaces are not touched."
  ${EndIf}
FunctionEnd

; **A folder Setup may later delete, and Koru may later write.** The
; uninstaller removes the whole install folder, so the folder must be Koru's
; alone: an empty or new one, or one an earlier Koru is in. And Koru updates
; itself, so the person must be able to write there without administrator
; rights. (Browse appends "${PRODUCT}" to the folder picked, as NSIS does.)
Function CheckDirectory
  ${If} ${FileExists} "$INSTDIR\koru.exe"
    Return
  ${EndIf}
  ${DirState} "$INSTDIR" $0
  ${If} $0 == 1
    MessageBox MB_ICONEXCLAMATION|MB_OK "$INSTDIR already holds other files.$\r$\n$\r$\nChoose an empty or new folder, or the folder ${PRODUCT} is already in: uninstalling ${PRODUCT} deletes the folder it is installed in."
    Abort
  ${EndIf}
  ClearErrors
  CreateDirectory "$INSTDIR"
  FileOpen $1 "$INSTDIR\.koru-setup-check" w
  ${If} ${Errors}
    MessageBox MB_ICONEXCLAMATION|MB_OK "You cannot write to $INSTDIR without administrator rights.$\r$\n$\r$\nKoru updates itself, so choose a folder of your own, such as one under $LOCALAPPDATA\Programs."
    ${If} $0 == -1
      RMDir "$INSTDIR"
    ${EndIf}
    Abort
  ${EndIf}
  FileClose $1
  Delete "$INSTDIR\.koru-setup-check"
  ${If} $0 == -1
    RMDir "$INSTDIR"
  ${EndIf}
FunctionEnd

!define MUI_DIRECTORYPAGE_TEXT_TOP "Setup will install ${PRODUCT} in the folder below. To install it somewhere else, click Browse. Choose a folder that is empty, new or already holds ${PRODUCT}, and that you can write to."
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE CheckDirectory

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
; **Before you go**: what made someone uninstall, sent to the feedback
; endpoint the app's own form uses (uninstall-feedback.ps1 says how). It is
; optional, it comes before the Uninstall button so Cancel sends nothing, and
; a silent uninstall (/S) never asks.
Var FeedbackText
Var FeedbackContact
Var FeedbackMessage
Var FeedbackEmail

Function un.FeedbackPage
  !insertmacro MUI_HEADER_TEXT "Before you go" "Tell us why you are uninstalling ${PRODUCT}. This is optional."
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}
  ${NSD_CreateLabel} 0 0 100% 24u "What made you uninstall? It goes to the Koru team, with the version you are uninstalling. Leave it empty to skip."
  Pop $0
  ; A multi-line box that wraps: nsDialogs' own (NSD_CreateMLText) scrolls
  ; sideways instead.
  nsDialogs::CreateControl EDIT ${DEFAULT_STYLES}|${WS_TABSTOP}|${ES_AUTOVSCROLL}|${ES_MULTILINE}|${ES_WANTRETURN}|${WS_VSCROLL} ${__NSD_Text_EXSTYLE} 0 26u 100% 66u "$FeedbackMessage"
  Pop $FeedbackText
  ; The longest text NSIS's own strings hold, so nothing typed is cut off.
  SendMessage $FeedbackText ${EM_LIMITTEXT} 1000 0
  ${NSD_CreateLabel} 0 98u 100% 10u "Your email, if you would like a reply (optional):"
  Pop $0
  ${NSD_CreateText} 0 110u 100% 12u "$FeedbackEmail"
  Pop $FeedbackContact
  ${NSD_SetFocus} $FeedbackText
  nsDialogs::Show
FunctionEnd

Function un.FeedbackLeave
  ${NSD_GetText} $FeedbackText $FeedbackMessage
  ${NSD_GetText} $FeedbackContact $FeedbackEmail
FunctionEnd

; Waited for at most twenty seconds; whether it arrived never stops the
; uninstall. The text reaches PowerShell through the environment, from
; registers, so nothing typed is parsed as a command.
Function un.SendFeedback
  StrLen $0 $FeedbackMessage
  ${If} $0 < 3
    Return
  ${EndIf}
  InitPluginsDir
  File "/oname=$PLUGINSDIR\uninstall-feedback.ps1" "${__FILEDIR__}\uninstall-feedback.ps1"
  StrCpy $1 $FeedbackMessage
  System::Call 'Kernel32::SetEnvironmentVariable(t "KORU_FEEDBACK_MESSAGE", t r1)'
  StrCpy $1 $FeedbackEmail
  System::Call 'Kernel32::SetEnvironmentVariable(t "KORU_FEEDBACK_CONTACT", t r1)'
  StrCpy $1 "${PRODUCT} ${VERSION}"
  System::Call 'Kernel32::SetEnvironmentVariable(t "KORU_FEEDBACK_PRODUCT", t r1)'
  DetailPrint "Sending your feedback"
  nsExec::Exec /TIMEOUT=20000 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\uninstall-feedback.ps1"'
  Pop $0
FunctionEnd

UninstPage custom un.FeedbackPage un.FeedbackLeave
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; **Every Koru process running from the install directory, ended.** The
; agent is asked first, as the updater asks it (`koru-agent.exe stop`), so
; the Store closes cleanly; what is still running from the directory after
; fifteen seconds — a window, a worker — is ended, because files in use cannot
; be replaced or deleted. The directory reaches PowerShell through the
; environment, never inside the command, so no path needs quoting.
!macro STOP_KORU AGENT
  ${If} ${FileExists} "${AGENT}"
    ExecWait '"${AGENT}" stop'
  ${EndIf}
  System::Call 'Kernel32::SetEnvironmentVariable(t "KORU_INSTALL_DIR", t "$INSTDIR")'
  nsExec::Exec `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$dir = $$env:KORU_INSTALL_DIR.TrimEnd('\') + '\'; $$deadline = (Get-Date).AddSeconds(15); do { $$running = @(Get-Process | Where-Object { $$_.Path -and $$_.Path.StartsWith($$dir, [StringComparison]::OrdinalIgnoreCase) }); if (-not $$running) { break }; Start-Sleep -Milliseconds 250 } while ((Get-Date) -lt $$deadline); $$running | Stop-Process -Force -ErrorAction SilentlyContinue"`
  Pop $0
!macroend

Section "Install"
  SetShellVarContext current
  ; **The agent of this channel stops wherever it runs from**, including an
  ; unzipped copy: `stop` reaches the channel's agent, not a directory's. On
  ; a first install there is no agent here yet, so the new one asks.
  ; It is packed once, here, and copied into place below rather than packed a
  ; second time with the rest.
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "${STAGE}\koru-agent.exe"
  !insertmacro STOP_KORU "$PLUGINSDIR\koru-agent.exe"

  SetOutPath "$INSTDIR"
  File /r /x koru-agent.exe "${STAGE}\*.*"
  CopyFiles /SILENT "$PLUGINSDIR\koru-agent.exe" "$INSTDIR\koru-agent.exe"
  ${IfNot} ${FileExists} "$INSTDIR\koru-agent.exe"
    Abort "Could not install koru-agent.exe"
  ${EndIf}
  WriteUninstaller "$INSTDIR\${UNINSTALLER}"

  CreateShortcut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\koru.exe" "" "$INSTDIR\koru.exe" 0
  CreateShortcut "$DESKTOP\${PRODUCT}.lnk" "$INSTDIR\koru.exe" "" "$INSTDIR\koru.exe" 0

  ; Settings -> Apps. The version is the installed one; once Koru has updated
  ; itself, it is the version this installer brought.
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Koru Works, Inc."
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\koru.exe,0"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://koru.do"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\${UNINSTALLER}"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALLER}" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Call un.SendFeedback
  !insertmacro STOP_KORU "$INSTDIR\koru-agent.exe"

  ; What Koru registered for itself while it ran: koru:// links, starting at
  ; sign-in, and the notification identity (only a signed build has one).
  ; Workspaces and settings live under %LOCALAPPDATA%\Koru and stay.
  ${If} ${FileExists} "$INSTDIR\koru-agent.exe"
    ExecWait '"$INSTDIR\koru-agent.exe" unregister-url-scheme'
  ${EndIf}
  DeleteRegValue HKCU "${RUN_KEY}" "${AUTOSTART}"
  nsExec::Exec `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "Get-AppxPackage -Name '${IDENTITY}' | Remove-AppxPackage"`
  Pop $0

  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  Delete "$DESKTOP\${PRODUCT}.lnk"
  ; Only a directory that holds Koru is deleted, in case `/D=` pointed the
  ; install somewhere that was not empty.
  ${If} ${FileExists} "$INSTDIR\koru.exe"
    RMDir /r "$INSTDIR"
  ${Else}
    Delete "$INSTDIR\${UNINSTALLER}"
    RMDir "$INSTDIR"
  ${EndIf}
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
