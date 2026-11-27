#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'test-report-lib.ps1')

function Assert-Equal($Expected, $Actual, [string] $Case) {
    if ($Expected -cne $Actual) {
        throw "${Case}: expected <$Expected>, got <$Actual>"
    }
}

$pass = @'
running 1 test
test arithmetic::adds ... ok

successes:

successes:
    arithmetic::adds

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
'@
$report = Read-TestReport $pass '' 0 $false $true $false
Assert-Equal 1 $report.counts.passed 'Reviewed ordinary success'
Assert-Equal 1 $report.executed 'Executed test bodies'
$report = Read-TestReport $pass '' 0 $false $false $false
Assert-Equal 'functional-coverage-unconfirmed' $report.tests[0].uncertainty 'Default certainty'
Assert-Equal 0 $report.counts.passed 'Cargo success without affirmative evidence'

$mix = @'
running 6 tests
test arithmetic::adds ... ok
test emit::diff_tests::translated_compute_matches_pc ... ok
test emit::diff_tests::float_semantics_are_the_consoles ... ok
test emit::waterfall_tests::lanes_sample_the_texture_their_arm_loaded_or_picked ... ok
test arithmetic::manual ... ignored, manually scheduled
test arithmetic::wrong ... FAILED

successes:

---- emit::waterfall_tests::lanes_sample_the_texture_their_arm_loaded_or_picked stdout ----
VICTORY HOST GPU: Example Graphics (vendor 0x1234)
VICTORY TEST SKIPPED: no SM 6.6 warp device; skipped

---- emit::diff_tests::float_semantics_are_the_consoles stdout ----
VICTORY TEST SKIPPED: no console compiler; skipped

---- emit::diff_tests::translated_compute_matches_pc stdout ----
VICTORY TEST SKIPPED: case_1: refused on WARP (unsupported)
case_2: equal

successes:
    arithmetic::adds
    emit::diff_tests::translated_compute_matches_pc
    emit::diff_tests::float_semantics_are_the_consoles
    emit::waterfall_tests::lanes_sample_the_texture_their_arm_loaded_or_picked

failures:

---- arithmetic::wrong stdout ----
thread 'arithmetic::wrong' panicked at src/lib.rs:42:1:
assertion failed: left == right

failures:
    arithmetic::wrong

test result: FAILED. 4 passed; 1 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.00s
'@
$report = Read-TestReport $mix '' 101 $false $true $true
Assert-Equal 'failed' $report.outcome 'Failed overall'
Assert-Equal 1 $report.counts.passed 'Passed alongside failure'
Assert-Equal 1 $report.counts.failed 'Assertion failure'
Assert-Equal 1 $report.counts.ignored 'Framework ignore'
Assert-Equal 1 $report.counts.unavailable 'Full differential early return'
Assert-Equal 2 $report.counts.partial 'Only documented partial cases'
Assert-Equal 6 $report.selected 'Selected includes ignored'
Assert-Equal 5 $report.executed 'Executed includes early returns'
Assert-Equal 3 $report.filtered_out 'Filtered count'
Assert-Equal 'console-compiler' $report.tests[2].prerequisites[0] 'Capture heading overrides result order'
Assert-Equal 'Example Graphics (vendor 0x1234)' $report.tests[3].adapters_reported[0] 'Named adapter'
Assert-Equal 0 $report.tests[1].adapters_reported.Count 'Adapter does not propagate'
$strict = $mix.Replace('assertion failed: left == right', 'VICTORY TEST SKIPPED (VICTORY_STRICT_TESTS=1): no dxil.dll; skipped')
$report = Read-TestReport $strict '' 101 $false $true $true
Assert-Equal 1 $report.counts.failed 'Strict skip remains a failure'
Assert-Equal 'dxil-validator' $report.tests[5].prerequisites[0] 'Strict skip recorded on failed test'

$report = Read-TestReport $mix 'VICTORY TEST SKIPPED: no PC dxc.exe; skipped' 101 $false $true $true
Assert-Equal 'pc-dxc' $report.unassigned_prerequisites[0] 'Unscoped stderr'
Assert-Equal 4 $report.counts.inconclusive 'Unscoped diagnostic blocks all successes'
Assert-Equal 1 $report.counts.failed 'Unscoped diagnostics never mask failure'
$report = Read-TestReport $mix '' 101 $false $true $false
Assert-Equal 3 $report.counts.inconclusive 'Shader mappings apply only to shader library'
Assert-Equal 'scope-of-skip-unknown' $report.tests[3].uncertainty 'Unknown extent recorded'

$secret = 'VICTORY TEST SKIPPED: cannot access C:\private path\sample'
$report = Read-TestReport $pass $secret 0 $false $true $false
Assert-Equal 'unspecified-skip' $report.unassigned_prerequisites[0] 'Skip sanitized'
Assert-Equal $false (($report | ConvertTo-Json -Depth 8).Contains('private path')) 'Private path absent'
$report = Read-TestReport ("$secret`n" + $pass) '' 0 $false $true $false
Assert-Equal 0 $report.counts.passed 'Unscoped stdout blocks approval'

$zero = @'
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s
'@
$report = Read-TestReport $zero '' 0 $false $true $false
Assert-Equal 'no_tests_selected' $report.outcome 'Zero selection'
Assert-Equal 0 $report.selected 'Zero selection count'
Assert-Equal 0 $report.executed 'Zero executed'

$truncated = "running 2 tests`ntest arithmetic::adds ... ok`n"
$report = Read-TestReport $truncated '' 0 $false $true $false
Assert-Equal 'inconclusive' $report.outcome 'Truncated output with zero exit'
Assert-Equal 2 $report.selected 'Count at start'
Assert-Equal $null $report.executed 'Unknown completed count'
Assert-Equal 1 $report.completed_test_bodies 'One observed completion'
Assert-Equal 0 $report.counts.passed 'No approval on partial log'
$report = Read-TestReport $truncated "error: process didn't exit successfully: sample (signal: 9, SIGKILL)" 101 $false $true $false
Assert-Equal 'interrupted' $report.outcome 'Signal through Cargo'
$report = Read-TestReport $truncated "error: process didn't exit successfully: sample (exit code: 0xc0000005)" 101 $false $true $false
Assert-Equal 'interrupted' $report.outcome 'Windows exception through Cargo'
$report = Read-TestReport $truncated '' 1 $true $true $false
Assert-Equal 'interrupted' $report.outcome 'Wrapper timeout'
$report = Read-TestReport '' 'error: could not compile' 101 $false $true $false
Assert-Equal 'process_failed' $report.outcome 'Cargo compile failure'
$report = Read-TestReport '' '' $null $false $true $false
Assert-Equal 'inconclusive' $report.outcome 'Missing process'
$report = Read-TestReport ($pass + "`n" + $zero) '' 0 $false $true $false
Assert-Equal 'inconclusive' $report.outcome 'Unexpected second harness'
$report = Read-TestReport ($pass.Replace('1 passed', '2 passed')) '' 0 $false $true $false
Assert-Equal 0 $report.counts.passed 'Inconsistent footer'
$report = Read-TestReport ($pass.Replace('arithmetic::adds ... ok', 'arithmetic::adds ... FAILED')) '' 0 $false $true $false
Assert-Equal 1 $report.counts.failed 'Failure remains visible with inconsistent footer and exit code'
$report = Read-TestReport $pass '' 101 $false $true $false
Assert-Equal 0 $report.counts.passed 'Complete footer with conflicting exit code'

'All synthetic test-report assertions passed.'
