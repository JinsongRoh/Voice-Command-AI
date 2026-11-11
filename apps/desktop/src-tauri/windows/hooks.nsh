; Installer hooks for the bundled vcad sidecar.
;
; Closing the app does not stop a vcad it started when the installer ends the
; app forcibly, and Windows refuses to overwrite a running vcad.exe. Stop only
; the vcad that runs from this install folder (other vcad instances, e.g. a
; developer build, keep running). The same goes for the bundled vca CLI,
; which agent hooks start briefly on each hook event; a hook stopped this
; way fails open, so the agent carries on.
!macro VCA_STOP_BUNDLED_VCAD
  ; The installer is 32-bit, so its PowerShell cannot read the path of a
  ; 64-bit process from Get-Process; Win32_Process.ExecutablePath works.
  nsExec::Exec `powershell.exe -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process -Filter \"Name='vcad.exe' OR Name='vca.exe'\" | Where-Object { $$_.ExecutablePath -eq '$INSTDIR\vcad.exe' -or $$_.ExecutablePath -eq '$INSTDIR\vca.exe' } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"`
  Sleep 500
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro VCA_STOP_BUNDLED_VCAD
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro VCA_STOP_BUNDLED_VCAD
!macroend

; "Start VCA when I sign in" (apps/shell/src/autostart.rs) is the HKCU Run
; value named after the product, which Tauri's uninstaller already deletes
; (not on updates). Also drop the Task Manager / Settings > Apps > Startup
; state Windows keeps under that name, so no orphan stays listed.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "${PRODUCTNAME}"
  ${EndIf}
!macroend
