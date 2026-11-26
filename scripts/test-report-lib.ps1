function Get-SkipReason([string] $Line) {
    $marked = $Line -cmatch '^VICTORY TEST SKIPPED(?: \(VICTORY_STRICT_TESTS=1\))?: (.+)$'
    if ($marked) { $Line = $Matches[1] }
    switch -CaseSensitive -Regex ($Line) {
        '^no console or PC compiler; skipped$' { 'console-or-pc-compiler'; return }
        '^no console compiler; skipped$' { 'console-compiler'; return }
        '^no PC dxc\.exe; skipped$' { 'pc-dxc'; return }
        '^no dxil\.dll; (?:validation )?skipped$' { 'dxil-validator'; return }
        '^no SM 6\.6(?: device)?; skipped$' { 'sm66-device'; return }
        '^no SM 6\.6 hardware device(?:; (?:skipped|not run))?$' { 'sm66-hardware'; return }
        '^no SM 6\.6 (?:WARP adapter|warp device)(?:; skipped)?$' { 'sm66-warp'; return }
        '^no narrow-wave hardware device; not run$' { 'narrow-wave-hardware'; return }
        '^no host (?:mesh shader|depth bounds) support; skipped$' { 'gpu-feature'; return }
        '^.+: refused on WARP \(' { if ($marked) { 'warp-program-refused'; return } }
    }
    if ($marked -or $Line -cmatch '\b(?:skipped|not run)\b') { 'unspecified-skip' }
}

function Read-TestReport(
    [string] $Stdout, [string] $Stderr, [Nullable[int]] $ExitCode,
    [bool] $Interrupted, [bool] $TrustHarness, [bool] $ShaderLibrary
) {
    $tests = [System.Collections.Specialized.OrderedDictionary]::new([StringComparer]::Ordinal)
    $sections = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $unassigned = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $adapters = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $selected = $null
    $summary = $null
    $capture = $null
    $phase = 'before'
    $invalid = $false

    foreach ($line in ($Stdout -split '\r?\n')) {
        if ($line -cmatch '^running (\d+) tests?$') {
            if ($phase -ne 'before') { $invalid = $true; continue }
            $selected = [int] $Matches[1]
            $phase = 'results'
            continue
        }
        if ($line -cmatch '^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; 0 measured; (\d+) filtered out(?:; finished in [0-9.]+s)?$') {
            if ($phase -notin 'results', 'successes', 'failures') { $invalid = $true; continue }
            $summary = @{
                ok = $Matches[1] -ceq 'ok'
                passed = [int] $Matches[2]
                failed = [int] $Matches[3]
                ignored = [int] $Matches[4]
                filtered_out = [int] $Matches[5]
            }
            $phase = 'after'
            $capture = $null
            continue
        }
        if ($phase -eq 'results' -and $line -cmatch '^test (\S+) \.\.\. (ok|FAILED|ignored)(?:, .*)?$') {
            $name, $harness = $Matches[1], $Matches[2]
            if ($tests.Contains($name)) { $invalid = $true; continue }
            $tests.Add($name, [ordered]@{
                name = $name
                harness = $harness
                outcome = 'inconclusive'
                prerequisites = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
                adapters_reported = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
                comparison_logged = $false
            })
            continue
        }
        if ($phase -in 'results', 'successes', 'failures' -and $line -cmatch '^(successes|failures):$') {
            $phase = $Matches[1]
            $capture = $null
            continue
        }
        if ($phase -in 'successes', 'failures' -and $line -cmatch '^---- (\S+) stdout ----$') {
            $name = $Matches[1]
            $expected = if ($phase -eq 'successes') { 'ok' } else { 'FAILED' }
            $capture = $null
            if ($tests.Contains($name) -and $tests[$name].harness -ceq $expected -and $sections.Add($name)) {
                $capture = $tests[$name]
            } else {
                $invalid = $true
            }
            continue
        }
        $reason = Get-SkipReason $line
        if ($reason) {
            if ($null -eq $capture) { [void] $unassigned.Add($reason) }
            else { [void] $capture.prerequisites.Add($reason) }
        }
        if ($line -cmatch '^VICTORY HOST GPU: ([\p{L}\p{N} ._+,()\-]{1,128} \(vendor 0x[\da-fA-F]{4}\))$') {
            $adapter = $Matches[1]
            [void] $adapters.Add($adapter)
            if ($null -ne $capture) { [void] $capture.adapters_reported.Add($adapter) }
        }
        if ($null -ne $capture -and $line -cmatch '^.+: (equal|finished)$') {
            $capture.comparison_logged = $true
        }
    }

    foreach ($line in ($Stderr -split '\r?\n')) {
        $reason = Get-SkipReason $line
        if ($reason -and $line -cnotmatch '^\s*(?:(?:Compiling|Finished|Running) |warning:)') {
            [void] $unassigned.Add($reason)
        }
        if ($line -cmatch '^VICTORY HOST GPU: ([\p{L}\p{N} ._+,()\-]{1,128} \(vendor 0x[\da-fA-F]{4}\))$') {
            [void] $adapters.Add($Matches[1])
        }
        if ($line -cmatch 'process didn.t exit successfully: .*\((?:signal:|exit code: 0x[cC][0-9a-fA-F]{7}\b)') {
            $Interrupted = $true
        }
    }
    if ($null -ne $ExitCode -and ($ExitCode -lt 0 -or $ExitCode -eq 130)) { $Interrupted = $true }

    $harnessCounts = @{ ok = 0; FAILED = 0; ignored = 0 }
    foreach ($test in $tests.Values) { $harnessCounts[$test.harness]++ }
    $complete = -not $invalid -and $null -ne $summary -and $selected -eq $tests.Count -and
        $summary.passed -eq $harnessCounts.ok -and $summary.failed -eq $harnessCounts.FAILED -and
        $summary.ignored -eq $harnessCounts.ignored -and $summary.ok -eq ($harnessCounts.FAILED -eq 0)
    $consistent = $complete -and $null -ne $ExitCode -and -not $Interrupted -and
        (($ExitCode -eq 0) -eq $summary.ok)
    $counts = [ordered]@{ passed = 0; failed = 0; ignored = 0; unavailable = 0; partial = 0; inconclusive = 0 }
    $prerequisites = [System.Collections.Generic.HashSet[string]]::new($unassigned, [StringComparer]::Ordinal)
    foreach ($test in $tests.Values) {
        $name = $test.name
        $reasons = $test.prerequisites
        $prerequisites.UnionWith($reasons)
        if ($test.harness -ceq 'FAILED') { $test.outcome = 'failed' }
        elseif ($test.harness -ceq 'ignored') { $test.outcome = 'ignored' }
        elseif ($consistent -and $unassigned.Count -eq 0) {
            if ($reasons.Count -eq 0 -and $TrustHarness) {
                $test.outcome = 'passed'
            } elseif ($ShaderLibrary -and $reasons.Count -gt 0) {
                if ($name -cmatch '^emit::diff_tests::(?:translated_shaders_draw_as_pc_ones|translated_mesh_shaders_draw_as_pc_ones|float_semantics_are_the_consoles|translated_compute_matches_pc)$' -and
                    $reasons.Count -eq 1 -and @($reasons)[0] -cin 'console-compiler', 'console-or-pc-compiler', 'sm66-device') {
                    $test.outcome = 'unavailable'
                } elseif ($name.StartsWith('emit::waterfall_tests::', [StringComparison]::Ordinal) -or
                    ($name -ceq 'emit::diff_tests::translated_compute_matches_pc' -and
                     $reasons.Contains('warp-program-refused') -and $test.comparison_logged)) {
                    $test.outcome = 'partial'
                }
            }
        }
        $counts[$test.outcome]++
        if ($test.outcome -eq 'inconclusive') {
            $test.uncertainty = if (-not $consistent) { 'incomplete-or-inconsistent-run' }
                elseif ($unassigned.Count -gt 0) { 'unassigned-skip' }
                elseif ($reasons.Count -gt 0) { 'scope-of-skip-unknown' }
                else { 'functional-coverage-unconfirmed' }
        }
        $test.Remove('comparison_logged')
        $test.prerequisites = @($reasons | Sort-Object)
        $test.adapters_reported = @($test.adapters_reported | Sort-Object)
    }

    $outcome = if ($Interrupted) { 'interrupted' }
        elseif ($harnessCounts.FAILED -gt 0) { 'failed' }
        elseif ($ExitCode -ne 0 -and $null -ne $ExitCode) { 'process_failed' }
        elseif (-not $consistent) { 'inconclusive' }
        elseif ($selected -eq 0) { 'no_tests_selected' }
        else { 'completed' }
    [ordered]@{
        outcome = $outcome
        selected = $selected
        executed = if ($complete -and -not $Interrupted) { $harnessCounts.ok + $harnessCounts.FAILED } else { $null }
        completed_test_bodies = $harnessCounts.ok + $harnessCounts.FAILED
        harness_summary_consistent = $consistent
        filtered_out = if ($complete) { $summary.filtered_out } else { $null }
        counts = $counts
        prerequisites = @($prerequisites | Sort-Object)
        unassigned_prerequisites = @($unassigned | Sort-Object)
        adapters_reported = @($adapters | Sort-Object)
        tests = @($tests.Values)
    }
}

function Format-TestReport($Report) {
    $result = $Report.result
    "Run: $($result.outcome) (Cargo exit: $($Report.exit_code ?? 'unavailable'))"
    "Commit: $($Report.commit ?? 'unavailable') ($($Report.checkout ?? 'unknown'))"
    "Rust: $($Report.rust ?? 'unavailable')"
    "Cargo: $($Report.cargo ?? 'unavailable')"
    "Profile: $($Report.profile) | Duration: $($Report.duration_seconds)s | Strict tests: $($Report.strict_tests)"
    "Harness pass accepted for reviewed selection: $($Report.trust_harness)"
    if ($Report -is [System.Collections.IDictionary] -and $Report.Contains('wrapper_exit_code')) {
        "Wrapper exit: $($Report.wrapper_exit_code) | Timed out: $($Report.timed_out) | Capture complete: $($Report.capture_complete)"
        if ($Report.cleanup_warning) { "Cleanup warning: $($Report.cleanup_warning)" }
    }
    if ($Report -is [System.Collections.IDictionary] -and $Report.Contains('test_settings')) {
        "WARP requested: $($Report.test_settings.warp_requested) | Internal case filter set: $($Report.test_settings.diff_only_set) | Console compiler configured: $($Report.test_settings.console_dxc_configured)"
    }
    "Command: $($Report.command -join ' ')"
    "Selected: $($result.selected ?? 'unknown') | Executed: $($result.executed ?? 'unknown') | Completed bodies observed: $($result.completed_test_bodies)"
    'Counts: ' + (($result.counts.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ', ')
    'Prerequisites or limitations: ' + ($result.prerequisites -join ', ')
    'Unassigned prerequisites or limitations: ' + ($result.unassigned_prerequisites -join ', ')
    'Adapters reported: ' + ($result.adapters_reported -join ', ')
    "Original logs (relative to this report): $($Report.logs.stdout), $($Report.logs.stderr)"
    ''
    foreach ($test in $result.tests) {
        $details = @($test.prerequisites) + @($test['uncertainty'] | Where-Object { $_ })
        $line = "$($test.outcome)  $($test.name) [libtest: $($test.harness)]"
        if ($details.Count) { $line += " ($($details -join ', '))" }
        $line
    }
}
