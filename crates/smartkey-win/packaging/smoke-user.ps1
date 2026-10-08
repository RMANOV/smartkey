# One bounded native maintenance smoke. Run ONLY in an ephemeral standard account.
# The parent CI job supplies/loads that user's profile; this script never elevates.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Bundle,
    [Parameter(Mandatory=$true)][string]$OutDir,
    [ValidateSet('Prerequisite', 'Lifecycle')][string]$Phase = 'Lifecycle'
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
$install = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)) 'SmartKey'
$localInstall = Join-Path $env:LOCALAPPDATA 'SmartKey'
$data = Join-Path $env:APPDATA 'smartkey'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$clsid = 'HKCU:\SOFTWARE\Classes\CLSID\{7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C}'
$tip = 'HKCU:\Software\Microsoft\CTF\TIP\{7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C}'
$machineClass = 'HKLM:\SOFTWARE\Classes\CLSID\{7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C}'
$machineTip = 'HKLM:\Software\Microsoft\CTF\TIP\{7A3B9E1F-4C2D-4E5A-8F6B-1D2E3F4A5B6C}'
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
    # Only the ephemeral account's bounded keyboard configuration is read.
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
function Machine-Snapshot {
    # Only this product's protected files and exact machine identities are read.
    $files = @(Get-ChildItem -LiteralPath $install -File -Force | Sort-Object Name | ForEach-Object {
        [ordered]@{ name=$_.Name; sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
    })
    $registry = foreach ($path in @($machineClass, $machineTip)) {
        Assert-True (Test-Path -LiteralPath $path) "Missing machine registration: $path"
        $keys = @((Get-Item -LiteralPath $path)) + @(Get-ChildItem -LiteralPath $path -Recurse | Sort-Object Name)
        foreach ($key in $keys) {
            $values = [ordered]@{}
            foreach ($name in @($key.GetValueNames() | Sort-Object)) { $values[$name] = $key.GetValue($name) }
            [ordered]@{ path=$key.Name; values=$values }
        }
    }
    return ([ordered]@{ files=$files; registry=@($registry) } | ConvertTo-Json -Depth 8 -Compress)
}
$steps = New-Object System.Collections.Generic.List[object]
function Invoke-Helper([string]$exe, [string]$action, [int]$expectedExit = 0) {
    $stem = '{0:D2}-{1}' -f $steps.Count, $action.TrimStart('-')
    $stdout = Join-Path $OutDir "$stem.stdout.txt"
    $stderr = Join-Path $OutDir "$stem.stderr.txt"
    # Keep our own .NET Process/handle from Start through ExitCode. Windows
    # PowerShell Start-Process -PassThru can yield a null cached ExitCode even
    # after WaitForExit/Refresh; null must never be recorded as native success.
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = New-Object System.Diagnostics.ProcessStartInfo
    $process.StartInfo.FileName = $exe
    $process.StartInfo.Arguments = "$action --no-dialog"
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    try {
        if (-not $process.Start()) { throw "Cannot start $action." }
        # Drain both streams asynchronously before waiting, avoiding pipe
        # deadlocks while preserving the real helper HRESULT/error output.
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(45000)) {
            $process.Kill()
            if (-not $process.WaitForExit(5000)) { throw "$action timed out and did not exit after termination." }
            [IO.File]::WriteAllText($stdout, $stdoutTask.GetAwaiter().GetResult())
            [IO.File]::WriteAllText($stderr, $stderrTask.GetAwaiter().GetResult())
            throw "$action exceeded 45 seconds; the smoke stopped."
        }
        $exitCode = $process.ExitCode
        if ($null -eq $exitCode) { throw "$action exited but its real exit code was unavailable; refusing an inferred result." }
        $stdoutText = $stdoutTask.GetAwaiter().GetResult()
        $stderrText = $stderrTask.GetAwaiter().GetResult()
        [IO.File]::WriteAllText($stdout, $stdoutText)
        [IO.File]::WriteAllText($stderr, $stderrText)
        $steps.Add([ordered]@{ action=$action; exit_code=[int]$exitCode; expected_exit=$expectedExit })
        if ($exitCode -ne $expectedExit) {
            throw "$action returned exit $exitCode (required $expectedExit); see $stderr."
        }
        return [pscustomobject]@{ stdout=$stdoutText; stderr=$stderrText }
    } finally { $process.Dispose() }

}
$receipt = [ordered]@{
    status='NOT_RUN'; standard_nonadmin=$true; current_user_profile_confirmed=$true; no_dialog=$true
    phase=$Phase; typing_exercised=$false; logon_exercised=$false
    steps=$steps; keyboard_defaults_preserved=$false; personal_sentinel_preserved=$false
    machine_prerequisite_refused=$false; machine_install_preserved=$false
}
$before = Keyboard-Snapshot
$before | Set-Content -LiteralPath (Join-Path $OutDir 'keyboard-before.json') -Encoding UTF8
function Assert-KeyboardPreserved([string]$message) {
    $after = Keyboard-Snapshot
    $after | Set-Content -LiteralPath (Join-Path $OutDir ('keyboard-after-{0:D2}.json' -f $steps.Count)) -Encoding UTF8
    Assert-True ($after -eq $before) $message
}
try {
    # Never use a daily account or delete a prior installation for this smoke.
    Assert-True (-not (Test-Path -LiteralPath $localInstall)) 'The ephemeral user already has a SmartKey install folder.'
    Assert-True (-not (Test-Path -LiteralPath $data)) 'The ephemeral user already has SmartKey data.'
    Assert-True (-not (Test-Path -LiteralPath $clsid)) 'The ephemeral user already has SmartKey COM registration.'
    Assert-True (-not (Test-Path -LiteralPath $tip)) 'The ephemeral user already has SmartKey user enablement.'
    Assert-True (-not (Test-Path -LiteralPath $menu)) 'The ephemeral user already has a SmartKey Start menu folder.'
    Assert-True ($null -eq (Get-Login)) 'An existing SmartKey Run value must not be overwritten by this smoke.'
    if ($Phase -eq 'Prerequisite') {
        foreach ($path in @($install, $machineClass, $machineTip)) {
            Assert-True (-not (Test-Path -LiteralPath $path)) 'The prerequisite refusal requires a fresh machine without SmartKey.'
        }
        $refusal = Invoke-Helper (Join-Path $bundlePath 'smartkey-register.exe') '--install' 1
        Assert-True ($refusal.stderr.Contains('SmartKey machine installation is missing.') -and
            $refusal.stderr.Contains('Install-Machine-SmartKey.cmd')) 'Missing-machine refusal did not explain the supported administrator setup.'
        foreach ($path in @($data, $menu, $localInstall, $clsid, $tip, $install, $machineClass, $machineTip)) {
            Assert-True (-not (Test-Path -LiteralPath $path)) "Prerequisite refusal created unexpected state: $path"
        }
        Assert-True ($null -eq (Get-Login)) 'Prerequisite refusal created a login entry.'
        Assert-KeyboardPreserved 'Prerequisite refusal changed keyboard/default settings.'
        $receipt['machine_prerequisite_refused']=$true
        $receipt['keyboard_defaults_preserved']=$true
        $receipt['status']='PASS_NATIVE_STANDARD_USER_PREREQUISITE'
        return
    }

    $expectedFiles = @(Get-ChildItem -LiteralPath $bundlePath -Force | ForEach-Object Name | Sort-Object)
    $actualFiles = @(Get-ChildItem -LiteralPath $install -Force | ForEach-Object Name | Sort-Object)
    Assert-True (-not (Compare-Object -ReferenceObject $expectedFiles -DifferenceObject $actualFiles)) 'Protected install does not contain the exact frozen bundle.'
    foreach ($name in $expectedFiles) {
        Assert-True ((Get-FileHash -LiteralPath (Join-Path $bundlePath $name) -Algorithm SHA256).Hash -eq
            (Get-FileHash -LiteralPath (Join-Path $install $name) -Algorithm SHA256).Hash) "Protected file differs from frozen bundle: $name"
    }
    $machineBefore = Machine-Snapshot
    $helper = Join-Path $install 'smartkey-register.exe'
    $initialStatus = Invoke-Helper $helper '--status'
    Assert-True ($initialStatus.stdout -match '(?m)^TSF profile: disabled\.') 'Machine registration enabled SmartKey before this user opted in.'
    New-Item -ItemType Directory -Path $data | Out-Null
    $personal = Join-Path $data 'personal.json'
    [IO.File]::WriteAllText($personal, '{"synthetic_smoke_sentinel":true}')
    $personalHash = (Get-FileHash -LiteralPath $personal -Algorithm SHA256).Hash

    Invoke-Helper (Join-Path $bundlePath 'smartkey-register.exe') '--install' | Out-Null
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
    Assert-KeyboardPreserved 'Installation changed existing keyboard/default settings.'
    Invoke-Helper $helper '--status' | Out-Null
    Invoke-Helper $helper '--disable' | Out-Null
    Assert-True ($null -eq (Get-Login)) 'Disable left the owned login entry.'
    Assert-KeyboardPreserved 'Disable changed existing keyboard/default settings.'
    Invoke-Helper $helper '--enable' | Out-Null
    Assert-True ((Get-Login) -eq ('"' + $helper + '" --login')) 'Enable did not restore the exact owned login entry.'
    Invoke-Helper $helper '--uninstall' | Out-Null
    Assert-True ($null -eq (Get-Login)) 'Uninstall left the owned login entry.'
    Assert-True (-not (Test-Path -LiteralPath $clsid)) 'Uninstall left own HKCU COM registration.'
    # Windows may retain an explicit per-user disabled marker. Verify the API
    # state in a fresh process instead of deleting that supported state by hand.
    $status = Invoke-Helper $helper '--status'
    Assert-True ($status.stdout -match '(?m)^TSF profile: disabled\.') 'Fresh process did not report the uninstalled user profile disabled.'
    Invoke-Helper $helper '--verify-machine' | Out-Null
    foreach ($name in @('Status-SmartKey.cmd','Enable-SmartKey.cmd','Disable-SmartKey.cmd')) {
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $menu $name))) "Uninstall left a user maintenance command: $name"
    }
    Assert-KeyboardPreserved 'Maintenance changed existing keyboard/default settings.'
    Assert-True ((Get-FileHash -LiteralPath $personal -Algorithm SHA256).Hash -eq $personalHash) 'Maintenance changed the synthetic personal file.'
    Assert-True (-not (Test-Path -LiteralPath $localInstall)) 'User setup created an unprotected local binary folder.'
    Assert-True ((Machine-Snapshot) -eq $machineBefore) 'User maintenance changed the protected bundle or machine registration.'
    $receipt['keyboard_defaults_preserved']=$true
    $receipt['personal_sentinel_preserved']=$true
    $receipt['machine_install_preserved']=$true
    $receipt['status']='PASS_NATIVE_STANDARD_USER_MAINTENANCE'
} catch {
    $receipt['status']='FAIL_NATIVE_STANDARD_USER_' + $Phase.ToUpperInvariant()
    $receipt['error']=$_.Exception.Message
    throw
} finally {
    $receipt | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutDir 'smoke-result.json') -Encoding UTF8
}
