@echo off
setlocal
rem Explicit machine action. Right-click this command in the extracted archive
rem and choose Run as administrator. No automatic elevation is attempted.
if not exist "%~dp0smartkey-register.exe" (
  echo Extract the whole original SmartKey archive before this machine action.
  pause
  exit /b 1
)
"%~dp0smartkey-register.exe" --install-machine
if errorlevel 1 (
  echo Machine action was incomplete. The error window describes the failure.
  pause
  exit /b 1
)
exit /b 0
