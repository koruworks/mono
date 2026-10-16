param([Parameter(ValueFromRemainingArguments=$true)][string[]]$FixtureArgs)
$ErrorActionPreference = 'Stop'
function Trace-Fixture([string]$value) {
    if ($env:CODEX_FIXTURE_TRACE) {
        Add-Content -LiteralPath $env:CODEX_FIXTURE_TRACE -Value $value
    }
}
function Trace-Clock([string]$what) {
    Trace-Fixture ("clock:" + $what + " " + [DateTime]::Now.ToString('HH:mm:ss.ff'))
}
Trace-Clock 'script'
Trace-Fixture ("args:" + ($FixtureArgs -join ','))
Trace-Fixture ("pid:" + $PID)
# The bearer never rides argv (ADR-341): the launch names the variable and
# sets it on this process. The trace says only whether it arrived.
Trace-Fixture ("bearer_env:" + $(if ($env:KORU_RUN_MCP_TOKEN) { 'set' } else { 'unset' }))

# `CODEX_FIXTURE_TURN=register`: before ending its turn, the fixture does
# what a real delegated agent does — writes a file under its own
# `.artifacts/<RunId>/` folder and registers it through the run's MCP
# endpoint with the bearer it was handed. Everything it needs is in the
# launch: the URL is an override on argv, the run id is inside that URL,
# and the folder is under the working directory it was started in.
function Invoke-RunRegistration {
    $override = $FixtureArgs | Where-Object { $_ -like 'mcp_servers.koru_run.url=*' } | Select-Object -First 1
    if (-not $override) { Trace-Fixture 'register:no_endpoint'; return }
    $url = ($override -replace "^mcp_servers\.koru_run\.url=", '').Trim("'")
    if ($url -notmatch '/v1/runs/([^/]+)/mcp$') { Trace-Fixture 'register:unparsable_endpoint'; return }
    $run = $Matches[1]
    if (-not $env:KORU_RUN_MCP_TOKEN) { Trace-Fixture 'register:no_bearer'; return }
    $folder = Join-Path (Join-Path (Get-Location).Path '.artifacts') $run
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    # Exact bytes: `Set-Content -Encoding utf8` on Windows PowerShell adds a
    # BOM and a line ending, and the test weighs what the blob store weighed.
    [System.IO.File]::WriteAllBytes(
        (Join-Path $folder 'result.md'),
        [System.Text.Encoding]::UTF8.GetBytes('FIXTURE_RESULT'))
    $body = @{
        jsonrpc = '2.0'; id = 1; method = 'tools/call'
        params = @{ name = 'create'; arguments = @{ path = 'result.md'; title = 'Fixture result'; mime = 'text/markdown' } }
    } | ConvertTo-Json -Compress -Depth 10
    try {
        $answer = Invoke-RestMethod -Uri $url -Method Post -ContentType 'application/json' `
            -Headers @{ Authorization = ("Bearer " + $env:KORU_RUN_MCP_TOKEN) } -Body $body -TimeoutSec 30
        if ($answer.result.isError) { Trace-Fixture ('register:refused:' + ($answer.result.structuredContent.code)) }
        else { Trace-Fixture 'register:ok' }
    } catch {
        Trace-Fixture ('register:failed:' + $_.Exception.Message)
    }
}
if ($FixtureArgs.Count -eq 1 -and $FixtureArgs[0] -eq '--version') {
    [Console]::Out.WriteLine('codex-cli 0.150.1')
    exit 0
}
if ($FixtureArgs.Count -lt 1 -or $FixtureArgs[0] -ne 'app-server') { exit 2 }

function Send-Frame([object]$value) {
    [Console]::Out.WriteLine(($value | ConvertTo-Json -Compress -Depth 20))
    [Console]::Out.Flush()
}

while (($line = [Console]::In.ReadLine()) -ne $null) {
    $message = $line | ConvertFrom-Json
    Trace-Fixture ("method:" + $message.method)
    switch ($message.method) {
        'initialize' {
            Send-Frame @{ id=$message.id; result=@{ userAgent='koru_terminal/0.150.1 (fixture)'; codexHome='fixture'; platformFamily='windows'; platformOs='windows' } }
            Trace-Clock 'initialized'
        }
        'initialized' {}
        'model/list' {
            Send-Frame @{ id=$message.id; result=@{ data=@(@{ id='fixture-model'; model='fixture-model'; displayName='Fixture'; description='fixture'; isDefault=$true; hidden=$false; defaultReasoningEffort='medium'; supportedReasoningEfforts=@(); inputModalities=@('text','image') }); nextCursor=$null } }
        }
        'thread/start' {
            Send-Frame @{ id=$message.id; result=@{ thread=@{ id='fixture-thread' } } }
        }
        'turn/start' {
            $image = @($message.params.input | Where-Object { $_.type -eq 'localImage' } | Select-Object -First 1)
            Send-Frame @{ id=$message.id; result=@{ turn=@{ id='fixture-turn'; status='inProgress'; items=@() } } }
            Send-Frame @{ method='item/agentMessage/delta'; params=@{ threadId='fixture-thread'; turnId='fixture-turn'; itemId='message-1'; delta='FIXTURE_TEXT' } }
            if ($image.Count -eq 1) {
                Send-Frame @{ method='item/completed'; params=@{ threadId='fixture-thread'; turnId='fixture-turn'; item=@{ type='imageView'; id='image-1'; path=$image[0].path } } }
            }
            # CODEX_FIXTURE_TURN=complete: the turn reports its usage and ends
            # cleanly with no approval, which is what a delegated run needs to
            # reach its end without a person answering (P1.6's process test).
            if ($env:CODEX_FIXTURE_TURN -eq 'register') { Invoke-RunRegistration }
            if ($env:CODEX_FIXTURE_TURN -eq 'complete' -or $env:CODEX_FIXTURE_TURN -eq 'register') {
                Send-Frame @{ method='thread/tokenUsage/updated'; params=@{ threadId='fixture-thread'; turnId='fixture-turn'; tokenUsage=@{ last=@{ inputTokens=120; outputTokens=40; cachedInputTokens=20 }; total=@{ inputTokens=120; outputTokens=40; cachedInputTokens=20 }; modelContextWindow=1000 } } }
                Send-Frame @{ method='turn/completed'; params=@{ threadId='fixture-thread'; turn=@{ id='fixture-turn'; items=@(); status='completed'; error=$null } } }
                continue
            }
            Send-Frame @{ method='item/commandExecution/requestApproval'; id='fixture-approval'; params=@{ threadId='fixture-thread'; turnId='fixture-turn'; itemId='tool-1'; command='redacted' } }
        }
        'turn/interrupt' {
            Send-Frame @{ id=$message.id; result=@{} }
            Send-Frame @{ method='turn/completed'; params=@{ threadId='fixture-thread'; turn=@{ id='fixture-turn'; items=@(); status='interrupted'; error=$null } } }
        }
    }
}
