$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$conversationPaths = @(
    (Join-Path $root "conversation.json"),
    (Join-Path $root "agent\conversation.json")
)

foreach ($path in $conversationPaths) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "missing artifact: $path"
    }
    $size = (Get-Item -LiteralPath $path).Length
    if ($size -gt 128 * 1024) {
        throw "conversation material exceeds 128 KiB: $path"
    }
    Get-Content -Raw -LiteralPath $path | ConvertFrom-Json | Out-Null
}

$dockerfile = Get-Content -Raw -LiteralPath (Join-Path $root "Dockerfile")
if ($dockerfile -notmatch "cargo build --release --locked") {
    throw "Dockerfile must build from source with locked dependencies"
}

Write-Output "artifact checks passed"
