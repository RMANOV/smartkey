# One bounded native maintenance smoke. Run ONLY in an ephemeral standard account.
# The parent CI job supplies/loads that user's profile; this script never elevates.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Bundle,
    [Parameter(Mandatory=$true)][string]$OutDir
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
$adminSid = 'S-1-5-32-544'
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) -or
    @($identity.Groups | Where-Object { $_.Value -eq $adminSid }).Count -ne 0) {
    throw 'Refusing smoke: this must be a standard NONADMIN account, not an elevated or split-token administrator.'
}
if (-not $env:LOCALAPPDATA -or -not $env:APPDATA -or -not $env:USERPROFILE) {
    throw 'The standard user profile must be loaded (APPDATA, LOCALAPPDATA, USERPROFILE required).'
}
# Credential launchers can inherit the administrator's environment even when
# the token/profile is correct. Cross-check actual SID profile + shell folders
# before constructing any destination, and fail rather than touching that home.
$profileKey = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey(
    'SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\' + $identity.User.Value)
if ($null -eq $profileKey) { throw 'No loaded Windows profile is registered for the current standard-user SID.' }
try { $actualProfile = [Environment]::ExpandEnvironmentVariables([string]$profileKey.GetValue('ProfileImagePath', $null)) }
finally { $profileKey.Dispose() }
$actualLocal = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
$actualRoaming = [Environment]::GetFolderPath([Environment+SpecialFolder]::ApplicationData)
foreach ($pair in @(@($env:USERPROFILE, $actualProfile), @($env:LOCALAPPDATA, $actualLocal), @($env:APPDATA, $actualRoaming))) {
    if (-not $pair[1] -or -not [string]::Equals(
        [IO.Path]::GetFullPath($pair[0]).TrimEnd('\'),
        [IO.Path]::GetFullPath($pair[1]).TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Inherited USERPROFILE/APPDATA/LOCALAPPDATA does not match the actual standard-user profile. The parent must supply the correct user environment.'
    }
}
$bundlePath = (Resolve-Path -LiteralPath $Bundle).Path
$install = Join-Path $env:LOCALAPPDATA 'SmartKey'
$data = Join-Path $env:APPDATA 'smartkey'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$clsid = 'HKCU:\SOFTWARE\Classes\CLSID\{7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C}'
$tip = 'HKCU:\Software\Microsoft\CTF\TIP\{7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C}'
$menu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\SmartKey'
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null

function Assert-True([bool]$condition, [string]$message) {
    if (-not $condition) { throw $message }
}
function Get-Login {
    if (-not (Test-Path -LiteralPath $run)) { return $null }
    return (Get-Item -LiteralPath $run).GetValue('SmartKey', $null)
}
function Keyboard-Snapshot {
    # Only the ephemeral account is read. The receipt reports equality, not values.
    $snapshot = [ordered]@{}
    foreach ($name in @('Keyboard Layout\Preload', 'Keyboard Layout\Substitutes')) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($name)
        $values = [ordered]@{}
        if ($null -ne $key) {
            try { foreach ($value in @($key.GetValueNames() | Sort-Object)) { $values[$value] = $key.GetValue($value) } }
            finally { $key.Dispose() }
        }
        $snapshot[$name] = $values
    }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Control Panel\International\User Profile')
    try {
        $snapshot['InputMethodOverride'] = if ($null -ne $key) { $key.GetValue('InputMethodOverride', $null) } else { $null }
        $snapshot['DefaultInputMethodOverride'] = if ($null -ne $key) { $key.GetValue('DefaultInputMethodOverride', $null) } else { $null }
    }
    finally { if ($null -ne $key) { $key.Dispose() } }
    $default = Get-WinDefaultInputMethodOverride
    $snapshot['WindowsDefaultInputTip'] = if ($null -ne $default) { $default.InputTip } else { $null }
    return ($snapshot | ConvertTo-Json -Depth 4 -Compress)
}
$steps = New-Object System.Collections.Generic.List[object]
function Invoke-Helper([string]$exe, [string]$action) {
    $stem = $action.TrimStart('-')
    $stdout = Join-Path $OutDir "$stem.stdout.txt"
    $stderr = Join-Path $OutDir "$stem.stderr.txt"
    $process = Start-Process -FilePath $exe -ArgumentList @($action, '--no-dialog') -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if (-not $process.WaitForExit(45000)) {
        $process.Kill()
        $process.WaitForExit()
        throw "$action exceeded 45 seconds; the smoke stopped."
    }
    $process.Refresh()
    $steps.Add([ordered]@{ action=$action; exit_code=$process.ExitCode })
    if ($process.ExitCode -ne 0) {
        # Exact helper HRESULT/error is retained as the failed native evidence.
        throw "$action failed (exit $($process.ExitCode)); see $stderr."
    }
}
$receipt = [ordered]@{
    status='NOT_RUN'; standard_nonadmin=$true; current_user_profile_confirmed=$true; no_dialog=$true
    typing_exercised=$false; logon_exercised=$false
    steps=$steps; keyboard_defaults_preserved=$false; personal_sentinel_preserved=$false
}
$before = Keyboard-Snapshot
try {
    # Never use a daily account or delete a prior installation for this smoke.
    Assert-True (-not (Test-Path -LiteralPath $install)) 'The ephemeral user already has a SmartKey install folder.'
    Assert-True (-not (Test-Path -LiteralPath $data)) 'The ephemeral user already has SmartKey data.'
    Assert-True (-not (Test-Path -LiteralPath $clsid)) 'The ephemeral user already has SmartKey COM registration.'
    Assert-True ($null -eq (Get-Login)) 'An existing SmartKey Run value must not be overwritten by this smoke.'
    New-Item -ItemType Directory -Path $data | Out-Null
    $personal = Join-Path $data 'personal.json'
    [IO.File]::WriteAllText($personal, '{"synthetic_smoke_sentinel":true}')
    $personalHash = (Get-FileHash -LiteralPath $personal -Algorithm SHA256).Hash

    Invoke-Helper (Join-Path $bundlePath 'smartkey-register.exe') '--install'
    $helper = Join-Path $install 'smartkey-register.exe'
    foreach ($name in @('smartkey-register.exe','smartkey_win.dll','Install-SmartKey.cmd','Status-SmartKey.cmd','Enable-SmartKey.cmd','Disable-SmartKey.cmd','Uninstall-SmartKey.cmd')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $install $name) -PathType Leaf) "Missing staged file: $name"
    }
    foreach ($name in @('smartkey.json','corpus_en.json','corpus_bg.json','corpus_tech.json')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $data $name) -PathType Leaf) "Missing runtime data: $name"
    }
    foreach ($name in @('Status-SmartKey.cmd','Enable-SmartKey.cmd','Disable-SmartKey.cmd','Uninstall-SmartKey.cmd')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $menu $name) -PathType Leaf) "Missing Start menu command: $name"
    }
    Assert-True ((Get-Login) -eq ('"' + $helper + '" --login')) 'Login entry does not identify the stable installed helper.'
    Assert-True ((Keyboard-Snapshot) -eq $before) 'Installation changed existing keyboard/default settings.'
    Invoke-Helper $helper '--status'
    Invoke-Helper $helper '--disable'
    Assert-True ($null -eq (Get-Login)) 'Disable left the owned login entry.'
    Assert-True ((Keyboard-Snapshot) -eq $before) 'Disable changed existing keyboard/default settings.'
    Invoke-Helper $helper '--enable'
    Assert-True ((Get-Login) -eq ('"' + $helper + '" --login')) 'Enable did not restore the exact owned login entry.'
    Invoke-Helper $helper '--uninstall'
    Assert-True ($null -eq (Get-Login)) 'Uninstall left the owned login entry.'
    Assert-True (-not (Test-Path -LiteralPath $clsid)) 'Uninstall left own HKCU COM registration.'
    Assert-True (-not (Test-Path -LiteralPath $tip)) 'Uninstall left own HKCU TIP registration.'
    Assert-True ((Keyboard-Snapshot) -eq $before) 'Maintenance changed existing keyboard/default settings.'
    Assert-True ((Get-FileHash -LiteralPath $personal -Algorithm SHA256).Hash -eq $personalHash) 'Maintenance changed the synthetic personal file.'
    # The EXE API uninstalls integration; the user-facing CMD performs file
    # removal afterwards. Mirror only that exact binary deletion here.
    Remove-Item -LiteralPath (Join-Path $install 'smartkey_win.dll'), $helper
    Assert-True (-not (Test-Path -LiteralPath $helper)) 'Helper binary remains.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $install 'smartkey_win.dll'))) 'DLL remains.'
    $receipt['keyboard_defaults_preserved']=$true
    $receipt['personal_sentinel_preserved']=$true
    $receipt['status']='PASS_NATIVE_STANDARD_USER_MAINTENANCE'
} catch {
    $receipt['status']='FAIL_NATIVE_STANDARD_USER_MAINTENANCE'
    $receipt['error']=$_.Exception.Message
    throw
} finally {
    $receipt | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutDir 'smoke-result.json') -Encoding UTF8
}
