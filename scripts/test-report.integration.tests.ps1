#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

function Assert-Equal($Expected, $Actual, [string] $Case) {
    if ($Expected -cne $Actual) {
        throw "${Case}: expected <$Expected>, got <$Actual>"
    }
}

$runner = Join-Path $PSScriptRoot 'test-report.ps1'
$pwsh = (Get-Process -Id $PID).Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("test report integration " + [guid]::NewGuid().ToString('N'))
[void] [System.IO.Directory]::CreateDirectory((Join-Path $root 'src'))
Write-Host "Integration evidence: $root"
@'
[package]
name = "test-report-fixture"
version = "0.1.0"
edition = "2021"
[workspace]
'@ | Set-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Encoding utf8
@'
#[test]
fn ordinary_pass() {
    assert_eq!(2 + 2, 4);
}
#[test]
fn ordinary_fail() {
    assert_eq!(2 + 2, 5);
}
#[test]
fn holds_output_after_exit() {
    use std::process::{Command, Stdio};
    let _child = Command::new(std::env::var_os("TEST_REPORT_FIXTURE_PWSH").unwrap())
        .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 20"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn output holder");
}
#[test]
fn slow_body() {
    std::thread::sleep(std::time::Duration::from_secs(20));
}
'@ | Set-Content -LiteralPath (Join-Path $root 'src/lib.rs') -Encoding utf8

function Invoke-Fixture([string] $Name, [string] $Filter, [int] $ExpectedExit, [int] $Timeout = 0) {
    $output = Join-Path $root $Name
    $arguments = @('-NoProfile', '-File', $runner, '-Package', 'test-report-fixture',
        '-OutputDirectory', $output, '-Filter', $Filter, '-Exact', '-Offline', '-TrustHarness')
    if ($Timeout) { $arguments += @('-TimeoutSeconds', [string] $Timeout) }
    & $pwsh @arguments | Out-Host
    Assert-Equal $ExpectedExit $LASTEXITCODE "$Name wrapper exit"
    $report = Get-Content -LiteralPath (Join-Path $output 'report.json') -Raw | ConvertFrom-Json
    Assert-Equal $ExpectedExit $report.wrapper_exit_code "$Name reported wrapper exit"
    foreach ($log in 'stdout.log', 'stderr.log', 'report.txt') {
        Assert-Equal $true (Test-Path -LiteralPath (Join-Path $output $log) -PathType Leaf) "$Name $log exists"
    }
    $report
}

$env:TEST_REPORT_FIXTURE_PWSH = $pwsh
# Use an isolated target and ensure that prebuild and wrapper select one toolchain.
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
$env:RUST_TEST_NOCAPTURE = '1'
$env:VICTORY_DIFF_ONLY = 'synthetic-case'
$env:VICTORY_WARP = '1'
if (-not $env:RUSTUP_TOOLCHAIN -and (Get-Command rustup -CommandType Application -ErrorAction SilentlyContinue)) {
    $active = & rustup show active-toolchain
    Assert-Equal 0 $LASTEXITCODE 'Resolve active Rust toolchain'
    $env:RUSTUP_TOOLCHAIN = ($active -split '\s+')[0]
}
Push-Location $root
try {
    # Preserve the caller's selected Rust toolchain even outside the workspace.
    # RUSTUP_TOOLCHAIN, if already supplied by the caller, remains in effect.
    cargo generate-lockfile --offline
    Assert-Equal 0 $LASTEXITCODE 'Generate fixture lockfile'
    cargo test --offline --locked --no-run -j 2
    Assert-Equal 0 $LASTEXITCODE 'Prebuild fixture'

    $pass = Invoke-Fixture 'pass' 'ordinary_pass' 0
    Assert-Equal 'completed' $pass.result.outcome 'Normal completion'
    Assert-Equal 1 $pass.result.counts.passed 'Normal approval'
    Assert-Equal 0 $pass.exit_code 'Normal Cargo code'
    Assert-Equal $true $pass.capture_complete 'Normal capture'
    Assert-Equal $false $pass.timed_out 'Normal deadline'
    Assert-Equal $true $pass.test_settings.diff_only_set 'Internal filter presence'
    Assert-Equal $true $pass.test_settings.warp_requested 'WARP setting'

    $fail = Invoke-Fixture 'fail' 'ordinary_fail' 101
    Assert-Equal 'failed' $fail.result.outcome 'Assertion failure'
    Assert-Equal 101 $fail.exit_code 'Failure Cargo code'
    Assert-Equal 1 $fail.result.counts.failed 'Failure visible'
    Assert-Equal $true $fail.capture_complete 'Failure capture'

    $empty = Invoke-Fixture 'empty' 'nonexistent' 0
    Assert-Equal 'no_tests_selected' $empty.result.outcome 'Empty selection'
    Assert-Equal 0 $empty.result.selected 'Empty count'

    $slow = Invoke-Fixture 'slow' 'slow_body' 124 5
    Assert-Equal 'interrupted' $slow.result.outcome 'Running process deadline'
    Assert-Equal $true $slow.timed_out 'Running process timeout'
    Assert-Equal 0 $slow.result.counts.passed 'Running process not approved'

    # The child is deliberately short-lived; its parent and Cargo exit first.
    # The wrapper must not wait for the child's inherited pipe handles.
    $held = Invoke-Fixture 'held-output' 'holds_output_after_exit' 124 5
    Assert-Equal 'interrupted' $held.result.outcome 'Output capture deadline'
    Assert-Equal $true $held.timed_out 'Output capture timeout'
    Assert-Equal $false $held.capture_complete 'Output capture incomplete'
    Assert-Equal 0 $held.exit_code 'Actual Cargo success preserved'
    Assert-Equal 0 $held.result.counts.passed 'Interrupted capture not approved'
    if ($held.duration_seconds -ge 18) { throw 'Capture waited for the output holder.' }
} finally {
    Pop-Location
}
Write-Host 'All test-report integration assertions passed.'
