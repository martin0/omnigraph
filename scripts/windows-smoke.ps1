# Smoke test for local storage roots on Windows (issue #907).
#
# Exercises the paths that failed while a local root was spelled with the
# verbatim `\\?\` prefix: branches on a standalone graph, a local cluster root
# (plan, apply, direct data commands) and a server booted from that root.
#
#   pwsh scripts/windows-smoke.ps1 -Bin target/debug
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Bin,
    [int]$Port = 18907
)

$ErrorActionPreference = 'Stop'
$og = (Resolve-Path (Join-Path $Bin 'omnigraph.exe')).Path
$server = (Resolve-Path (Join-Path $Bin 'omnigraph-server.exe')).Path
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("og smoke " + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $work | Out-Null
Write-Host "work directory: $work"

function Invoke-Og {
    param([Parameter(Mandatory = $true)][string]$Step, [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest)
    Write-Host "::group::$Step"
    Write-Host "omnigraph $($Rest -join ' ')"
    $output = & $og @Rest 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $output | ForEach-Object { Write-Host $_ }
    Write-Host "::endgroup::"
    if ($code -ne 0) { throw "$Step failed with exit code $code" }
    if ($output -match '\\\\\?\\') { throw "$Step printed a verbatim path" }
    return ($output -join "`n")
}

# A direct write on a cluster root keeps its admission lock. Release exactly
# the lock that write reported, as an operator would before the next writer.
function Unlock-Root {
    param([string]$Root)
    $status = Invoke-Og 'cluster status' cluster status --target $Root --json
    if ($status -match '"lock_id"\s*:\s*"([^"]+)"') {
        Invoke-Og 'cluster force-unlock' cluster force-unlock $Matches[1] --target $Root | Out-Null
    }
}

Set-Content -Path (Join-Path $work 'schema.pg') -Encoding utf8 -Value @'
node Person {
  name: String @key
  age: I32?
}

edge Knows: Person -> Person { @key(@src, @dst) }
'@
Set-Content -Path (Join-Path $work 'queries.gq') -Encoding utf8 -Value @'
query people() {
  match { $p: Person }
  return { $p.name }
  order { $p.name asc }
  limit 10
}
'@
Set-Content -Path (Join-Path $work 'seed.jsonl') -Encoding utf8 -Value @'
{"type":"Person","data":{"name":"ada","age":36}}
{"type":"Person","data":{"name":"grace"}}
{"edge":"Knows","from":"ada","to":"grace","data":{}}
'@
Set-Content -Path (Join-Path $work 'delta.jsonl') -Encoding utf8 -Value @'
{"type":"Person","data":{"name":"linus"}}
'@
Set-Content -Path (Join-Path $work 'delta2.jsonl') -Encoding utf8 -Value @'
{"type":"Person","data":{"name":"margaret"}}
'@

Push-Location $work
try {
    # ---- standalone graph: every target spelling, then branches -------------
    $graph = Join-Path $work 'graph.omni'
    $graphUri = 'file:///' + ($graph -replace '\\', '/' -replace ' ', '%20')
    Invoke-Og 'init' init --schema schema.pg graph.omni | Out-Null
    Invoke-Og 'load main' load --data seed.jsonl --mode merge --target ./graph.omni | Out-Null
    foreach ($target in @('./graph.omni', $graph, $graphUri)) {
        Invoke-Og "snapshot via $target" snapshot --target $target --branch main --json | Out-Null
    }

    Invoke-Og 'branch create' branch create --from main review/one --target ./graph.omni | Out-Null
    Invoke-Og 'load onto branch' load --data delta.jsonl --mode merge --branch review/one --target ./graph.omni | Out-Null
    Invoke-Og 'load --from' load --data delta2.jsonl --mode merge --from main --branch review-two --target ./graph.omni | Out-Null
    $branches = Invoke-Og 'branch list' branch list --target ./graph.omni
    foreach ($name in @('review/one', 'review-two')) {
        if ($branches -notmatch [regex]::Escape($name)) { throw "branch list is missing $name" }
    }
    Invoke-Og 'branch diff' branch diff review/one --into main --target ./graph.omni --json | Out-Null
    Invoke-Og 'merge dry run' branch merge review/one --into main --dry-run --target ./graph.omni --json | Out-Null
    Invoke-Og 'merge' branch merge review/one --into main --delete-branch --target ./graph.omni | Out-Null
    $people = Invoke-Og 'query main' query people --query queries.gq --format json --target ./graph.omni
    if ($people -notmatch 'linus') { throw 'the merged branch row is not on main' }
    $review = Invoke-Og 'query branch' query people --query queries.gq --format json --branch review-two --target ./graph.omni
    if ($review -notmatch 'margaret') { throw 'the review branch row is missing' }

    # ---- local cluster root ---------------------------------------------------
    New-Item -ItemType Directory -Force 'cluster src', 'cluster data' | Out-Null
    Copy-Item schema.pg 'cluster src/schema.pg'
    Set-Content -Path 'cluster src/cluster.yaml' -Encoding utf8 -Value @'
version: 1
state: { backend: cluster, lock: true }
graphs:
  knowledge:
    schema: schema.pg
'@
    $root = Join-Path $work 'cluster data'
    $config = Join-Path $work 'cluster src'
    Invoke-Og 'cluster validate' cluster validate --config $config | Out-Null
    Invoke-Og 'cluster plan' cluster plan --target $root --config $config --json | Out-Null
    Invoke-Og 'cluster apply' cluster apply --target $root --config $config --as act-ci --json | Out-Null
    if (-not (Test-Path (Join-Path $root 'graphs/knowledge.omni'))) { throw 'apply did not create the graph root' }
    Unlock-Root $root

    Invoke-Og 'cluster plan (converged)' cluster plan --target './cluster data' --config $config --json | Out-Null
    Invoke-Og 'load cluster graph' load --data seed.jsonl --mode merge --target $root --graph knowledge | Out-Null
    Unlock-Root $root
    Invoke-Og 'branch on cluster graph' branch create --from main review --target $root --graph knowledge | Out-Null
    Unlock-Root $root
    $clustered = Invoke-Og 'query cluster graph' query people --query queries.gq --format json --target $root --graph knowledge
    if ($clustered -notmatch 'grace') { throw 'the cluster graph row is missing' }
    Unlock-Root $root

    # ---- server booted from the local root ------------------------------------
    $rootUri = 'file:///' + ($root -replace '\\', '/' -replace ' ', '%20')
    $log = Join-Path $work 'server.log'
    $process = Start-Process -FilePath $server -PassThru -NoNewWindow `
        -ArgumentList @('--cluster', $rootUri, '--bind', "127.0.0.1:$Port", '--unauthenticated') `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    try {
        $ready = $null
        foreach ($attempt in 1..60) {
            if ($process.HasExited) { break }
            try {
                $ready = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/readyz" -TimeoutSec 2
                if ($ready.ready) { break }
            } catch { }
            Start-Sleep -Seconds 1
        }
        Write-Host '::group::server log'
        Get-Content $log, "$log.err" -ErrorAction SilentlyContinue | ForEach-Object { Write-Host $_ }
        Write-Host '::endgroup::'
        if (-not $ready -or -not $ready.ready) { throw 'the server did not become ready' }
        Write-Host "readyz: $($ready | ConvertTo-Json -Compress)"
        if ($ready.ready_graph_count -lt 1) { throw 'the server serves no ready graph' }
    } finally {
        if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    }
    Write-Host 'windows smoke: ok'
} finally {
    Pop-Location
}
