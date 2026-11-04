# SPDX-License-Identifier: MPL-2.0

param(
    [ValidateSet("Build", "Run", "BuildAndRun")]
    [string]$Action = "BuildAndRun",
    [ValidateRange(0, 65535)]
    [int]$Build = 0,
    [switch]$ReuseExisting,
    [string]$DataRoot = "",
    [switch]$InstallCertificate,
    [string]$CertificatePath = "",
    [string]$CertificateThumbprint = ""
)

# Build and/or launch a signed, notification-capable reminder rehearsal on an
# isolated root. Only the public-certificate import runs elevated. Build writes
# a small pointer under target so Run never needs copied build or root values.
# ASCII only: Windows PowerShell 5.1 reads a BOM-less script as ANSI.
$ErrorActionPreference = "Stop"

function Get-TrustedCertificate([string]$Thumbprint) {
    Get-ChildItem Cert:\LocalMachine\TrustedPeople |
        Where-Object Thumbprint -eq $Thumbprint |
        Select-Object -First 1
}

if ($InstallCertificate) {
    if ([string]::IsNullOrWhiteSpace($CertificatePath) -or
        [string]::IsNullOrWhiteSpace($CertificateThumbprint)) {
        throw "CertificatePath and CertificateThumbprint are required"
    }
    $certificate = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2(
        [System.IO.Path]::GetFullPath($CertificatePath)
    )
    if ($certificate.Thumbprint -ne $CertificateThumbprint) {
        throw "The public certificate does not have the expected thumbprint"
    }
    Import-Certificate `
        -FilePath $CertificatePath `
        -CertStoreLocation "Cert:\LocalMachine\TrustedPeople" | Out-Null
    if ($null -eq (Get-TrustedCertificate $CertificateThumbprint)) {
        throw "The certificate was not installed in LocalMachine TrustedPeople"
    }
    Write-Host "[Koru] trusted the public development certificate"
    exit 0
}

function Quote-PowerShellLiteral([string]$Value) {
    return "'" + $Value.Replace("'", "''") + "'"
}

function Install-DevelopmentCertificate([string]$Path, [string]$Thumbprint) {
    if ($null -ne (Get-TrustedCertificate $Thumbprint)) {
        Write-Host "[Koru] development certificate is already trusted"
        return
    }
    Write-Host "[Koru] Windows will ask once to trust the public certificate"
    $command = "& " + (Quote-PowerShellLiteral $PSCommandPath) +
        " -InstallCertificate -CertificatePath " +
        (Quote-PowerShellLiteral $Path) + " -CertificateThumbprint " +
        (Quote-PowerShellLiteral $Thumbprint)
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $powershell = Join-Path $env:SystemRoot `
        "System32\WindowsPowerShell\v1.0\powershell.exe"
    $elevated = Start-Process `
        -FilePath $powershell `
        -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", $encoded) `
        -Verb RunAs -Wait -PassThru
    if ($elevated.ExitCode -ne 0 -or
        $null -eq (Get-TrustedCertificate $Thumbprint)) {
        throw "Certificate trust failed or the elevation prompt was cancelled"
    }
}

function Invoke-AgentClient([string]$Executable, [string[]]$Arguments) {
    $stdout = Join-Path $env:TEMP `
        ("koru-reminder-status-" + [guid]::NewGuid().ToString("N") + ".out")
    $stderr = $stdout + ".err"
    try {
        # koru-agent.exe is a GUI-subsystem executable. PowerShell's call
        # operator does not wait for it and leaves LASTEXITCODE stale. The
        # status verb also exits zero for the legitimate "not running" answer,
        # so readiness is the response text, not only the process exit code.
        $probe = Start-Process `
            -FilePath $Executable `
            -ArgumentList $Arguments `
            -WorkingDirectory (Split-Path -Parent $Executable) `
            -RedirectStandardOutput $stdout `
            -RedirectStandardError $stderr `
            -WindowStyle Hidden `
            -Wait `
            -PassThru
        $answer = if (Test-Path -LiteralPath $stdout) {
            Get-Content -LiteralPath $stdout -Raw
        } else {
            ""
        }
        $failure = if (Test-Path -LiteralPath $stderr) {
            Get-Content -LiteralPath $stderr -Raw
        } else {
            ""
        }
        return [pscustomobject]@{
            ExitCode = $probe.ExitCode
            Stdout = $answer
            Stderr = $failure
        }
    } finally {
        Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue
    }
}

function Test-NativeReady([string]$Executable, [string[]]$Arguments) {
    $probe = Invoke-AgentClient $Executable $Arguments
    return $probe.ExitCode -eq 0 -and
        $probe.Stdout -match '(?m)^running, version '
}

function Wait-NativeReady(
    [string]$Name,
    [string]$Executable,
    [string[]]$Arguments
) {
    for ($attempt = 0; $attempt -lt 150; $attempt++) {
        if (Test-NativeReady $Executable $Arguments) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "$Name did not become ready"
}

function Restart-NativeAgentIfExited(
    [string]$Name,
    [string]$Executable,
    [string[]]$LaunchArguments,
    [string[]]$StatusArguments,
    [System.Diagnostics.Process]$Process
) {
    if ($null -ne $Process) {
        $Process.Refresh()
        if (-not $Process.HasExited) {
            Write-Warning "$Name is alive but its endpoint remains unavailable"
            return $Process
        }
        Write-Warning "$Name exited with code $($Process.ExitCode); restarting it"
    } else {
        Write-Warning "$Name has no live launch process; restarting it"
    }

    $replacement = Start-Process `
        -FilePath $Executable `
        -ArgumentList $LaunchArguments `
        -WorkingDirectory (Split-Path -Parent $Executable) `
        -WindowStyle Hidden `
        -PassThru
    try {
        Wait-NativeReady $Name $Executable $StatusArguments
        Write-Host "[Koru] $Name restarted"
    } catch {
        # Keep the rehearsal supervisor alive. The next bounded health cycle
        # reports and retries again instead of letting its job tear down every
        # other Agent child while Koru itself remains open.
        Write-Warning "$Name restart did not become ready: $($_.Exception.Message)"
    }
    return $replacement
}

function Resolve-DisposablePath(
    [string]$Repo,
    [string]$Value,
    [string]$Description
) {
    $candidate = if ([IO.Path]::IsPathRooted($Value)) {
        $Value
    } else {
        Join-Path $Repo $Value
    }
    $resolved = [IO.Path]::GetFullPath($candidate)
    $targetPrefix = [IO.Path]::GetFullPath((Join-Path $Repo "target")) + `
        [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith(
        $targetPrefix,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "$Description must be a disposable location under this worktree's target directory"
    }
    return $resolved
}

function Read-LatestBuild([string]$Path, [string]$Repo) {
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "No rooted signed build is ready. Run 'koru.bat build-root-signed' first."
    }
    try {
        $saved = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    } catch {
        throw "The rooted signed build record is unreadable. Run 'koru.bat build-root-signed' again."
    }
    if ($saved.FormatVersion -ne 1 -or
        $saved.Build -lt 1 -or
        [string]::IsNullOrWhiteSpace($saved.Bundle) -or
        [string]::IsNullOrWhiteSpace($saved.DataRoot) -or
        [string]::IsNullOrWhiteSpace($saved.CertificateThumbprint)) {
        throw "The rooted signed build record is incomplete. Run 'koru.bat build-root-signed' again."
    }
    return [pscustomobject]@{
        Build = [int]$saved.Build
        Bundle = Resolve-DisposablePath $Repo $saved.Bundle "Saved bundle"
        DataRoot = Resolve-DisposablePath $Repo $saved.DataRoot "Saved data root"
        CertificateThumbprint = [string]$saved.CertificateThumbprint
    }
}

function Write-LatestBuild(
    [string]$Path,
    [int]$Build,
    [string]$Bundle,
    [string]$DataRoot,
    [string]$CertificateThumbprint
) {
    $record = [ordered]@{
        FormatVersion = 1
        Build = $Build
        Bundle = $Bundle
        DataRoot = $DataRoot
        CertificateThumbprint = $CertificateThumbprint
    }
    $temporary = $Path + ".part"
    $record | ConvertTo-Json | Set-Content -LiteralPath $temporary -Encoding ASCII
    Move-Item -LiteralPath $temporary -Destination $Path -Force
}

$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$latestBuildPath = Join-Path $repo "target\root-signed-latest.json"
$previousPublisher = $env:KORU_WINDOWS_PACKAGE_PUBLISHER
$previousSigning = $env:KORU_SIGNTOOL_COMMAND

Push-Location -LiteralPath $repo
try {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "Reminder native-notification rehearsal is available only on Windows"
    }

    New-Item -ItemType Directory -Force -Path (Join-Path $repo "target") | Out-Null

    if ($Action -eq "Run") {
        if ($Build -ne 0 -or $ReuseExisting -or
            -not [string]::IsNullOrWhiteSpace($DataRoot)) {
            throw "Run reads the latest rooted signed build and does not accept Build, ReuseExisting, or DataRoot"
        }
        $saved = Read-LatestBuild $latestBuildPath $repo
        $Build = $saved.Build
        $bundle = $saved.Bundle
        $root = $saved.DataRoot
        $certificateThumbprint = $saved.CertificateThumbprint
        $devCert = $null
        $signTool = $null
    } else {
        $pattern = Join-Path ${env:ProgramFiles(x86)} `
            "Windows Kits\10\bin\*\x64\signtool.exe"
        $signTool = Get-ChildItem $pattern |
            Sort-Object FullName -Descending |
            Select-Object -First 1
        if ($null -eq $signTool) {
            throw "SignTool was not found; install the Windows SDK signing tools, then retry"
        }
        $makeAppx = Join-Path $signTool.Directory.FullName "MakeAppx.exe"
        if (-not (Test-Path -LiteralPath $makeAppx)) {
            throw "MakeAppx was not found beside SignTool; install the Windows SDK packaging tools, then retry"
        }

        $devCert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
            Where-Object {
                $_.FriendlyName -eq "Koru reminder development" -and
                $_.NotAfter -gt (Get-Date).AddDays(1) -and
                $_.HasPrivateKey
            } |
            Sort-Object NotAfter -Descending |
            Select-Object -First 1
        if ($null -eq $devCert) {
            Write-Host "[Koru] creating a per-user reminder development certificate"
            $devCert = New-SelfSignedCertificate `
                -Type Custom `
                -KeyUsage DigitalSignature `
                -Subject "CN=Koru Development" `
                -CertStoreLocation "Cert:\CurrentUser\My" `
                -TextExtension @(
                    "2.5.29.37={text}1.3.6.1.5.5.7.3.3",
                    "2.5.29.19={text}"
                ) `
                -FriendlyName "Koru reminder development" `
                -HashAlgorithm SHA256
        }
        if ($devCert.Subject -ne "CN=Koru Development") {
            throw "The development certificate subject must be CN=Koru Development"
        }

        $publicCert = Join-Path $repo "target\koru-reminder-development.cer"
        Export-Certificate -Cert $devCert -FilePath $publicCert -Force | Out-Null
        Install-DevelopmentCertificate $publicCert $devCert.Thumbprint
        $certificateThumbprint = $devCert.Thumbprint

        if ($ReuseExisting -and $Build -eq 0) {
            throw "ReuseExisting requires an explicit -Build number"
        }
        if ($Build -eq 0) {
            do {
                $Build = Get-Random -Minimum 1 -Maximum 65536
                $candidate = Join-Path $repo ("target\reminder-dev-bundle-" + $Build)
            } while (Test-Path -LiteralPath $candidate)
        }

        $bundle = Join-Path $repo ("target\reminder-dev-bundle-" + $Build)
        if ([string]::IsNullOrWhiteSpace($DataRoot)) {
            $root = Join-Path $repo ("target\reminder-dev-root-" + $Build)
        } else {
            $root = Resolve-DisposablePath $repo $DataRoot "DataRoot"
        }
        if ((Test-Path -LiteralPath $bundle) -and -not $ReuseExisting) {
            throw "Build $Build already has an extracted bundle; choose another -Build value"
        }
        if ($ReuseExisting -and -not (Test-Path -LiteralPath $bundle)) {
            throw "Build $Build has no extracted bundle to reuse"
        }
    }
    New-Item -ItemType Directory -Force -Path $root | Out-Null

    $windowsPowerShell = Join-Path $env:SystemRoot `
        "System32\WindowsPowerShell\v1.0\powershell.exe"
    & $windowsPowerShell `
        -NoProfile `
        -ExecutionPolicy Bypass `
        -File (Join-Path $PSScriptRoot "stop-worktree.ps1")
    if ($LASTEXITCODE -ne 0) {
        throw "The worktree processes could not be stopped"
    }
    $oldNames = @(
        "koru.exe",
        "koru-agent.exe",
        "koru-com-notification.exe",
        # Retired executables (the gateway and session host are now roles of
        # koru-agent.exe; koru-terminal.exe and koru-ai-chat.exe are gone),
        # kept because an older bundle in target/ still carries these images
        # and this list is what stops one that is running.
        "koru-api-gateway.exe",
        "koru-session-host.exe",
        "koru-voice-worker.exe",
        "koru-terminal.exe",
        "koru-ai-chat.exe",
        "koru-mcp.exe",
        "koru-store-server.exe"
    )
    $oldBundleImages = @(
        Get-ChildItem (Join-Path $repo "target") `
            -Directory `
            -Filter "reminder-dev-bundle-*" `
            -ErrorAction SilentlyContinue |
            ForEach-Object {
                Get-ChildItem $_.FullName -File |
                    Where-Object { $oldNames -contains $_.Name } |
                    Select-Object -ExpandProperty FullName
            }
    )
    if ($oldBundleImages.Count -gt 0) {
        & (Join-Path $PSScriptRoot "stop-exact-exe.ps1") `
            -ExecutablePath $oldBundleImages
    }

    if ($Action -eq "Run" -or $ReuseExisting) {
        Write-Host "[Koru] reusing signed reminder package $Build"
    } else {
        $quote = [char]34
        $env:KORU_WINDOWS_PACKAGE_PUBLISHER = $devCert.Subject
        $env:KORU_SIGNTOOL_COMMAND = $quote + $signTool.FullName + $quote +
            " sign /sha1 " + $devCert.Thumbprint + " /fd SHA256 {file}"

        Write-Host "[Koru] building signed reminder package $Build"
        & (Join-Path $repo "koru.bat") `
            dist-local beta 0.0.0-reminder-dev --build $Build
        if ($LASTEXITCODE -ne 0) {
            throw "The signed reminder package did not build"
        }

        $zip = Join-Path $repo `
            "dist\cloud\koru-0.0.0-reminder-dev-windows-x86_64.zip"
        if (-not (Test-Path -LiteralPath $zip)) {
            throw "The release bundle was not produced at $zip"
        }
        Expand-Archive -LiteralPath $zip -DestinationPath $bundle
    }

    $identity = Join-Path $bundle "koru-identity.msix"
    if (-not (Test-Path -LiteralPath $identity)) {
        throw "The saved signed bundle is incomplete. Run 'koru.bat build-root-signed' again."
    }
    $signature = Get-AuthenticodeSignature $identity
    if ($null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Thumbprint -ne $certificateThumbprint) {
        throw "The identity package signature does not match the verified rooted signed build"
    }
    if ($null -eq (Get-TrustedCertificate $certificateThumbprint)) {
        throw "The rooted signed build certificate is no longer trusted. Run 'koru.bat build-root-signed' again."
    }

    $agent = Join-Path $bundle "koru-agent.exe"
    $comNotification = Join-Path $bundle "koru-com-notification.exe"
    $app = Join-Path $bundle "koru.exe"
    if (-not (Test-Path -LiteralPath $agent) -or
        -not (Test-Path -LiteralPath $comNotification) -or
        -not (Test-Path -LiteralPath $app)) {
        throw "The release bundle is missing Koru, its Agent, or its notification COM broker"
    }

    $expectedVersion = "0.0.0.$Build"
    Write-Host "[Koru] registering the Windows package identity"
    try {
        Add-AppxPackage `
            -Path $identity `
            -ExternalLocation $bundle `
            -ForceUpdateFromAnyVersion `
            -ErrorAction Stop
    } catch {
        throw "Windows package registration failed: $($_.Exception.Message)"
    }
    $registered = Get-AppxPackage -Name Koru.Desktop.Beta |
        Where-Object { $_.Version.ToString() -eq $expectedVersion } |
        Select-Object -First 1
    if ($null -eq $registered) {
        throw "Windows reported success but Koru.Desktop.Beta $expectedVersion is absent"
    }

    if ($Action -ne "Run") {
        Write-LatestBuild `
            $latestBuildPath `
            $Build `
            $bundle `
            $root `
            $certificateThumbprint
    }
    if ($Action -eq "Build") {
        Write-Host ""
        Write-Host "[Koru] rooted signed build is ready"
        Write-Host "[Koru] package: $($registered.Name) $($registered.Version)"
        Write-Host "[Koru] bundle:  $bundle"
        Write-Host "[Koru] root:    $root"
        Write-Host "[Koru] next: koru.bat run-root-signed"
        exit 0
    }

    # Start-Process flattens ArgumentList into one Windows command line. Quote
    # the root explicitly so a path with spaces stays one argument.
    $escapedRoot = $root -replace '(\\+)$', '$1$1'
    $rootArg = [char]34 + $escapedRoot + [char]34
    $rootedAgentProcess = Start-Process `
        -FilePath $agent `
        -ArgumentList @("--root", $rootArg, "--tray") `
        -WorkingDirectory $bundle `
        -WindowStyle Hidden `
        -PassThru
    Wait-NativeReady "The rooted Koru Agent" $agent @("--root", $rootArg, "status")

    Start-Process `
        -FilePath $app `
        -ArgumentList @("--root", $rootArg) `
        -WorkingDirectory $bundle | Out-Null

    Write-Host ""
    Write-Host "[Koru] reminder rehearsal is ready"
    Write-Host "[Koru] package: $($registered.Name) $($registered.Version)"
    Write-Host "[Koru] bundle:  $bundle"
    Write-Host "[Koru] root:    $root"
    Write-Host "[Koru] next: open Settings > Reminders and press Test notification"
    Write-Host "[Koru] leave this PowerShell window open while testing"
    Write-Host "[Koru] press Ctrl+C when the reminder rehearsal is finished"

    # Some development terminals place descendants in a job that is cleaned up
    # as soon as the invoking script returns. Keep the rehearsal attached and
    # probe the endpoint rather than the launch PID. The package's dedicated
    # one-shot COM broker owns Windows notification activation now; a default
    # Agent is neither needed for routing nor entitled to add a second tray icon.
    $rootedMisses = 0
    while ($true) {
        # Status takes the Agent mutex and starts a short-lived GUI-subsystem
        # client. Ten seconds is enough observability without turning the
        # development monitor into background load of its own. A projection or
        # native-adapter retry can delay one answer, so only a sustained outage
        # ends the rehearsal.
        Start-Sleep -Seconds 10
        if (Test-NativeReady $agent @("--root", $rootArg, "status")) {
            if ($rootedMisses -gt 0) {
                Write-Host "[Koru] rooted Agent endpoint recovered"
            }
            $rootedMisses = 0
        } else {
            $rootedMisses++
            Write-Warning "Rooted Agent endpoint missed health check $rootedMisses of 3"
        }
        if ($rootedMisses -ge 3) {
            $rootedAgentProcess = Restart-NativeAgentIfExited `
                "The rooted Koru Agent" `
                $agent `
                @("--root", $rootArg, "--tray") `
                @("--root", $rootArg, "status") `
                $rootedAgentProcess
            $rootedMisses = 0
        }
    }
} catch {
    Write-Error $_
    exit 1
} finally {
    $env:KORU_WINDOWS_PACKAGE_PUBLISHER = $previousPublisher
    $env:KORU_SIGNTOOL_COMMAND = $previousSigning
    Pop-Location
}
