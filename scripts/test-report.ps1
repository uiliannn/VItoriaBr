#Requires -Version 7.4
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidatePattern('^[A-Za-z0-9_][A-Za-z0-9_.-]*$')] [string] $Package,
    [Parameter(Mandatory)] [string] $OutputDirectory,
    [ValidatePattern('^[A-Za-z0-9_][A-Za-z0-9_.-]*$')] [string] $TestTarget,
    [ValidatePattern('^[\p{L}\p{N}_][\p{L}\p{N}_:.\-]*$')] [string] $Filter,
    [switch] $Exact,
    [ValidatePattern('^[A-Za-z0-9_][A-Za-z0-9_-]*$')] [string] $Profile = 'test',
    [ValidateRange(1, 1024)] [int] $Jobs = 2,
    [ValidateRange(1, 2147483)] [int] $TimeoutSeconds,
    [switch] $Offline,
    [switch] $TrustHarness
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'test-report-lib.ps1')
if ($Exact -and -not $Filter) { throw '-Exact requires -Filter.' }

function Get-ToolOutput([string] $Executable, [string[]] $Arguments) {
    if (-not (Get-Command $Executable -CommandType Application -ErrorAction SilentlyContinue)) { return }
    $PSNativeCommandUseErrorActionPreference = $false
    $lines = & $Executable @Arguments 2>$null
    if ($LASTEXITCODE -eq 0) { $lines }
}

$commit = Get-ToolOutput git @('rev-parse', '--verify', 'HEAD')
$checkout = if ($commit -cmatch '^[0-9a-f]{40,64}$') {
    $PSNativeCommandUseErrorActionPreference = $false
    $changes = & git --no-optional-locks status --porcelain=v1 --untracked-files=normal 2>$null
    if ($LASTEXITCODE -eq 0) {
        if ($changes) { 'modified' } else { 'clean' }
    }
} else { $commit = $null }
$rust = Get-ToolOutput rustc @('--version')
$cargo = Get-ToolOutput cargo @('--version')
$strict = $env:VICTORY_STRICT_TESTS -ceq '1'
# Record only the presence of case filters and compiler paths, not their values.
# A caller using -TrustHarness must review the internal selection too.
$testSettings = [ordered]@{
    warp_requested = $env:VICTORY_WARP -ceq '1'
    diff_only_set = $null -ne [Environment]::GetEnvironmentVariable('VICTORY_DIFF_ONLY')
    console_dxc_configured = -not [string]::IsNullOrEmpty($env:VICTORY_CONSOLE_DXC)
}

$cargoArgs = @('test', '--color', 'never', '--locked', '--jobs', [string] $Jobs, '--package', $Package)
if ($TestTarget) { $cargoArgs += @('--test', $TestTarget) } else { $cargoArgs += '--lib' }
if ($Profile -ne 'test') { $cargoArgs += @('--profile', $Profile) }
if ($Offline) { $cargoArgs += '--offline' }
if ($Filter) { $cargoArgs += $Filter }
$cargoArgs += @('--', '--format', 'pretty', '--show-output', '--test-threads', '1')
if ($Exact) { $cargoArgs += '--exact' }

$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
if (Test-Path -LiteralPath $output) { throw 'Choose a new report directory.' }
[void] [System.IO.Directory]::CreateDirectory($output)
$stdoutPath = Join-Path $output 'stdout.log'
$stderrPath = Join-Path $output 'stderr.log'
$reportPath = Join-Path $output 'report.json'

$process = [System.Diagnostics.Process]::new()
$process.StartInfo.FileName = 'cargo'
$process.StartInfo.WorkingDirectory = (Get-Location).ProviderPath
$process.StartInfo.UseShellExecute = $false
$process.StartInfo.RedirectStandardOutput = $true
$process.StartInfo.RedirectStandardError = $true
$process.StartInfo.Environment['CARGO_TERM_PROGRESS_WHEN'] = 'never'
$process.StartInfo.Environment['RUST_TEST_NOCAPTURE'] = '0'
foreach ($argument in $cargoArgs) { $process.StartInfo.ArgumentList.Add($argument) }

$exitCode = $null
$interrupted = $false
$timedOut = $false
$captureComplete = $false
$capturePending = $false
$cleanupWarning = $null
$captureCancellation = [System.Threading.CancellationTokenSource]::new()
$launchFailed = $false
$stdoutFile = $null
$stderrFile = $null
$clock = [System.Diagnostics.Stopwatch]::StartNew()
try {
    $stdoutFile = [System.IO.File]::Open($stdoutPath, 'CreateNew', 'Write', 'Read')
    $stderrFile = [System.IO.File]::Open($stderrPath, 'CreateNew', 'Write', 'Read')
    try { [void] $process.Start() }
    catch { $launchFailed = $true }
    if (-not $launchFailed) {
        $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdoutFile, $captureCancellation.Token)
        $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderrFile, $captureCancellation.Token)
        $completion = [System.Threading.Tasks.Task]::WhenAll([System.Threading.Tasks.Task[]] @(
            $process.WaitForExitAsync(), $stdoutCopy, $stderrCopy
        ))
        try {
            if ($TimeoutSeconds) {
                $remaining = [math]::Max(0, $TimeoutSeconds * 1000.0 - $clock.Elapsed.TotalMilliseconds)
                [void] $completion.WaitAsync([timespan]::FromMilliseconds($remaining)).GetAwaiter().GetResult()
            } else {
                [void] $completion.GetAwaiter().GetResult()
            }
            $captureComplete = $true
        } catch [System.TimeoutException] {
            $timedOut = $true
            $interrupted = $true
            # The Cargo process may already have exited while a descendant
            # still holds the inherited output handles.
            try {
                if (-not $process.HasExited) { $process.Kill($true) }
            } catch {
                if (-not $process.HasExited) { $cleanupWarning = $_.Exception.Message }
            }
            [void] $captureCancellation.CancelAsync()
            try {
                [void] $completion.WaitAsync([timespan]::FromSeconds(1)).GetAwaiter().GetResult()
            } catch {
                # Cancellation is expected. Do not wait indefinitely for I/O.
            }
            $captureComplete = $stdoutCopy.IsCompletedSuccessfully -and $stderrCopy.IsCompletedSuccessfully
            $capturePending = -not ($stdoutCopy.IsCompleted -and $stderrCopy.IsCompleted)
            if ($capturePending -or -not $process.HasExited) {
                $cleanupWarning = 'Process or output capture did not finish within cleanup grace.'
            }
        }
        if ($process.HasExited) { $exitCode = $process.ExitCode }
    }
} finally {
    # Do not synchronously dispose streams that still have pending writes.
    # The script exits below, releasing any remaining local handles.
    if (-not $capturePending) {
        if ($null -ne $stdoutFile) { $stdoutFile.Dispose() }
        if ($null -ne $stderrFile) { $stderrFile.Dispose() }
        $process.Dispose()
    }
    if (-not $timedOut) { $captureCancellation.Dispose() }
    $clock.Stop()
}

# If a capture task is still active, its log is not a stable snapshot.
$stdoutText = if ($capturePending) { '' } else { [System.IO.File]::ReadAllText($stdoutPath, [text.encoding]::UTF8) }
$stderrText = if ($capturePending) { '' } else { [System.IO.File]::ReadAllText($stderrPath, [text.encoding]::UTF8) }
$result = Read-TestReport $stdoutText $stderrText $exitCode $interrupted `
    $TrustHarness ($Package -ceq 'victory-shader' -and -not $TestTarget)
if ($launchFailed) { $result.outcome = 'process_not_started' }
$wrapperExitCode = if ($timedOut) { 124 } elseif ($null -eq $exitCode) { 1 } else { $exitCode }
$report = [ordered]@{
    schema_version = 1
    commit = $commit
    checkout = $checkout
    rust = $rust
    cargo = $cargo
    profile = $Profile
    command = @('cargo') + $cargoArgs
    strict_tests = $strict
    test_settings = $testSettings
    trust_harness = [bool] $TrustHarness
    timeout_seconds = if ($TimeoutSeconds) { $TimeoutSeconds } else { $null }
    duration_seconds = [math]::Round($clock.Elapsed.TotalSeconds, 3)
    exit_code = $exitCode
    wrapper_exit_code = $wrapperExitCode
    timed_out = $timedOut
    capture_complete = $captureComplete
    cleanup_warning = $cleanupWarning
    logs = [ordered]@{ stdout = 'stdout.log'; stderr = 'stderr.log' }
    result = $result
}
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $reportPath -Encoding utf8
Format-TestReport $report | Set-Content -LiteralPath (Join-Path $output 'report.txt') -Encoding utf8
Write-Output (Format-TestReport $report)
exit $wrapperExitCode
