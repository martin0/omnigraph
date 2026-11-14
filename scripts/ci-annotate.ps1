# Run a command and, when it fails, repeat its errors as GitHub annotations.
#
# Annotations are readable on the check run without opening the log, which is
# what a contributor iterating from a fork needs.
#
#   pwsh scripts/ci-annotate.ps1 cargo test --locked -p omnigraph-storage
$ErrorActionPreference = 'Continue'
$PSNativeCommandUseErrorActionPreference = $false
$env:CARGO_TERM_COLOR = 'never'

$command = $args[0]
$rest = @($args | Select-Object -Skip 1)
$log = Join-Path ([System.IO.Path]::GetTempPath()) ("ci-annotate-" + [System.Guid]::NewGuid().ToString('N') + ".log")

& $command @rest 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath $log
$code = $LASTEXITCODE

function Write-Annotation {
    param([string[]]$Lines)
    $text = ($Lines -join "`n") -replace "`r", '' -replace '%', '%25' -replace "`n", '%0A'
    Write-Host "::error::$text"
}

if ($code -ne 0) {
    $lines = @(Get-Content $log)
    $starts = @(0..([Math]::Max($lines.Count - 1, 0)) | Where-Object {
            $lines[$_] -match '^(error(\[|:)|---- .* stdout ----|thread .* panicked|test result: FAILED)'
        } | Select-Object -First 8)
    foreach ($start in $starts) {
        Write-Annotation $lines[$start..([Math]::Min($start + 18, $lines.Count - 1))]
    }
    Write-Annotation (@("last lines of: $command $($rest -join ' ')") + @($lines | Select-Object -Last 30))
}
exit $code
