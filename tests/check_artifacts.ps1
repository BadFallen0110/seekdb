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
    $document = Get-Content -Raw -Encoding UTF8 -LiteralPath $path | ConvertFrom-Json
    $pointers = @($document.evidence_attribution.student_contributions) +
        @($document.evidence_attribution.agent_or_tool_only_results)
    foreach ($pointer in $pointers) {
        $node = $document
        foreach ($rawSegment in $pointer.TrimStart("/").Split("/")) {
            $segment = $rawSegment.Replace("~1", "/").Replace("~0", "~")
            if ($node -is [System.Array]) {
                $node = $node[[int]$segment]
            } else {
                $property = $node.PSObject.Properties[$segment]
                if ($null -eq $property) {
                    throw "unresolvable evidence pointer $pointer in $path"
                }
                $node = $property.Value
            }
        }
        if ($node -isnot [string]) {
            throw "evidence pointer does not resolve to a string: $pointer in $path"
        }
    }
}

$dockerfile = Get-Content -Raw -LiteralPath (Join-Path $root "Dockerfile")
if ($dockerfile -notmatch "cargo build --release --locked") {
    throw "Dockerfile must build from source with locked dependencies"
}

Write-Output "artifact checks passed"
