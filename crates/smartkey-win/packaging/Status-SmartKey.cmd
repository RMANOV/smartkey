@echo off
setlocal
set "SMARTKEY_HELPER=%~dp0smartkey-register.exe"
if not exist "%SMARTKEY_HELPER%" set "SMARTKEY_HELPER=%LOCALAPPDATA%\SmartKey\smartkey-register.exe"
if not exist "%SMARTKEY_HELPER%" (
  echo SmartKey helper is missing. Extract the whole SmartKey archive first.
  pause
  exit /b 1
)
"%SMARTKEY_HELPER%" --status
if errorlevel 1 (
  echo SmartKey could not complete the action. The error window describes the failure.
  pause
  exit /b 1
)
exit /b 0
