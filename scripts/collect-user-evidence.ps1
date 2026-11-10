param(
    [string]$OutputPath
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    $timestamp = Get-Date
    $commit = (git -c safe.directory="$repoRoot" rev-parse HEAD).Trim()
    if (-not $OutputPath) {
        $OutputPath = Join-Path $repoRoot (
            "student-evidence/user-run-{0}.md" -f $timestamp.ToString("yyyyMMdd-HHmmss")
        )
    }
    $outputDirectory = Split-Path -Parent $OutputPath
    New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null

    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add("# MiniVectorDB personal evidence run")
    $lines.Add("")
    $lines.Add("- Run time: $($timestamp.ToString("yyyy-MM-dd HH:mm:ss zzz"))")
    $lines.Add("- Commit: ``$commit``")
    $lines.Add("- OS: $([System.Environment]::OSVersion.VersionString)")
    $lines.Add("- PowerShell: $($PSVersionTable.PSVersion)")
    $lines.Add("")
    $lines.Add("This transcript records commands actually run by the student and the student's own answers.")
    $lines.Add("")

    function Read-Answer {
        param([string]$Prompt)
        do {
            $answer = Read-Host $Prompt
        } while ([string]::IsNullOrWhiteSpace($answer))
        return $answer.Trim()
    }

    function Run-Test {
        param(
            [string]$Title,
            [string]$TestArgs
        )
        Write-Host ""
        Write-Host "Running: $Title" -ForegroundColor Cyan
        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = "cargo"
        $startInfo.Arguments = "test --offline $TestArgs"
        $startInfo.WorkingDirectory = $repoRoot
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $process = [System.Diagnostics.Process]::Start($startInfo)
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $exitCode = $process.ExitCode
        $commandOutput = (($stdoutTask.Result + $stderrTask.Result).TrimEnd())
        $lines.Add("## $Title")
        $lines.Add("")
        $lines.Add('```text')
        $lines.Add("cargo test --offline $TestArgs")
        $lines.Add($commandOutput)
        $lines.Add("exit_code=$exitCode")
        $lines.Add('```')
        $lines.Add("")
        return $exitCode
    }

    $lines.Add("## Prediction before testing")
    $lines.Add("")
    $prediction = Read-Answer "1/5 Prediction: INSERT 1 auto-committed, BEGIN then INSERT 2, then shutdown or force-kill. Which ids should be visible after restart?"
    $lines.Add("**Student answer:** $prediction")
    $lines.Add("")

    $exit1 = Run-Test "JSONL protocol" "--test jsonl_protocol default_protocol_emits_ready_then_executes_ascii_sql -- --nocapture"
    $exit2 = Run-Test "Shutdown rollback" "--test jsonl_protocol shutdown_rolls_back_uncommitted_changes_and_reopens_same_data -- --nocapture"
    $exit3 = Run-Test "Forced-kill recovery" "--test jsonl_protocol forced_kill_recovers_committed_jsonl_writes_and_drops_the_open_transaction -- --nocapture"

    $lines.Add("## Explanation after testing")
    $lines.Add("")
    $transactionExplanation = Read-Answer "2/5 From the output, explain why force-kill recovery cannot rely on saving during process exit."
    $lines.Add("**Student answer:** $transactionExplanation")
    $lines.Add("")

    $indexPrediction = Read-Answer "3/5 Predict how many rows WHERE category='same' returns after INSERT id 1, DELETE id 1, and INSERT id 1 again."
    $lines.Add("**Student prediction:** $indexPrediction")
    $lines.Add("")
    $exit4 = Run-Test "Delete/reinsert index regression" "--test sql_integration deleting_then_reinserting_same_primary_key_does_not_duplicate_index_matches -- --nocapture"
    $indexExplanation = Read-Answer "4/5 Using the test name and output, explain why the primary rows and auxiliary index must be updated together."
    $lines.Add("**Student answer:** $indexExplanation")
    $lines.Add("")

    $limits = Read-Answer "5/5 Your judgment: why do passing public cases still not guarantee hidden cases and official performance scores?"
    $lines.Add("## Limits and tradeoffs")
    $lines.Add("")
    $lines.Add("**Student answer:** $limits")
    $lines.Add("")

    $lines.Add("## Execution summary")
    $lines.Add("")
    $lines.Add("- JSONL protocol exit: $exit1")
    $lines.Add("- Shutdown rollback exit: $exit2")
    $lines.Add("- Forced-kill recovery exit: $exit3")
    $lines.Add("- Index regression exit: $exit4")
    $lines.Add("")
    $lines.Add("Generated from the student's actual terminal session; do not edit command output.")
    $lines.Add("")

    [System.IO.File]::WriteAllLines($OutputPath, $lines, [System.Text.UTF8Encoding]::new($false))
    Write-Host ""
    Write-Host "Evidence written to: $OutputPath" -ForegroundColor Green
    Write-Host "Send the complete file content back to the Agent."
} finally {
    Pop-Location
}
