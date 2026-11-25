# Installs the MSI, checks what it put on the system, runs the installed app, uninstalls it
# and checks what is left. The release workflow runs this on the package it just built, so
# a package that does not install or an app that does not start never reaches a release.
#
#   pwsh packaging/windows/smoke-test.ps1 MSI [OUT_DIR]
#
# Needs administrator rights (the package installs for all users) and changes the machine
# it runs on: use a CI runner or a virtual machine. OUT_DIR (default: a temporary folder)
# gets the installer logs, the app's output and a screenshot of its window.
param(
    [Parameter(Mandatory)][string]$Msi,
    [string]$Out = (Join-Path ([IO.Path]::GetTempPath()) "pagebara-smoke-$PID")
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Msi = (Resolve-Path $Msi).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

# The app's name and ID, from their one source.
function Identity([string]$const) {
    $line = Select-String -Path (Join-Path $root 'crates\support\identity\src\lib.rs') `
        -Pattern "^pub const ${const}: &str = `"(.*)`";" | Select-Object -First 1
    if (-not $line) { throw "no $const in the identity crate" }
    $line.Matches[0].Groups[1].Value
}
$name = Identity 'NAME'
$appId = Identity 'APP_ID'
$progId = "$appId.pdf"
$dir = Join-Path $env:ProgramFiles $name
$classes = 'HKLM:\Software\Classes'

$failed = 0
function Check([string]$what, [bool]$ok) {
    if ($ok) {
        Write-Host "ok    $what"
    } else {
        Write-Host "FAIL  $what"
        $script:failed++
    }
}

# A registry value, or $null when the key or the value is missing ('' is the default value).
function RegValue([string]$key, [string]$value) {
    if (-not (Test-Path -LiteralPath $key)) { return $null }
    (Get-Item -LiteralPath $key).GetValue($value, $null)
}

function Msiexec([string[]]$arguments, [string]$log) {
    $p = Start-Process msiexec.exe -Wait -PassThru `
        -ArgumentList ($arguments + @('/qn', '/norestart', '/l*v', "`"$log`""))
    if ($p.ExitCode -ne 0) {
        Get-Content $log -Tail 80 | Write-Host
        throw "msiexec $($arguments -join ' ') ended with $($p.ExitCode)"
    }
}

# Runs the installed app with its output in files (a GUI program has no console to print
# to) and waits up to $seconds for it. Returns the exit code, or $null after a timeout.
function RunApp([string]$exe, [string[]]$arguments, [string]$tag, [int]$seconds) {
    $quoted = $arguments | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }
    $p = Start-Process $exe -ArgumentList $quoted -PassThru -NoNewWindow `
        -RedirectStandardOutput (Join-Path $Out "$tag.out.txt") `
        -RedirectStandardError (Join-Path $Out "$tag.err.txt")
    # Without this the exit code of a process started this way reads as empty.
    $null = $p.Handle
    if (-not $p.WaitForExit($seconds * 1000)) {
        Stop-Process -Id $p.Id -Force
        return $null
    }
    $p.ExitCode
}

# The Windows subsystem of an executable: 2 is a GUI program, 3 a console program.
function Subsystem([string]$exe) {
    $bytes = [IO.File]::ReadAllBytes($exe)
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    # The optional header starts 24 bytes after the PE signature; Subsystem is at 68 in it.
    [BitConverter]::ToUInt16($bytes, $pe + 24 + 68)
}

Write-Host "== Install $Msi"
$pdfBefore = RegValue "$classes\.pdf" ''
Msiexec @('/i', "`"$Msi`"") (Join-Path $Out 'install.log')

$exe = Get-ChildItem -LiteralPath $dir -Filter '*.exe' -ErrorAction SilentlyContinue |
    Select-Object -First 1 -ExpandProperty FullName
Check "the app is in $dir" ([bool]$exe)
if (-not $exe) { throw 'nothing to test' }

Write-Host '== The executable'
Check 'it is a GUI program: starting it opens no console window' ((Subsystem $exe) -eq 2)
$text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($exe))
Check 'it carries an application manifest (long paths, per-monitor DPI)' `
    ($text.Contains('longPathAware') -and $text.Contains('PerMonitorV2'))

Write-Host '== What the installer registered'
$command = RegValue "$classes\$progId\shell\open\command" ''
Check 'PDFs can be opened with the app' ($command -and $command.Contains($exe))
$print = RegValue "$classes\$progId\shell\print\command" ''
Check "Explorer's Print reaches the app" ($print -and $print.Contains('--print'))
Check 'the app is offered under Open with' `
    ((Test-Path "$classes\.pdf\OpenWithProgids") -and
     ((Get-Item "$classes\.pdf\OpenWithProgids").GetValueNames() -contains $progId))
Check 'the app is listed in Settings > Default apps' `
    ((RegValue 'HKLM:\Software\RegisteredApplications' $name) -eq "Software\$name\Capabilities")
Check 'the handler of .pdf for all users is left as it was' `
    ((RegValue "$classes\.pdf" '') -eq $pdfBefore)
$shortcut = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\$name.lnk"
Check 'the Start menu has a shortcut' (Test-Path -LiteralPath $shortcut)

Write-Host '== The app runs'
$code = RunApp $exe @('--version') 'version' 60
$version = (Get-Content (Join-Path $Out 'version.out.txt') -Raw -ErrorAction SilentlyContinue)
Write-Host "      --version: $version"
Check '--version prints the version and ends with 0' (($code -eq 0) -and ($version -match '\d+\.\d+\.\d+'))

$samples = Join-Path $Out 'samples'
$code = RunApp $exe @('--make-samples', $samples) 'samples' 120
Check 'it writes its sample documents' (($code -eq 0) -and (Test-Path (Join-Path $samples 'lease.pdf')))

# The window, driven by a script: it opens a sample, waits for the pages to be drawn and
# takes a screenshot. A runner has no graphics card, so this is the software renderer.
$env:PAGEBARA_BACKGROUND = '1'
$env:PAGEBARA_LANG = 'en'
$env:PAGEBARA_SPEECH = 'stub'
$env:SAMPLES = $samples
$env:OUT = $Out
$env:PAGEBARA_SHOTS = $Out
$appProfile = Join-Path $Out 'profile'
New-Item -ItemType Directory -Force $appProfile | Out-Null
$code = RunApp $exe @('--profile', $appProfile, '--size', '1280x800', '--light',
    '--script', (Join-Path $PSScriptRoot 'smoke.pdfs')) 'window' 180
if ($code -ne 0) {
    Get-Content (Join-Path $Out 'window.err.txt') -Tail 60 -ErrorAction SilentlyContinue | Write-Host
}
Check 'its window opens a document and draws it' ($code -eq 0)
Check 'the screenshot of the window was written' (Test-Path (Join-Path $Out 'smoke.png'))

Write-Host '== Uninstall'
Msiexec @('/x', "`"$Msi`"") (Join-Path $Out 'uninstall.log')
Check 'the program folder is gone' (-not (Test-Path -LiteralPath $dir))
Check 'the file type registration is gone' (-not (Test-Path "$classes\$progId"))
Check 'the Default apps entry is gone' `
    ($null -eq (RegValue 'HKLM:\Software\RegisteredApplications' $name))
Check 'the Start menu shortcut is gone' (-not (Test-Path -LiteralPath $shortcut))
Check 'the handler of .pdf for all users is still as it was' `
    ((RegValue "$classes\.pdf" '') -eq $pdfBefore)

Write-Host "== Output in $Out"
if ($failed -gt 0) {
    Write-Host "$failed check(s) failed"
    exit 1
}
Write-Host 'All checks passed'
