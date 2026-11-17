param([Parameter(ValueFromRemainingArguments=$true)][string[]]$FixtureArgs)
$ErrorActionPreference = 'Stop'
# Only the language and .NET from here on, no cmdlet from a module: the first
# use of one makes Windows PowerShell import its module, type and format files
# and all, and on a CI runner that took seconds. The trace there showed 5.6 s
# between this script starting and it reading its first request, spent on
# three Add-Content calls, and the provider's 10 s initialize limit expired.
[void][System.Reflection.Assembly]::LoadWithPartialName('System.Web.Extensions')
$json = [System.Web.Script.Serialization.JavaScriptSerializer]::new()
$json.MaxJsonLength = [int]::MaxValue
function Trace-Fixture([string]$value) {
    if ($env:CODEX_FIXTURE_TRACE) {
        [System.IO.File]::AppendAllText($env:CODEX_FIXTURE_TRACE, $value + "`r`n")
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
    $override = @($FixtureArgs | Where-Object { $_ -like 'mcp_servers.koru_run.url=*' })
    if ($override.Count -eq 0) { Trace-Fixture 'register:no_endpoint'; return }
    $url = ($override[0] -replace "^mcp_servers\.koru_run\.url=", '').Trim("'")
    if ($url -notmatch '/v1/runs/([^/]+)/mcp$') { Trace-Fixture 'register:unparsable_endpoint'; return }
    $run = $Matches[1]
    if (-not $env:KORU_RUN_MCP_TOKEN) { Trace-Fixture 'register:no_bearer'; return }
    $folder = [System.IO.Path]::Combine([System.IO.Directory]::GetCurrentDirectory(), '.artifacts', $run)
    [void][System.IO.Directory]::CreateDirectory($folder)
    # Exact bytes: the test weighs what the blob store weighed.
    [System.IO.File]::WriteAllBytes(
        [System.IO.Path]::Combine($folder, 'result.md'),
        [System.Text.Encoding]::UTF8.GetBytes('FIXTURE_RESULT'))
    $body = $json.Serialize(@{
        jsonrpc = '2.0'; id = 1; method = 'tools/call'
        params = @{ name = 'create'; arguments = @{ path = 'result.md'; title = 'Fixture result'; mime = 'text/markdown' } }
    })
    try {
        $client = [System.Net.WebClient]::new()
        $client.Headers['Content-Type'] = 'application/json'
        $client.Headers['Authorization'] = "Bearer " + $env:KORU_RUN_MCP_TOKEN
        $answer = $json.DeserializeObject($client.UploadString($url, 'POST', $body))
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
    [Console]::Out.WriteLine($json.Serialize($value))
    [Console]::Out.Flush()
}

# Scripted native acceptance only. No provider credentials or global config.
function Native-Rpc([string]$method, [object]$params) {
    $override = @($FixtureArgs | Where-Object { $_ -like 'mcp_servers.koru_run.url=*' })
    if ($override.Count -ne 1 -or -not $env:KORU_RUN_MCP_TOKEN) { throw 'native endpoint unavailable' }
    $url = ($override[0] -replace '^mcp_servers\.koru_run\.url=', '').Trim("'")
    $request = [System.Net.HttpWebRequest]::Create($url)
    $request.Method = 'POST'
    $request.ContentType = 'application/json'
    $request.Headers['Authorization'] = 'Bearer ' + $env:KORU_RUN_MCP_TOKEN
    $request.Timeout = 30000
    $request.ReadWriteTimeout = 30000
    $body = [System.Text.Encoding]::UTF8.GetBytes($json.Serialize(@{jsonrpc='2.0';id=1;method=$method;params=$params}))
    $request.ContentLength = $body.Length
    $stream = $request.GetRequestStream()
    try { $stream.Write($body, 0, $body.Length) } finally { $stream.Dispose() }
    $response = $request.GetResponse()
    try {
        $reader = [System.IO.StreamReader]::new($response.GetResponseStream())
        try { $answer = $json.DeserializeObject($reader.ReadToEnd()) } finally { $reader.Dispose() }
        if ($answer.error) { throw 'native RPC refused' }
        return $answer.result
    } finally { $response.Dispose() }
}
function Native-Call([string]$id, [string]$name, [object]$arguments, [bool]$refused = $false) {
    Trace-Fixture ('native:call:' + $id)
    $item = @{type='mcpToolCall';id=$id;server='koru_run';tool=$name;arguments=$arguments;status='inProgress'}
    Send-Frame @{method='item/started';params=@{threadId='fixture-thread';turnId='fixture-turn';item=$item}}
    $result = Native-Rpc 'tools/call' @{name=$name;arguments=$arguments}
    if ([bool]$result.isError -ne $refused) { throw 'native tool outcome mismatch' }
    $item.status = $(if ($refused) {'failed'} else {'completed'})
    $item.result = $result
    Send-Frame @{method='item/completed';params=@{threadId='fixture-thread';turnId='fixture-turn';item=$item}}
    return $result.structuredContent
}
function Native-Discover {
    $catalog = Native-Rpc 'tools/list' @{}
    $names = @($catalog.tools | ForEach-Object { $_.name } | Sort-Object)
    # Listed at thread start, before the Run's first input. Exactly one of two
    # catalogs. Sixteen: the thirteen, read_bot_request and the two ask tools,
    # for a Run with a reply of its own whose Bot may ask other Bots (a task
    # in the Bot's conversation). Fourteen: without the two ask tools, for a
    # Run with no reply of its own (a task started through the public API) or
    # a Bot whose "May ask other bots" is off. The test says which it expects.
    $joined = $names -join ','
    if ($joined -eq 'ask_bot,correct_memory,create_document,create_routine,edit_document,forget_memory,list_askable_bots,list_memory,list_routines,read_bot_request,read_document,read_memory,read_routine_creation,remember_memory,retry_routine_creation,search_documents') { Trace-Fixture 'native:catalog:16'; return }
    if ($joined -ne 'correct_memory,create_document,create_routine,edit_document,forget_memory,list_memory,list_routines,read_bot_request,read_document,read_memory,read_routine_creation,remember_memory,retry_routine_creation,search_documents') { Trace-Fixture ('native:catalog-mismatch:' + $names.Count); throw 'native catalog mismatch' }
    Trace-Fixture 'native:catalog:14'
}
function Invoke-NativePages {
    Trace-Fixture 'native:turn'
    $config = $json.DeserializeObject([System.IO.File]::ReadAllText($env:CODEX_FIXTURE_NATIVE_CONFIG))
    try {
        $create = @{operation_id='11111111-1111-4111-8111-111111111111';workspace_id=$config.workspace;title='Native fixture result';initial_operations=@(@{op='insert';node_id='body';parent_id='root';after_node_id=$null;kind='paragraph';text='NATIVE ORIGINAL'})}
        $created = Native-Call 'native-create' 'create_document' $create
        if ($created.status -ne 'saved') { throw 'native create unconfirmed' }
        $repeat = Native-Call 'native-create-replay' 'create_document' $create
        if ($repeat.document_id -ne $created.document_id -or $repeat.base_revision -ne $created.base_revision) { throw 'native create replay changed' }
        $readArgs = @{workspace_id=$config.workspace;document_id=$created.document_id}
        $before = Native-Call 'native-read-before' 'read_document' $readArgs
        $node = @($before.nodes | Where-Object { $_.content.text -eq 'NATIVE ORIGINAL' })
        if ($node.Count -ne 1) { throw 'native body missing' }
        $edit = @{operation_id='22222222-2222-4222-8222-222222222222';workspace_id=$config.workspace;document_id=$created.document_id;base_revision=$before.revision;operations=@(@{op='replace_text';node_id=$node[0].node_id;expected='NATIVE ORIGINAL';text='NATIVE EDITED'})}
        $edited = Native-Call 'native-edit' 'edit_document' $edit
        if ($edited.status -ne 'saved') { throw 'native edit unconfirmed' }
        $repeat = Native-Call 'native-edit-replay' 'edit_document' $edit
        if ($repeat.document_id -ne $created.document_id -or $repeat.base_revision -ne $edited.base_revision) { throw 'native edit replay changed' }
        $changed = $json.DeserializeObject($json.Serialize($create)); $changed.title = 'Changed payload'
        [void](Native-Call 'native-create-conflict' 'create_document' $changed $true)
        $stale = $json.DeserializeObject($json.Serialize($edit)); $stale.operation_id = '33333333-3333-4333-8333-333333333333'
        [void](Native-Call 'native-stale-edit' 'edit_document' $stale $true)
        $after = Native-Call 'native-read-after' 'read_document' $readArgs
        if (@($after.nodes | Where-Object { $_.content.text -eq 'NATIVE EDITED' }).Count -ne 1) { throw 'native edited body missing' }
        [System.IO.File]::WriteAllText($config.report, $json.Serialize(@{status='ready';document=$created.document_id}))
        $deadline = [DateTime]::UtcNow.AddSeconds(30)
        while (-not [System.IO.File]::Exists($config.finish)) {
            if ([DateTime]::UtcNow -gt $deadline) { throw 'native inspection timeout' }
            [System.Threading.Thread]::Sleep(25)
        }
    } catch {
        Trace-Fixture 'native:failed'
        [System.IO.File]::WriteAllText($config.report, '{"status":"failed"}')
        throw 'native scripted sequence failed'
    }
}

Trace-Clock 'reading'
$first = $true
while (($line = [Console]::In.ReadLine()) -ne $null) {
    if ($first) { Trace-Clock 'read' }
    $message = $json.DeserializeObject($line)
    if ($first) { Trace-Clock 'parsed'; $first = $false }
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
            if ($env:CODEX_FIXTURE_TURN -eq 'native') {
                if ($message.params.model -ne 'fixture-model') { throw 'native requested model mismatch' }
                Trace-Fixture 'native:model:fixture-model'
                Native-Discover
            }
            Send-Frame @{ id=$message.id; result=@{ thread=@{ id='fixture-thread' } } }
        }
        'turn/start' {
            $image = @($message.params.input | Where-Object { $_.type -eq 'localImage' })
            if ($env:CODEX_FIXTURE_TURN -eq 'native') {
                # Counts only: a picture's path names the disposable root.
                Trace-Fixture ('native:images:' + $image.Count)
                Trace-Fixture ('native:images-on-disk:' + @($image | Where-Object { [System.IO.File]::Exists($_.path) }).Count)
            }
            Send-Frame @{ id=$message.id; result=@{ turn=@{ id='fixture-turn'; status='inProgress'; items=@() } } }
            $text = $(if ($env:CODEX_FIXTURE_TURN -eq 'native') { 'FIXTURE_NATIVE_TEXT' } else { 'FIXTURE_TEXT' })
            Send-Frame @{ method='item/agentMessage/delta'; params=@{ threadId='fixture-thread'; turnId='fixture-turn'; itemId='message-1'; delta=$text } }
            if ($image.Count -ge 1) {
                Send-Frame @{ method='item/completed'; params=@{ threadId='fixture-thread'; turnId='fixture-turn'; item=@{ type='imageView'; id='image-1'; path=$image[0].path } } }
            }
            # CODEX_FIXTURE_TURN=complete: the turn reports its usage and ends
            # cleanly with no approval, which is what a delegated run needs to
            # reach its end without a person answering (P1.6's process test).
            if ($env:CODEX_FIXTURE_TURN -eq 'register') { Invoke-RunRegistration }
            if ($env:CODEX_FIXTURE_TURN -eq 'native') { Invoke-NativePages }
            if ($env:CODEX_FIXTURE_TURN -eq 'complete' -or $env:CODEX_FIXTURE_TURN -eq 'register' -or $env:CODEX_FIXTURE_TURN -eq 'native') {
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
