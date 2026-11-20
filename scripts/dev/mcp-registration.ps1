# SPDX-License-Identifier: MPL-2.0

# Shared by root --mcp and mcp-add. Dot-sourcing defines functions only.
function Assert-McpName([string] $Name) {
    if ($Name -notmatch '^[A-Za-z0-9_][A-Za-z0-9_-]*$') {
        throw "Invalid MCP name '$Name'; use letters, numbers, underscores or hyphens."
    }
}

function Get-BranchMcpName([string] $Repo) {
    $branch = & git -C $Repo branch --show-current
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read the checkout branch for MCP registration.' }
    if ([string]::IsNullOrWhiteSpace($branch)) {
        $commit = & git -C $Repo rev-parse --short HEAD
        if ($LASTEXITCODE -ne 0) { throw "Cannot identify this checkout for MCP registration." }
        $branch = (Split-Path -Leaf $Repo) + "_" + $commit
    }
    $name = "koru_" + ($branch -replace '[^A-Za-z0-9_-]', '_')
    Assert-McpName $name
    return $name
}

function Get-DevelopmentBin([string] $Repo) {
    $target = $env:CARGO_TARGET_DIR
    if ([string]::IsNullOrWhiteSpace($target)) { $target = Join-Path $Repo 'target' }
    if (-not [IO.Path]::IsPathRooted($target)) { $target = Join-Path $Repo $target }
    return Join-Path ([IO.Path]::GetFullPath($target)) 'debug'
}

function ConvertTo-NativeArgument([string] $Value) {
    # Windows CommandLineToArgvW quoting, including trailing backslashes.
    return '"' + (($Value -replace '(\\*)"', '$1$1\"') -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-McpCli([string] $Executable, [string[]] $Arguments, [switch] $CaptureOutput) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $CaptureOutput
    $start.RedirectStandardError = $CaptureOutput
    if ([IO.Path]::GetExtension($Executable) -in @('.cmd', '.bat')) {
        # npm installs .cmd shims. Expand values exactly once from environment
        # inside quotes: %, &, ^ and ! in paths cannot become cmd source.
        $start.FileName = $env:ComSpec
        $start.EnvironmentVariables['MCP_REG_COMMAND'] = $Executable
        $parts = @('"%MCP_REG_COMMAND%"')
        for ($index = 0; $index -lt $Arguments.Count; $index++) {
            if ($Arguments[$index].Contains('"')) { throw 'A CLI argument contains a quote.' }
            $key = "MCP_REG_ARG_$index"
            $start.EnvironmentVariables[$key] = ConvertTo-NativeArgument $Arguments[$index]
            $parts += '%' + $key + '%'
        }
        $start.Arguments = '/d /v:off /s /c "' + ($parts -join ' ') + '"'
    } else {
        $start.FileName = $Executable
        $start.Arguments = ($Arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join ' '
    }
    $process = [Diagnostics.Process]::Start($start)
    try {
        if ($CaptureOutput) {
            # Drain both pipes concurrently: a diagnostic on one stream must
            # not block behind a full buffer on the other.
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
        }
        $process.WaitForExit()
        if ($CaptureOutput) {
            return [pscustomobject]@{
                ExitCode = $process.ExitCode
                StdOut = $stdout.GetAwaiter().GetResult()
                StdErr = $stderr.GetAwaiter().GetResult()
            }
        }
        return $process.ExitCode
    } finally { $process.Dispose() }
}

function Write-McpCliOutput($Result) {
    [Console]::Out.Write($Result.StdOut)
    [Console]::Error.Write($Result.StdErr)
}

function Register-ClaudeMcpBridge([string] $Executable, [string[]] $Arguments, [string] $Name) {
    $result = Invoke-McpCli $Executable $Arguments -CaptureOutput
    $duplicate = $false
    foreach ($line in (($result.StdOut + "`n" + $result.StdErr) -split '\r?\n')) {
        if ($line.Trim() -ceq "MCP server $Name already exists in user config") {
            $duplicate = $true
        }
    }
    if ($result.ExitCode -ne 0 -and $duplicate) {
        # Claude add does not replace an existing user entry. Only its exact
        # named duplicate diagnostic authorizes this targeted replacement;
        # authentication, parsing and other add failures never remove anything.
        Write-Host "[Koru] updating Claude user registration: $Name"
        $removed = Invoke-McpCli $Executable @('mcp', 'remove', '--scope', 'user', $Name) -CaptureOutput
        Write-McpCliOutput $removed
        if ($removed.ExitCode -ne 0) {
            throw "could not update '$Name': removal failed (exit $($removed.ExitCode)); replacement was not attempted."
        }
        try {
            $result = Invoke-McpCli $Executable $Arguments -CaptureOutput
        } catch {
            throw "removed '$Name', but replacement could not start: $($_.Exception.Message). The entry may now be missing; retry this command."
        }
        Write-McpCliOutput $result
        if ($result.ExitCode -ne 0) {
            throw "removed '$Name', but replacement failed (exit $($result.ExitCode)). The entry may now be missing; retry this command."
        }
    } else {
        Write-McpCliOutput $result
    }
    return $result.ExitCode
}

function Register-McpBridge([string] $Name, [string] $Bridge, [string] $RootPath) {
    Assert-McpName $Name
    $serve = @('serve', '--stdio')
    if (-not [string]::IsNullOrWhiteSpace($RootPath)) { $serve += @('--root', $RootPath) }
    $found = 0
    $failed = @()
    foreach ($cli in @('claude', 'codex', 'gemini')) {
        $command = Get-Command $cli -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($null -eq $command) {
            Write-Host "[Koru] $cli is not on PATH; skipped"
            continue
        }
        $found++
        switch ($cli) {
            'claude' { $arguments = @('mcp', 'add', '--scope', 'user', $Name, '--', $Bridge) + $serve }
            'codex' { $arguments = @('mcp', 'add', $Name, '--', $Bridge) + $serve }
            'gemini' { $arguments = @('mcp', 'add', '--scope', 'user', $Name, $Bridge, '--') + $serve }
        }
        try {
            if ($cli -eq 'claude') {
                $code = Register-ClaudeMcpBridge $command.Source $arguments $Name
            } else {
                $code = Invoke-McpCli $command.Source $arguments
            }
            if ($code -ne 0) { $failed += "${cli} (exit $code)" }
            else { Write-Host "[Koru] registered $Name with $cli" }
        } catch {
            $failed += "${cli} ($($_.Exception.Message))"
        }
    }
    if ($found -eq 0) { throw 'None of claude, codex or gemini is on PATH; nothing was registered.' }
    if ($failed.Count -gt 0) {
        throw "MCP registration failed: $($failed -join ', '). Successful registrations in other clients were retained."
    }
}
