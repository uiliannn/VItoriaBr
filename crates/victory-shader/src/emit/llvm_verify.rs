//! Tests: every module a test emits goes through LLVM's verifier.
//!
//! DXIL is LLVM bitcode, so `opt -passes=verify` catches a broken module
//! (an instruction that does not dominate its uses, a mistyped operand, a
//! block without a terminator) on any platform, before dxil.dll would. It
//! runs when `VICTORY_LLVM_OPT` names an `opt` binary (`opt`, or a path),
//! and is off otherwise.
//!
//! The module's own data layout predates the rule that `i8` is 8-bit
//! aligned, and current LLVM refuses to read it without an override:
//! [`DATA_LAYOUT`] is DXIL's layout written the way current LLVM spells
//! it. An `opt` that cannot read the module at all (too old, or
//! something else on PATH) is reported once and no longer asked; only
//! LLVM's own "input module is broken" fails the test.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// DXIL's data layout (`dxil.dll`'s), with `i8` aligned to 8 bits.
const DATA_LAYOUT: &str =
    "e-m:e-p:32:32-i1:32-i8:8-i16:16-i32:32-i64:64-f16:16-f32:32-f64:64-n8:16:32:64";

/// Set once `opt` failed to read a module for a reason of its own.
static UNUSABLE: AtomicBool = AtomicBool::new(false);

/// Panics if LLVM's verifier rejects `bitcode` (a module just written),
/// keeping the module in the temporary directory for `llvm-dis`.
pub(super) fn check(bitcode: &[u8]) {
    let Err(err) = verify(bitcode) else {
        return;
    };
    let path = std::env::temp_dir().join(format!("victory-broken-{}.bc", std::process::id()));
    let kept = std::fs::write(&path, bitcode).map_or_else(
        |e| format!("(not kept: {e})"),
        |()| format!("kept at {}", path.display()),
    );
    panic!("LLVM's verifier rejects an emitted module, {kept}:\n{err}");
}

/// LLVM's complaint if it reads `bitcode` and finds it broken. `Ok` if
/// it is valid, or if no usable `opt` is configured.
fn verify(bitcode: &[u8]) -> Result<(), String> {
    let Some(opt) = std::env::var_os("VICTORY_LLVM_OPT").filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    if UNUSABLE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut child = Command::new(&opt)
        .arg(format!("-data-layout={DATA_LAYOUT}"))
        .args(["-passes=verify", "-disable-output", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("VICTORY_LLVM_OPT={}: {e}", opt.display()));
    let mut stdin = child.stdin.take().expect("piped stdin");
    // A writer of its own: opt may report before it has read everything.
    let out = std::thread::scope(|s| {
        s.spawn(move || {
            let _ = stdin.write_all(bitcode);
        });
        child.wait_with_output().expect("opt runs")
    });
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    if err.contains("input module is broken") {
        return Err(err);
    }
    if !UNUSABLE.swap(true, Ordering::Relaxed) {
        eprintln!(
            "VICTORY_LLVM_OPT={} cannot read the emitted DXIL, so modules are not verified:\n{err}",
            opt.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::bitcode::{Bin, Module, Term, pred};

    /// A diamond whose join adds 1 to `x`, made in one arm: through a phi
    /// (valid), or directly (`x` does not dominate the join).
    fn diamond(broken: bool) -> Vec<u8> {
        let mut m = Module::default();
        let i32t = m.int(32);
        let void = m.void();
        let main_ty = m.func_ty(void, vec![]);
        m.function("main", main_ty, None, true);
        let (entry, then, other, join) =
            (m.new_block(), m.new_block(), m.new_block(), m.new_block());
        let (one, two) = (m.i32(1), m.i32(2));
        let c = m.cmp(entry, pred::EQ, one, two);
        m.terminate(entry, Term::CondBr(c, then, other));
        let x = m.bin(then, Bin::Add, one, two);
        m.terminate(then, Term::Br(join));
        m.terminate(other, Term::Br(join));
        let v = if broken {
            x
        } else {
            let p = m.phi(join, i32t);
            m.set_phi(p, vec![(x, then), (one, other)]);
            p
        };
        m.bin(join, Bin::Add, v, one);
        m.terminate(join, Term::Ret);
        m.write()
    }

    #[test]
    fn the_verifier_rejects_a_use_its_definition_does_not_dominate() {
        if std::env::var_os("VICTORY_LLVM_OPT").is_none_or(|v| v.is_empty()) {
            eprintln!("VICTORY_LLVM_OPT is not set; the verifier is not run");
            return;
        }
        assert_eq!(super::verify(&diamond(false)), Ok(()));
        if super::UNUSABLE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("VICTORY_LLVM_OPT cannot read DXIL; the verifier is not run");
            return;
        }
        let why = super::verify(&diamond(true)).expect_err("a broken module passes");
        assert!(why.contains("does not dominate all uses"), "{why}");
    }
}
