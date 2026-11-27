# Local test reports

Use this PowerShell 7.4+ script from the Cargo workspace root for a selected
libtest target. Git is optional for recording the checkout. Cargo, Rust, and
the target's usual build prerequisites are needed to run tests. Choose tests
whose input requirements you have reviewed.

```powershell
pwsh -NoProfile -File scripts/test-report.ps1 -Package victory-core -Filter config::tests::example_config_parses -Exact -TrustHarness -OutputDirectory 'out/core example'
pwsh -NoProfile -File scripts/test-report.tests.ps1
pwsh -NoProfile -File scripts/test-report.integration.tests.ps1
```

The default selects the package's library tests. Use `-TestTarget NAME` for
one integration target, `-Profile NAME` for a Cargo profile, and `-Offline`
when the required dependencies are cached. Cargo uses the lockfile and two
build jobs by default (`-Jobs` overrides the latter). `-Filter` matches a
substring unless you add `-Exact`. Omitting the filter selects the full
target. Use a new output directory under the ignored `out/` directory or
outside the checkout.

The integration checks create a dependency-free synthetic Cargo crate in a
new temporary directory, build it offline, and exercise success, failure,
empty selection and both process and output-capture timeouts. They require
a working Rust toolchain and retain their logs in that directory.

The script writes `report.txt`, `report.json` (schema version 1), and the
original byte streams in `stdout.log` and `stderr.log`. Log paths in the
reports are relative to the report directory. The reports omit machine and
environment inventories and reduce recognized skip messages to reason codes.
`test_settings` records whether WARP was requested, whether
`VICTORY_DIFF_ONLY` is present (even if empty), and whether a console
compiler path is configured. It does not reveal compiler paths or case filter
values. These are configuration hints, not proof of device or compiler use.
The raw logs may contain absolute paths or other test output. Review them
before sharing. The wrapper returns Cargo's exit code when available, except on a wrapper
timeout (exit 124). JSON preserves Cargo's actual `exit_code` separately
from `wrapper_exit_code` and reports `timed_out` and `capture_complete`.

## Reading the result

The run outcome and each test's outcome are independent. A completed run can
include any of the outcomes below. `selected` includes framework ignores.
`executed` counts completed test bodies, including early returns, when the
harness totals are available. A truncated or interrupted run may have only
`completed_test_bodies`, the count of visible completions. Use `counts.passed`
for the number of approvals under the selected evidence policy.

| Test outcome | Evidence |
| --- | --- |
| `passed` | The named libtest result is `ok`, the summary and process exit agree, and there are no unresolved skip signals for the test. Requires `-TrustHarness`. |
| `failed` | The named result is `FAILED`, including a strict-mode panic after a missing prerequisite. |
| `ignored` | libtest reported `ignored`. |
| `unavailable` | An audited shader differential test returned before its comparisons because a required compiler or device was unavailable. |
| `partial` | A skip was recorded and there is evidence of other checks. The audited waterfall tests assert properties during build, before optional DXIL validation and GPU execution. The compute differential test can log a completed case alongside a WARP refusal. |
| `inconclusive` | The skip's scope is unknown, the run did not yield a consistent summary, the skip cannot be attributed, or functional coverage was not confirmed. |

Use `-TrustHarness` for a reviewed selection whose successful return is
sufficient evidence that the intended assertions ran. Review internal case
filters (including `VICTORY_DIFF_ONLY`) as well as the libtest selection;
record relevant filter values separately for reproduction. The default keeps
`ok` visible as the libtest result and treats it as inconclusive for
functional coverage. A zero-test selection is `no_tests_selected`. An
observed crash or a wrapper timeout is `interrupted`. Use
`-TimeoutSeconds N` to bound the wait for Cargo and its output streams.
On timeout, the runner attempts to stop the still-running Cargo process tree,
cancels output capture, and allows up to one second of asynchronous cleanup.
A descendant whose parent has already exited may continue running. Logs may
be incomplete; if capture is still active, the report does not parse them.
A timeout is always interrupted even if Cargo already returned zero. External termination of PowerShell itself may leave only
the raw logs.

## Capture and limits

The script forces libtest capture, `--show-output`, pretty format, and one
test thread. Rust output captured by libtest is attributed using named
output sections. Output printed directly to stderr or outside a named
section stays unassigned. An unassigned skip makes successful tests
inconclusive. With `--nocapture`, concurrent messages can interleave and
their position next to a test name is insufficient for attribution.
[The rustc book](https://doc.rust-lang.org/rustc/tests/index.html)

Adapter names are evidence of announcements. The shader helper emits the
name once per adapter per process, so its absence from another test's output
leaves that test's adapter unknown. The known early returns, partial paths,
and legacy messages are a limited set. Uninstrumented early returns and
helper-thread output can escape classification. This parser expects one
ordinary pretty-format libtest run and omits doc tests, custom harnesses,
workspace-wide runs, and arbitrary Cargo flags. Reproduction can still
require recording relevant opt-in settings separately from the report.
[Issue #32](https://github.com/uiliannn/VItoriaBr/issues/32)
[PR #43](https://github.com/uiliannn/VItoriaBr/pull/43)
