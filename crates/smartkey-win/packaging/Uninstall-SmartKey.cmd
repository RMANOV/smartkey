@echo off
setlocal
set "SMARTKEY_HELPER=%LOCALAPPDATA%\SmartKey\smartkey-register.exe"
if not exist "%SMARTKEY_HELPER%" set "SMARTKEY_HELPER=%~dp0smartkey-register.exe"
if not exist "%SMARTKEY_HELPER%" (
  echo SmartKey helper is missing; registration removal was not attempted.
  pause
  exit /b 1
)
"%SMARTKEY_HELPER%" --uninstall
if errorlevel 1 (
  echo Removal was incomplete. Staged files have been retained so you can retry.
  pause
  exit /b 1
)
rem Delete only the installed SmartKey binaries, after the helper has exited.
del /q "%LOCALAPPDATA%\SmartKey\smartkey_win.dll" "%LOCALAPPDATA%\SmartKey\smartkey-register.exe" 2>nul
if exist "%LOCALAPPDATA%\SmartKey\smartkey_win.dll" (
  echo Registration removed. Close apps using SmartKey before deleting the remaining DLL.
  pause
  exit /b 1
)
if exist "%LOCALAPPDATA%\SmartKey\smartkey-register.exe" (
  echo Registration removed. The helper file could not be deleted; remove it after closing it.
  pause
  exit /b 1
)
echo SmartKey removed. Your keyboards and AppData\smartkey personal data were preserved.
rem The final parsed block can remove this Start menu command after all work.
(
  del /q "%APPDATA%\Microsoft\Windows\Start Menu\Programs\SmartKey\Uninstall-SmartKey.cmd" 2>nul
  exit /b 0
)
