<#
.SYNOPSIS
    singbox-board one-click installer, upgrader and uninstaller for Windows.

.DESCRIPTION
    Run in PowerShell opened as administrator:

      irm https://raw.githubusercontent.com/MiChongs/singbox-board/main/install.ps1 | iex

    With options (the script block form passes them on):

      & ([scriptblock]::Create((irm https://raw.githubusercontent.com/MiChongs/singbox-board/main/install.ps1))) -Mirror https://ghfast.top/
      powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall -Purge

    Installs singbox-board.exe and the tray launcher from the latest GitHub
    release (verified against SHA256SUMS) into Program Files, writes
    C:\ProgramData\singbox-board\daemon.toml, creates the singbox-board group
    and adds the current user, registers and starts the singbox-board service,
    adds Start menu shortcuts, installs the sing-box core and asks whether to
    enable the optional components (Sub-Store, http-meta). Re-running it
    upgrades in place.

    Options can also come from the environment when piping into iex:
    SBB_VERSION, SBB_MIRROR, SBB_SUB_STORE, SBB_HTTP_META, SBB_REPO.
#>
[CmdletBinding()]
param(
    # Install this release (e.g. v0.2.0) instead of the latest.
    [string]$Version = $env:SBB_VERSION,
    # Prefix for github.com downloads, e.g. https://ghfast.top/ (also written to daemon.toml).
    [string]$Mirror = $env:SBB_MIRROR,
    # Install from an extracted release archive (or the .zip) instead of downloading.
    [string]$Local = "",
    # Installation directory of the programs.
    [string]$Prefix = (Join-Path $env:ProgramFiles "singbox-board"),
    # Account added to the singbox-board group (default: the current user).
    [string]$User = "",
    # Answer the first-run question without prompting: yes or no.
    [string]$SubStore = $env:SBB_SUB_STORE,
    [string]$HttpMeta = $env:SBB_HTTP_META,
    # Install files only; do not register or start the service.
    [switch]$NoStart,
    # Do not install the sing-box core.
    [switch]$NoCore,
    # Remove singbox-board (keeps configuration and data).
    [switch]$Uninstall,
    # With -Uninstall: also remove configuration, components, the sing-box binary and the group.
    [switch]$Purge
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = if ($env:SBB_REPO) { $env:SBB_REPO } else { "MiChongs/singbox-board" }
$Service = "singbox-board"
$Group = "singbox-board"
$DataRoot = Join-Path $env:ProgramData "singbox-board"
$CoreRoot = Join-Path $env:ProgramData "sing-box"
$Conf = Join-Path $DataRoot "daemon.toml"
$Bin = Join-Path $Prefix "singbox-board.exe"
$Launcher = Join-Path $Prefix "singbox-board-tray.exe"
$Menu = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\singbox-board"

function Say([string]$Text) { Write-Host "==> " -ForegroundColor Cyan -NoNewline; Write-Host $Text }
function Warn([string]$Text) { Write-Host "warning: " -ForegroundColor Yellow -NoNewline; Write-Host $Text }
# `throw`, not `exit`: under `irm | iex` exit would close the user's window.
function Die([string]$Text) { throw "error: $Text" }

function Test-Admin {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-Arch {
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        "AMD64" { "amd64" }
        "ARM64" { "arm64" }
        default { Die "unsupported architecture: $arch (amd64 and arm64 are supported)" }
    }
}

function Get-Url([string]$Url) {
    if ($Mirror -and $Url.StartsWith("https://github.com/")) { return $Mirror.TrimEnd("/") + "/" + $Url }
    $Url
}

function Get-File([string]$Url, [string]$Dest) {
    Invoke-WebRequest -UseBasicParsing -Uri (Get-Url $Url) -OutFile $Dest
}

# The directory of an extracted release archive.
function Get-Release([string]$Temp) {
    $name = "singbox-board-windows-$(Get-Arch)"
    if ($Local) {
        if ($Local.EndsWith(".zip")) {
            Expand-Archive -Path $Local -DestinationPath $Temp -Force
            return (Join-Path $Temp $name)
        }
        if (-not (Test-Path (Join-Path $Local "singbox-board.exe"))) { Die "$Local does not contain an extracted $name archive" }
        return $Local
    }
    $base = if ($Version) { "https://github.com/$Repo/releases/download/$Version" } else { "https://github.com/$Repo/releases/latest/download" }
    $tag = if ($Version) { $Version } else { "latest" }
    Say "downloading $name.zip ($tag)"
    $zip = Join-Path $Temp "$name.zip"
    Get-File "$base/$name.zip" $zip
    Get-File "$base/SHA256SUMS" (Join-Path $Temp "SHA256SUMS")
    $expected = $null
    foreach ($line in Get-Content (Join-Path $Temp "SHA256SUMS")) {
        $parts = $line -split "\s+", 2
        if ($parts.Count -eq 2 -and $parts[1].TrimStart("*") -eq "$name.zip") { $expected = $parts[0] }
    }
    if (-not $expected) { Die "SHA256SUMS has no entry for $name.zip" }
    $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash
    if ($actual -ne $expected.ToUpperInvariant()) { Die "checksum mismatch for $name.zip (expected $expected, got $actual)" }
    Say "checksum verified"
    Expand-Archive -Path $zip -DestinationPath $Temp -Force
    Join-Path $Temp $name
}

# Copies a program, moving a running copy (the tray) out of the way first.
function Copy-Program([string]$Source, [string]$Dest) {
    if (Test-Path $Dest) {
        $old = "$Dest.old"
        Remove-Item -Force $old -ErrorAction SilentlyContinue
        try { Remove-Item -Force $Dest } catch { Move-Item -Force $Dest $old }
    }
    Copy-Item -Force $Source $Dest
}

function Invoke-Sc([string[]]$Arguments) {
    $output = & sc.exe @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "sc.exe $($Arguments -join ' '): $output" }
    $output
}

# SYSTEM and administrators change, users read and run.
function Protect-Directory([string]$Dir) {
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    & icacls.exe $Dir /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-32-545:(OI)(CI)RX" /T /C /Q | Out-Null
}

function Get-GroupSid {
    try { (New-Object Security.Principal.NTAccount($Group)).Translate([Security.Principal.SecurityIdentifier]).Value } catch { $null }
}

function Add-Group {
    if (-not (Get-GroupSid)) {
        & net.exe localgroup $Group /add | Out-Null
        Say "created the local group $Group"
    }
    $account = if ($User) { $User } else { [Security.Principal.WindowsIdentity]::GetCurrent().Name }
    $output = & net.exe localgroup $Group $account /add 2>&1
    if ($LASTEXITCODE -eq 0) {
        Say "added $account to $Group (sign out and back in to use singbox-board without administrator rights)"
    } elseif ("$output" -match "1378") {
        Say "$account is already a member of $Group"
    } else {
        Warn "could not add $account to ${Group}: $output"
    }
}

function Install-Service {
    # Windows PowerShell mangles quotes passed to sc.exe, so the command line
    # goes through New-Service or straight into the registry.
    $path = "`"$Bin`" daemon --service"
    $description = "Supervises sing-box (TUN, routing) for the singbox-board dashboard and tray"
    if (Get-Service $Service -ErrorAction SilentlyContinue) {
        Set-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Services\$Service" -Name ImagePath -Value $path
        Set-Service -Name $Service -StartupType Automatic
    } else {
        New-Service -Name $Service -BinaryPathName $path -DisplayName "singbox-board" -Description $description -StartupType Automatic | Out-Null
        Say "registered the $Service service"
    }
    Invoke-Sc @("failure", $Service, "reset=", "86400", "actions=", "restart/5000/restart/10000/restart/30000") | Out-Null
    # Let members of the group start the service (the tray offers that).
    $sid = Get-GroupSid
    if ($sid) {
        $sddl = ((Invoke-Sc @("sdshow", $Service)) -join "").Trim()
        $ace = "(A;;RPLCLORC;;;$sid)"
        if (-not $sddl.Contains($ace)) {
            $index = $sddl.IndexOf("S:")
            $sddl = if ($index -ge 0) { $sddl.Insert($index, $ace) } else { $sddl + $ace }
            Invoke-Sc @("sdset", $Service, $sddl) | Out-Null
        }
    }
}

function Add-Shortcut([string]$Name, [string]$Target, [string]$Arguments, [string]$Description) {
    New-Item -ItemType Directory -Force -Path $Menu | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut((Join-Path $Menu "$Name.lnk"))
    $link.TargetPath = $Target
    $link.Arguments = $Arguments
    $link.WorkingDirectory = $Prefix
    $link.Description = $Description
    $link.IconLocation = "$Bin,0"
    $link.Save()
}

function Add-Path {
    $path = [Environment]::GetEnvironmentVariable("Path", "Machine")
    if (($path -split ";") -notcontains $Prefix) {
        [Environment]::SetEnvironmentVariable("Path", ($path.TrimEnd(";") + ";" + $Prefix), "Machine")
        Say "added $Prefix to PATH (new terminals pick it up)"
    }
}

function Remove-Path {
    $path = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $kept = ($path -split ";") | Where-Object { $_ -and $_ -ne $Prefix }
    [Environment]::SetEnvironmentVariable("Path", ($kept -join ";"), "Machine")
}

function Wait-Daemon {
    for ($i = 0; $i -lt 20; $i++) {
        & $Bin status *> $null
        if ($LASTEXITCODE -eq 0) { return $true }
        Start-Sleep -Seconds 1
    }
    $false
}

function Invoke-PostStart {
    if (-not (Wait-Daemon)) {
        Warn "the daemon did not come up; see $DataRoot\logs\daemon.log"
        return
    }
    $status = (& $Bin status --json) -join "`n"
    if (-not $NoCore) {
        if ($status -match '"core_version": null') {
            Say "installing the sing-box core from MiChongs/sing-box"
            & $Bin update
            if ($LASTEXITCODE -ne 0) { Warn "sing-box core install failed; retry with: singbox-board update (as administrator)" }
        } else {
            Say "sing-box core already installed (upgrade it with: singbox-board update)"
        }
    }
    if ($SubStore -or $HttpMeta) {
        $sub = if ($SubStore) { $SubStore } else { "no" }
        $meta = if ($HttpMeta) { $HttpMeta } else { "no" }
        & $Bin setup --sub-store $sub --http-meta $meta
        if ($LASTEXITCODE -ne 0) { Warn "component setup failed; retry with: singbox-board setup" }
    } elseif ($status -match '"setup_required": true') {
        if ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
            Write-Host ""
            & $Bin setup
            if ($LASTEXITCODE -ne 0) { Warn "component setup failed; retry with: singbox-board setup" }
        } else {
            Say "optional components (Sub-Store, http-meta) not chosen yet: singbox-board setup"
        }
    }
}

function Install-Board {
    $temp = Join-Path ([IO.Path]::GetTempPath()) ("singbox-board-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $temp | Out-Null
    try {
        $release = Get-Release $temp
        $running = Get-Service $Service -ErrorAction SilentlyContinue
        if ($running -and $running.Status -ne "Stopped") {
            Say "stopping the $Service service for the upgrade"
            Stop-Service $Service -Force
        }
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
        Copy-Program (Join-Path $release "singbox-board.exe") $Bin
        Copy-Program (Join-Path $release "singbox-board-tray.exe") $Launcher
        foreach ($doc in @("README.md", "install.ps1")) {
            $file = Join-Path $release $doc
            if (Test-Path $file) { Copy-Item -Force $file (Join-Path $Prefix $doc) }
        }
        Say "installed $Bin ($(& $Bin --version))"

        Protect-Directory $DataRoot
        Protect-Directory $CoreRoot
        if (Test-Path $Conf) {
            Say "keeping existing $Conf"
        } else {
            $template = (& $Bin daemon --print-default-config) -join "`r`n"
            if ($Mirror) { $template = $template.Replace('# mirror = ""', "mirror = `"$Mirror`"") }
            [IO.File]::WriteAllText($Conf, $template + "`r`n", (New-Object Text.UTF8Encoding($false)))
            Say "wrote $Conf"
        }

        Add-Group
        Add-Path
        Add-Shortcut "singbox-board" $Launcher "" "sing-box in the notification area"
        Add-Shortcut "singbox-board dashboard" $Bin "tui" "singbox-board terminal dashboard"
        if (-not $NoStart) {
            Install-Service
            Start-Service $Service
            Say "started the $Service service"
            Invoke-PostStart
        }
    } finally {
        Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
    }

    Write-Host ""
    Write-Host "singbox-board is installed." -ForegroundColor White
    Write-Host "  sing-box config     $CoreRoot\config.json  (then: singbox-board start)"
    Write-Host "  daemon config       $Conf  (then: Restart-Service singbox-board)"
    Write-Host "  dashboard           singbox-board  (or `"singbox-board dashboard`" in the Start menu)"
    Write-Host "  notification area   `"singbox-board`" in the Start menu  (or: singbox-board tray)"
    Write-Host "  status / logs       singbox-board status | singbox-board logs -f"
    Write-Host "  uninstall           run install.ps1 -Uninstall  (add -Purge to remove data)"
}

function Uninstall-Board {
    if (Get-Service $Service -ErrorAction SilentlyContinue) {
        Stop-Service $Service -Force -ErrorAction SilentlyContinue
        Invoke-Sc @("delete", $Service) | Out-Null
        Say "removed the $Service service"
    }
    Get-Process -Name "singbox-board" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "singbox-board" -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $Menu -ErrorAction SilentlyContinue
    Remove-Path
    if ($Purge) {
        $core = Join-Path $CoreRoot "sing-box.exe"
        if (Test-Path $Conf) {
            $line = Select-String -Path $Conf -Pattern "^binary = ['`"](.*)['`"]" | Select-Object -First 1
            if ($line) { $core = $line.Matches[0].Groups[1].Value }
        }
        Remove-Item -Force $core -ErrorAction SilentlyContinue
        Remove-Item -Recurse -Force $DataRoot -ErrorAction SilentlyContinue
        if (Get-GroupSid) { & net.exe localgroup $Group /delete | Out-Null }
        Say "removed configuration, components and $core (kept the rest of $CoreRoot)"
    }
    Remove-Item -Recurse -Force $Prefix -ErrorAction SilentlyContinue
    Say "singbox-board uninstalled"
}

if (-not (Test-Admin)) { Die "run this script in PowerShell opened as administrator" }
if ($Uninstall) { Uninstall-Board } else { Install-Board }
