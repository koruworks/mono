# SPDX-License-Identifier: MPL-2.0

param(
    [string]$Codex = "codex",
    [Parameter(Mandatory = $true)]
    [string]$Out
)

$ErrorActionPreference = "Stop"
$resolvedParent = Split-Path -Parent $Out
if (-not $resolvedParent) {
    $resolvedParent = "."
}
New-Item -ItemType Directory -Force -Path $resolvedParent | Out-Null
if (Test-Path -LiteralPath $Out) {
    if (Get-ChildItem -LiteralPath $Out -Force | Select-Object -First 1) {
        throw "Output directory must be empty: $Out"
    }
} else {
    New-Item -ItemType Directory -Path $Out | Out-Null
}

$versionOutput = & $Codex --version
if ($LASTEXITCODE -ne 0) {
    throw "Codex version probe failed"
}
& $Codex app-server generate-json-schema --out $Out
if ($LASTEXITCODE -ne 0) {
    throw "Codex schema generation failed"
}

$schemaPath = Join-Path $Out "codex_app_server_protocol.v2.schemas.json"
$schema = (Get-ChildItem -LiteralPath $Out -Filter "*.json" -File -Recurse |
    ForEach-Object { Get-Content -Raw -LiteralPath $_.FullName }) -join "`n"
$required = @(
    '"initialize"',
    '"model/list"',
    '"thread/start"',
    '"turn/start"',
    '"turn/interrupt"',
    '"turn/steer"',
    '"item/agentMessage/delta"',
    '"thread/tokenUsage/updated"',
    '"item/commandExecution/requestApproval"',
    '"item/fileChange/requestApproval"',
    '"item/permissions/requestApproval"',
    '"localImage"',
    '"inputModalities"'
)
foreach ($token in $required) {
    if (-not $schema.Contains($token)) {
        throw "Generated schema is missing required token: $token"
    }
}

$digest = (Get-FileHash -LiteralPath $schemaPath -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output "version=$versionOutput"
Write-Output "protocol=v2"
Write-Output "schema_sha256=$digest"
