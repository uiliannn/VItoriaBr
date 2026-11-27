//! Runs translated shaders on the CPU, so what a lowered instruction
//! computes can be checked on any host, without a GPU or `dxil.dll`.
//!
//! The DXIL inside a container is LLVM bitcode. LLVM's own tools read it:
//! `opt` gives it the host's data layout and triple (DXIL's pointers are 32
//! bits), `llvm-link` adds [`RUNTIME`], an IR module that stands in for the
//! `dx.op` intrinsics, and `lli` runs it. The programs have the shape of
//! `valu_tests.rs`' (`build_valu_cs`): four root constants arrive in
//! `s0..s3`, are copied to `v1..v4`, the body leaves its result in `v0`, and
//! `v0` is stored to dword 0 of a raw UAV. So an expectation written here
//! holds on the GPU too, and the other way round.
//!
//! The tools are found on `PATH`, or in `VICTORY_LLVM_BIN` if set. Without
//! them, or on a host other than x86-64 Linux, every test here says it was
//! skipped (see [`super::test_skip`]).
//!
//! What this checks: the arithmetic of the translation. What it cannot: the
//! host compiler's and driver's own handling (denormal flushing, a
//! vendor's f16 rounding, wave operations across lanes, which are one lane
//! here). Those stay with `valu_tests.rs` on Windows.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::lower;
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;
const LAYOUT: &str =
    "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128";
const TRIPLE: &str = "x86_64-unknown-linux-gnu";

/// The LLVM tools, or why they are not usable here.
struct Tools {
    opt: PathBuf,
    link: PathBuf,
    lli: PathBuf,
}

fn tools() -> Result<&'static Tools, &'static str> {
    static TOOLS: std::sync::OnceLock<Result<Tools, &'static str>> = std::sync::OnceLock::new();
    TOOLS
        .get_or_init(|| {
            if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                return Err("CPU runs of translated shaders are set up for x86-64 Linux only");
            }
            // VICTORY_LLVM_BIN, else PATH, else the newest of Debian's and
            // Ubuntu's versioned installs (/usr/lib/llvm-N/bin).
            let mut dirs: Vec<Option<PathBuf>> = match std::env::var_os("VICTORY_LLVM_BIN") {
                Some(d) => vec![Some(PathBuf::from(d))],
                None => vec![None],
            };
            if dirs == [None] {
                let mut versioned: Vec<(u32, PathBuf)> = std::fs::read_dir("/usr/lib")
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let n = e
                            .file_name()
                            .to_str()?
                            .strip_prefix("llvm-")?
                            .parse()
                            .ok()?;
                        Some((n, e.path().join("bin")))
                    })
                    .collect();
                versioned.sort();
                dirs.extend(versioned.into_iter().rev().map(|(_, d)| Some(d)));
            }
            let works = |path: &Path| {
                Command::new(path)
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success())
            };
            dirs.iter()
                .find_map(|dir| {
                    let at = |name: &str| {
                        dir.as_ref()
                            .map_or_else(|| PathBuf::from(name), |d| d.join(name))
                    };
                    let t = Tools {
                        opt: at("opt"),
                        link: at("llvm-link"),
                        lli: at("lli"),
                    };
                    (works(&t.opt) && works(&t.link) && works(&t.lli)).then_some(t)
                })
                .ok_or("LLVM's opt, llvm-link and lli were not found (set VICTORY_LLVM_BIN)")
        })
        .as_ref()
        .map_err(|e| *e)
}

/// SOP1 `s_mov_b32 sdst, src0`.
const fn s_mov_b32(sdst: u8, src0: u8) -> u32 {
    0xBE80_0000 | (sdst as u32) << 16 | 0x03 << 8 | src0 as u32
}

/// VOP1 `op vdst, src0` (`src0`: 0..105 = SGPR, 256..511 = VGPR).
const fn vop1(op: u16, vdst: u8, src0: u16) -> u32 {
    0x7E00_0000 | (vdst as u32) << 17 | (op as u32) << 9 | (src0 as u32 & 0x1FF)
}

/// VOP2 `op vdst, src0, vsrc1`.
const fn vop2(op: u8, vdst: u8, src0: u16, vsrc1: u8) -> u32 {
    (op as u32 & 0x3F) << 25 | (vdst as u32) << 17 | (vsrc1 as u32) << 9 | (src0 as u32 & 0x1FF)
}

/// VOP3 `op vdst, src0, src1, src2` with `clamp` and `neg` (one bit per source).
const fn vop3(op: u16, vdst: u8, src: [u16; 3], neg: u8, clamp: bool) -> [u32; 2] {
    [
        0x35 << 26 | (op as u32 & 0x3FF) << 16 | (clamp as u32) << 15 | vdst as u32,
        (neg as u32 & 7) << 29
            | (src[0] as u32 & 0x1FF)
            | (src[1] as u32 & 0x1FF) << 9
            | (src[2] as u32 & 0x1FF) << 18,
    ]
}

/// VOP3P `op vdst, src0, src1, src2` (op_sel_hi on for every source).
const fn vop3p(op: u16, vdst: u8, src: [u16; 3], clamp: bool) -> [u32; 2] {
    [
        0x33 << 26 | (op as u32 & 0x7F) << 16 | (clamp as u32) << 15 | 1 << 14 | vdst as u32,
        (src[0] as u32 & 0x1FF)
            | (src[1] as u32 & 0x1FF) << 9
            | (src[2] as u32 & 0x1FF) << 18
            | 3 << 27,
    ]
}

const fn vgpr(n: u8) -> u16 {
    256 + n as u16
}

/// `v1`, `v2`, `v3`: the first three inputs.
const V123: [u16; 3] = [vgpr(1), vgpr(2), vgpr(3)];

/// The program around `body`, as `valu_tests.rs` builds it.
fn program(body: &[u32]) -> Vec<u32> {
    let mut words = vec![s_mov_b32(5, 0x80)];
    // s_load_dwordx4 s[8:11], s[4:5], 0: the UAV's descriptor.
    words.extend([0x3D << 26 | 0x02 << 18 | 8 << 6 | 2, 125 << 25]);
    for i in 0..4u8 {
        words.push(vop1(0x001, i + 1, u16::from(i))); // v_mov_b32 v(i+1), s(i)
    }
    words.extend_from_slice(body);
    // buffer_store_dword v0, off, s[8:11], 0
    words.extend([0x38 << 26 | 0x1C << 18, 0x80 << 24 | 2 << 16]);
    words.push(S_ENDPGM);
    words
}

/// The DXIL bitcode of `body` translated in [`program`].
fn translate(body: &[u32]) -> Result<Vec<u8>, String> {
    let code: Vec<u8> = program(body).iter().flat_map(|w| w.to_le_bytes()).collect();
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 8,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1,
            threads: [1, 1, 1],
        }),
        code,
    };
    let e = |e: crate::ShaderError| e.to_string();
    let p = ir::build(&native.code, false, native.vgprs).map_err(e)?;
    let st = structure::structure(&p).map_err(e)?;
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).map_err(e)?;
    let io = io::raise(&p, &native).map_err(e)?;
    let lowered = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    })
    .map_err(e)?;
    bitcode(&lowered.dxil)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "no DXIL part".into())
}

/// The bitcode in a container's DXIL part: after the 8-byte program header,
/// a bitcode header whose third dword is the bitcode's offset from it and
/// whose fourth is its size.
fn bitcode(container: &[u8]) -> Option<&[u8]> {
    let c = crate::dxbc::Container::parse(container).ok()?;
    let d = c.part(crate::dxbc::FourCC::DXIL)?.data;
    let word = |at: usize| Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?) as usize);
    let (offset, size) = (word(16)?, word(20)?);
    d.get(8 + offset..8 + offset + size)
}

/// Runs the bitcode with root constants `inputs`: each dword of the UAV
/// the shader stored, by index.
fn execute(tools: &Tools, dxil: &[u8], inputs: [u32; 4]) -> Result<Vec<(usize, u32)>, String> {
    static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("victory-cpu-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = execute_in(tools, &dir, dxil, inputs);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn execute_in(
    tools: &Tools,
    dir: &Path,
    dxil: &[u8],
    inputs: [u32; 4],
) -> Result<Vec<(usize, u32)>, String> {
    let file = |name: &str| dir.join(name);
    std::fs::write(file("shader.bc"), dxil).map_err(|e| e.to_string())?;
    let consts = inputs
        .iter()
        .chain(&[0; 4])
        .map(|x| format!("i32 {}", *x as i32))
        .collect::<Vec<_>>()
        .join(", ");
    let runtime = RUNTIME.replace(
        "@consts = global [8 x i32] zeroinitializer",
        &format!("@consts = global [8 x i32] [{consts}]"),
    );
    let runtime =
        format!("target datalayout = \"{LAYOUT}\"\ntarget triple = \"{TRIPLE}\"\n{runtime}");
    std::fs::write(file("runtime.ll"), runtime).map_err(|e| e.to_string())?;
    let run = |cmd: &mut Command| -> Result<String, String> {
        let out = cmd.output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            Ok(text)
        } else {
            Err(format!(
                "{cmd:?}: {}{text}{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    };
    let host = run(Command::new(&tools.opt)
        .arg(format!("-data-layout={LAYOUT}"))
        .arg(format!("-mtriple={TRIPLE}"))
        .arg("-S")
        .arg(file("shader.bc"))
        .arg("-o")
        .arg("-"))?;
    std::fs::write(file("host.ll"), saturating_conversions(&host)).map_err(|e| e.to_string())?;
    run(Command::new(&tools.link)
        .arg(file("host.ll"))
        .arg(file("runtime.ll"))
        .arg("-o")
        .arg(file("run.bc")))?;
    let text = run(Command::new(&tools.lli)
        .arg("-entry-function=run")
        .arg(file("run.bc")))?;
    let mut stored = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let (written, value) = line
            .split_once(' ')
            .ok_or_else(|| format!("output: {text}"))?;
        if written == "1" {
            stored.push((
                i,
                u32::from_str_radix(value, 16).map_err(|e| e.to_string())?,
            ));
        }
    }
    Ok(stored)
}

/// `text` (an LLVM module) with each float-to-integer conversion made a
/// saturating one. In LLVM IR an out-of-range `fptosi`/`fptoui` is poison
/// (x86 gives 0x80000000); in DXIL it has D3D's meaning, which the
/// translation relies on: the result saturates, and NaN gives 0. That is
/// what `llvm.fpto[su]i.sat` computes.
fn saturating_conversions(text: &str) -> String {
    fn mangle(ty: &str) -> Option<&str> {
        match ty {
            "half" => Some("f16"),
            "float" => Some("f32"),
            "double" => Some("f64"),
            t if t.starts_with('i') => Some(t),
            _ => None,
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut declared = std::collections::BTreeSet::new();
    for line in text.lines() {
        let rewritten = ["fptosi", "fptoui"].iter().find_map(|op| {
            let (lhs, rest) = line.split_once(&format!(" = {op} "))?;
            // `<from> <value> to <into>`, perhaps with metadata after.
            let (from, rest) = rest.split_once(' ')?;
            let (value, rest) = rest.split_once(" to ")?;
            let into = rest.split([',', ' ']).next()?;
            let name = format!("llvm.{op}.sat.{}.{}", mangle(into)?, mangle(from)?);
            declared.insert(format!("declare {into} @{name}({from})"));
            Some(format!("{lhs} = call {into} @{name}({from} {value})"))
        });
        out.push_str(rewritten.as_deref().unwrap_or(line));
        out.push('\n');
    }
    for d in declared {
        out.push_str(&d);
        out.push('\n');
    }
    out
}

/// `body` translated and run once per input set; `check(case, inputs, v0)`.
/// Says it was skipped when the tools are missing.
fn run_cases(body: &[u32], cases: &[[u32; 4]], mut check: impl FnMut(usize, [u32; 4], u32)) {
    let tools = match tools() {
        Ok(t) => t,
        Err(why) => return super::test_skip::skip(&format!("{why}; CPU run skipped")),
    };
    let dxil = translate(body).unwrap_or_else(|e| panic!("translation failed: {e}"));
    for (i, &inputs) in cases.iter().enumerate() {
        let stored = execute(tools, &dxil, inputs).unwrap_or_else(|e| panic!("case {i}: {e}"));
        let [(0, v0)] = stored[..] else {
            panic!("case {i}: the shader stored {stored:x?}, not one dword at 0");
        };
        check(i, inputs, v0);
    }
}

/// [`run_cases`] where each case is `[inputs, want]`.
fn expect(what: &str, body: &[u32], cases: &[([u32; 4], u32)]) {
    let inputs: Vec<[u32; 4]> = cases.iter().map(|c| c.0).collect();
    run_cases(body, &inputs, |i, inp, got| {
        let want = cases[i].1;
        assert_eq!(
            got, want,
            "{what} case {i} {inp:08x?}: got {got:#010x}, want {want:#010x}"
        );
    });
}

fn f(x: f32) -> u32 {
    x.to_bits()
}

#[test]
fn bitcode_is_found_in_the_container() {
    // A real container (through the whole translation) and two broken ones.
    let dxil = translate(&[vop2(0x03, 0, vgpr(1), 2)]).expect("translates");
    assert_eq!(&dxil[..4], b"BC\xC0\xDE");
    assert!(bitcode(b"DXBC").is_none());
    assert!(bitcode(&[]).is_none());
}

#[test]
fn float_to_integer_conversions_become_saturating() {
    let text =
        "  %5 = fptosi float %4 to i32\n  %6 = fptoui float %4 to i32, !dbg !1\n  ret void\n";
    assert_eq!(
        saturating_conversions(text),
        "  %5 = call i32 @llvm.fptosi.sat.i32.f32(float %4)\n\
         \x20 %6 = call i32 @llvm.fptoui.sat.i32.f32(float %4)\n\
         \x20 ret void\n\
         declare i32 @llvm.fptosi.sat.i32.f32(float)\n\
         declare i32 @llvm.fptoui.sat.i32.f32(float)\n"
    );
}

#[test]
fn float_add_and_multiply_compute_what_they_should() {
    expect(
        "v_add_f32",
        &[vop2(0x03, 0, vgpr(1), 2)],
        &[
            ([f(1.5), f(2.25), 0, 0], f(3.75)),
            ([f(-0.0), f(0.0), 0, 0], f(0.0)),
            ([f(f32::INFINITY), f(1.0), 0, 0], f(f32::INFINITY)),
        ],
    );
    expect(
        "v_mul_f32",
        &[vop2(0x08, 0, vgpr(1), 2)],
        &[
            ([f(1.5), f(-4.0), 0, 0], f(-6.0)),
            ([f(3.0), f(0.5), 0, 0], f(1.5)),
        ],
    );
}

#[test]
fn integer_operations_compute_what_they_should() {
    expect(
        "v_add_nc_u32",
        &[vop2(0x25, 0, vgpr(1), 2)],
        &[
            ([0xFFFF_FFFF, 2, 0, 0], 1),
            ([0x7FFF_FFFF, 1, 0, 0], 0x8000_0000),
        ],
    );
    expect(
        "v_lshlrev_b32",
        &[vop2(0x1A, 0, vgpr(1), 2)],
        &[
            // Only the shift's low five bits count.
            ([33, 1, 0, 0], 2),
            ([4, 0x1234_5678, 0, 0], 0x2345_6780),
        ],
    );
    expect(
        "v_ashrrev_i32",
        &[vop2(0x18, 0, vgpr(1), 2)],
        &[([4, 0x8000_0000, 0, 0], 0xF800_0000)],
    );
    expect(
        "v_mul_hi_u32",
        &vop3(0x16A, 0, V123, 0, false),
        &[
            ([0xFFFF_FFFF, 0xFFFF_FFFF, 0, 0], 0xFFFF_FFFE),
            ([0x1_0000, 0x1_0000, 0, 0], 1),
        ],
    );
    expect(
        "v_mul_hi_i32",
        &vop3(0x16C, 0, V123, 0, false),
        &[
            ([0xFFFF_FFFF, 0xFFFF_FFFF, 0, 0], 0),
            ([0x8000_0000, 2, 0, 0], 0xFFFF_FFFF),
        ],
    );
    expect(
        "v_bfe_u32",
        &vop3(0x148, 0, V123, 0, false),
        &[([0xABCD_1234, 8, 8, 0], 0x12)],
    );
    expect(
        "v_bfe_i32",
        &vop3(0x149, 0, V123, 0, false),
        &[([0xABCD_1234, 28, 4, 0], 0xFFFF_FFFA)],
    );
    expect(
        "v_med3_i32",
        &vop3(0x158, 0, V123, 0, false),
        &[
            ([5, 0xFFFF_FFFF, 3, 0], 3),
            ([0x8000_0000, 0x7FFF_FFFF, 0, 0], 0),
        ],
    );
    expect(
        "v_bfrev_b32",
        &[vop1(0x38, 0, vgpr(1))],
        &[([1, 0, 0, 0], 0x8000_0000)],
    );
}

#[test]
fn bit_scans_count_from_the_ends_rdna2_names() {
    // v_ffbh_u32 counts from the top bit, v_ffbl_b32 from the bottom, and
    // v_ffbh_i32 skips the copies of the sign bit; no bit gives -1.
    expect(
        "v_ffbh_u32",
        &[vop1(0x39, 0, vgpr(1))],
        &[
            ([0x0001_0000, 0, 0, 0], 15),
            ([0, 0, 0, 0], u32::MAX),
            ([0x8000_0000, 0, 0, 0], 0),
        ],
    );
    expect(
        "v_ffbl_b32",
        &[vop1(0x3A, 0, vgpr(1))],
        &[([0x0001_0000, 0, 0, 0], 16), ([0, 0, 0, 0], u32::MAX)],
    );
    expect(
        "v_ffbh_i32",
        &[vop1(0x3B, 0, vgpr(1))],
        &[
            ([0xFFFF_0000, 0, 0, 0], 16),
            ([0x0000_FFFF, 0, 0, 0], 16),
            ([u32::MAX, 0, 0, 0], u32::MAX),
            ([0, 0, 0, 0], u32::MAX),
        ],
    );
}

#[test]
fn float_conversions_round_as_rdna2_does() {
    expect(
        "v_cvt_i32_f32",
        &[vop1(0x08, 0, vgpr(1))],
        &[
            // Toward zero, saturating; NaN gives 0.
            ([f(-2.75), 0, 0, 0], 0xFFFF_FFFE),
            ([f(3e9), 0, 0, 0], 0x7FFF_FFFF),
            ([f(-3e9), 0, 0, 0], 0x8000_0000),
            ([f(f32::NAN), 0, 0, 0], 0),
        ],
    );
    expect(
        "v_cvt_u32_f32",
        &[vop1(0x07, 0, vgpr(1))],
        &[
            ([f(-1.0), 0, 0, 0], 0),
            ([f(5e9), 0, 0, 0], u32::MAX),
            ([f(7.9), 0, 0, 0], 7),
        ],
    );
    expect(
        "v_cvt_f32_ubyte1",
        &[vop1(0x12, 0, vgpr(1))],
        &[([0x0000_C800, 0, 0, 0], f(200.0))],
    );
    expect(
        "v_rndne_f32",
        &[vop1(0x23, 0, vgpr(1))],
        &[([f(2.5), 0, 0, 0], f(2.0)), ([f(3.5), 0, 0, 0], f(4.0))],
    );
}

#[test]
fn med3_of_floats_with_nan_matches_rdna2() {
    // valu_tests.rs' hardware cases: with one NaN, RDNA2 gives the smaller
    // of the other two; with two, the one left.
    let nan = f(f32::NAN);
    expect(
        "v_med3_f32",
        &vop3(0x157, 0, V123, 0, false),
        &[
            ([f(1.0), f(2.0), f(3.0), 0], f(2.0)),
            ([f(3.0), f(1.0), f(2.0), 0], f(2.0)),
            ([f(1.0), f(2.0), nan, 0], f(1.0)),
            ([nan, f(1.0), f(0.0), 0], f(0.0)),
            ([nan, f(0.0), f(1.0), 0], f(0.0)),
            ([f(2.0), nan, f(1.0), 0], f(1.0)),
            ([f(-3.0), nan, f(4.0), 0], f(-3.0)),
            ([nan, f(5.0), nan, 0], f(5.0)),
            ([nan, nan, f(-2.0), 0], f(-2.0)),
            ([f(7.0), nan, nan, 0], f(7.0)),
        ],
    );
}

#[test]
fn packed_dot_products_match_rdna2() {
    // The same cases as valu_tests.rs' GPU run (and #44's v_dot8_i32_i4).
    expect(
        "v_dot8_i32_i4",
        &vop3p(0x18, 0, V123, false),
        &[
            ([0xFFFF_FFFF, 0x7777_7777, 100, 0], 44),
            ([0x8888_8888, 0x8888_8888, 0, 0], 512),
        ],
    );
    expect(
        "v_dot8_u32_u4",
        &vop3p(0x19, 0, V123, false),
        &[([0xFFFF_FFFF, 0xFFFF_FFFF, 1, 0], 1801)],
    );
    expect(
        "v_dot4_u32_u8",
        &vop3p(0x17, 0, V123, false),
        &[([0x0102_0304, 0x0101_0101, 5, 0], 15)],
    );
    // Clamped: the sum saturates instead of wrapping.
    expect(
        "v_dot4_i32_i8 clamp",
        &vop3p(0x16, 0, V123, true),
        &[
            ([0x7F7F_7F7F, 0x7F7F_7F7F, 0x7FFF_FFFF, 0], 0x7FFF_FFFF),
            ([0x8080_8080, 0x7F7F_7F7F, 0x8000_0000, 0], 0x8000_0000),
        ],
    );
}

#[test]
fn half_floats_round_to_nearest_even() {
    // v_cvt_f16_f32: 1 + 2^-11 is halfway between two f16 values and rounds
    // to the even one, 1.0; 1 + 3 * 2^-11 rounds up.
    expect(
        "v_cvt_f16_f32",
        &[vop1(0x0A, 0, vgpr(1))],
        &[
            ([f(1.0 + 1.0 / 2048.0), 0, 0, 0], 0x3C00),
            ([f(1.0 + 3.0 / 2048.0), 0, 0, 0], 0x3C02),
            ([f(65520.0), 0, 0, 0], 0x7C00),
        ],
    );
    expect(
        "v_cvt_f32_f16",
        &[vop1(0x0B, 0, vgpr(1))],
        &[([0xC500, 0, 0, 0], f(-5.0))],
    );
    // v_add_f16 writes the low half and keeps the destination's high half
    // (v0 is 0 here).
    expect(
        "v_add_f16",
        &[vop2(0x32, 0, vgpr(1), 2)],
        &[([0x3C00, 0x4000, 0, 0], 0x4200)],
    );
}

#[test]
fn neg_and_clamp_modifiers_apply() {
    // v_add_f32 through VOP3 (opcode 0x103) with neg on src1, then clamped.
    expect(
        "v_add_f32 -src1",
        &vop3(0x103, 0, V123, 0b010, false),
        &[([f(1.0), f(3.0), 0, 0], f(-2.0))],
    );
    expect(
        "v_add_f32 clamp",
        &vop3(0x103, 0, V123, 0, true),
        &[
            ([f(1.0), f(3.0), 0, 0], f(1.0)),
            ([f(-1.0), f(0.5), 0, 0], f(0.0)),
        ],
    );
}

/// Stands in for the `dx.op` intrinsics a translated compute shader calls,
/// so `lli` can run it. The test fills `@consts` (row 0: s0..s3; row 1: the
/// descriptor table, slot 0) and reads what the shader stored from `@run`'s
/// output: one line per dword of the UAV, "<written> <value>". Each
/// intrinsic follows DXIL.rst: the opcode is the first argument, and floats
/// keep IEEE semantics.
const RUNTIME: &str = r#"
%dx.types.Handle = type { ptr }
%dx.types.ResBind = type { i32, i32, i32, i8 }
%dx.types.ResourceProperties = type { i32, i32 }
%dx.types.CBufRet.i32 = type { i32, i32, i32, i32 }
%dx.types.Dimensions = type { i32, i32, i32, i32 }

@consts = global [8 x i32] zeroinitializer
@out = global [16 x i32] zeroinitializer
@written = global [16 x i8] zeroinitializer
@fmt = private constant [9 x i8] c"%u %08x\0A\00"

declare i32 @printf(ptr, ...)
declare void @main()

declare float @llvm.fabs.f32(float)
declare float @llvm.cos.f32(float)
declare float @llvm.sin.f32(float)
declare float @llvm.exp2.f32(float)
declare float @llvm.log2.f32(float)
declare float @llvm.sqrt.f32(float)
declare float @llvm.roundeven.f32(float)
declare float @llvm.floor.f32(float)
declare float @llvm.ceil.f32(float)
declare float @llvm.trunc.f32(float)
declare float @llvm.maxnum.f32(float, float)
declare float @llvm.minnum.f32(float, float)
declare float @llvm.fma.f32(float, float, float)
declare i32 @llvm.bitreverse.i32(i32)
declare i32 @llvm.ctpop.i32(i32)
declare i32 @llvm.cttz.i32(i32, i1)
declare i32 @llvm.ctlz.i32(i32, i1)
declare i32 @llvm.smax.i32(i32, i32)
declare i32 @llvm.smin.i32(i32, i32)
declare i32 @llvm.umax.i32(i32, i32)
declare i32 @llvm.umin.i32(i32, i32)

define i32 @run() {
entry:
  call void @main()
  br label %loop
loop:
  %i = phi i32 [ 0, %entry ], [ %n, %loop ]
  %p = getelementptr [16 x i32], ptr @out, i32 0, i32 %i
  %v = load i32, ptr %p
  %q = getelementptr [16 x i8], ptr @written, i32 0, i32 %i
  %w = load i8, ptr %q
  %wz = zext i8 %w to i32
  %x = call i32 (ptr, ...) @printf(ptr @fmt, i32 %wz, i32 %v)
  %n = add i32 %i, 1
  %done = icmp eq i32 %n, 16
  br i1 %done, label %end, label %loop
end:
  ret i32 0
}

; Resources: the one CBV is @consts, every heap view is the UAV @out.
define %dx.types.Handle @dx.op.createHandleFromBinding(i32 %op, %dx.types.ResBind %b, i32 %i, i1 %nu) {
  ret %dx.types.Handle { ptr @consts }
}
define %dx.types.Handle @dx.op.createHandleFromHeap(i32 %op, i32 %i, i1 %s, i1 %nu) {
  ret %dx.types.Handle { ptr @out }
}
define %dx.types.Handle @dx.op.annotateHandle(i32 %op, %dx.types.Handle %h, %dx.types.ResourceProperties %p) {
  ret %dx.types.Handle %h
}
define %dx.types.Dimensions @dx.op.getDimensions(i32 %op, %dx.types.Handle %h, i32 %mip) {
  ret %dx.types.Dimensions { i32 64, i32 0, i32 0, i32 0 }
}
define %dx.types.CBufRet.i32 @dx.op.cbufferLoadLegacy.i32(i32 %op, %dx.types.Handle %h, i32 %row) {
  %p = extractvalue %dx.types.Handle %h, 0
  %i = mul i32 %row, 4
  %ok = icmp ult i32 %row, 2
  %j = select i1 %ok, i32 %i, i32 0
  %q = getelementptr i32, ptr %p, i32 %j
  %v = load <4 x i32>, ptr %q, align 4
  %a = extractelement <4 x i32> %v, i32 0
  %b = extractelement <4 x i32> %v, i32 1
  %c = extractelement <4 x i32> %v, i32 2
  %d = extractelement <4 x i32> %v, i32 3
  %r0 = insertvalue %dx.types.CBufRet.i32 undef, i32 %a, 0
  %r1 = insertvalue %dx.types.CBufRet.i32 %r0, i32 %b, 1
  %r2 = insertvalue %dx.types.CBufRet.i32 %r1, i32 %c, 2
  %r3 = insertvalue %dx.types.CBufRet.i32 %r2, i32 %d, 3
  ret %dx.types.CBufRet.i32 %r3
}

; A raw buffer's index is a byte offset; a dword past the 64 bytes is dropped.
define void @store_dword(i32 %at, i32 %v) {
  %ok = icmp ult i32 %at, 16
  br i1 %ok, label %st, label %done
st:
  %p = getelementptr [16 x i32], ptr @out, i32 0, i32 %at
  store i32 %v, ptr %p
  %q = getelementptr [16 x i8], ptr @written, i32 0, i32 %at
  store i8 1, ptr %q
  br label %done
done:
  ret void
}
define void @store_lane(i32 %at, i32 %k, i32 %v, i8 %mask) {
  %kk = trunc i32 %k to i8
  %m = shl i8 1, %kk
  %on = and i8 %mask, %m
  %yes = icmp ne i8 %on, 0
  br i1 %yes, label %st, label %done
st:
  %d = add i32 %at, %k
  call void @store_dword(i32 %d, i32 %v)
  br label %done
done:
  ret void
}
define void @dx.op.rawBufferStore.i32(i32 %op, %dx.types.Handle %h, i32 %index, i32 %off, i32 %v0, i32 %v1, i32 %v2, i32 %v3, i8 %mask, i32 %align) {
  %at = lshr i32 %index, 2
  call void @store_lane(i32 %at, i32 0, i32 %v0, i8 %mask)
  call void @store_lane(i32 %at, i32 1, i32 %v1, i8 %mask)
  call void @store_lane(i32 %at, i32 2, i32 %v2, i8 %mask)
  call void @store_lane(i32 %at, i32 3, i32 %v3, i8 %mask)
  ret void
}

; One thread in one group.
define i32 @dx.op.threadIdInGroup.i32(i32 %op, i32 %c) {
  ret i32 0
}
define i32 @dx.op.threadId.i32(i32 %op, i32 %c) {
  ret i32 0
}
define i32 @dx.op.groupId.i32(i32 %op, i32 %c) {
  ret i32 0
}
define i32 @dx.op.flattenedThreadIdInGroup.i32(i32 %op) {
  ret i32 0
}
define i32 @dx.op.waveGetLaneIndex(i32 %op) {
  ret i32 0
}
define i32 @dx.op.waveGetLaneCount(i32 %op) {
  ret i32 32
}
define i32 @dx.op.waveReadLaneFirst.i32(i32 %op, i32 %x) {
  ret i32 %x
}
define void @dx.op.barrier(i32 %op, i32 %flags) {
  ret void
}

; Float conversions to and from f16 bits (DXIL rounds to nearest even).
define i32 @dx.op.legacyF32ToF16(i32 %op, float %x) {
  %h = fptrunc float %x to half
  %b = bitcast half %h to i16
  %r = zext i16 %b to i32
  ret i32 %r
}
define float @dx.op.legacyF16ToF32(i32 %op, i32 %x) {
  %t = trunc i32 %x to i16
  %h = bitcast i16 %t to half
  %r = fpext half %h to float
  ret float %r
}

define float @dx.op.unary.f32(i32 %op, float %x) {
  switch i32 %op, label %bad [
    i32 6, label %fabs
    i32 7, label %saturate
    i32 12, label %cos
    i32 13, label %sin
    i32 21, label %exp
    i32 22, label %frc
    i32 23, label %log
    i32 24, label %sqrt
    i32 25, label %rsqrt
    i32 26, label %round_ne
    i32 27, label %round_ni
    i32 28, label %round_pi
    i32 29, label %round_z
  ]
fabs:
  %a = call float @llvm.fabs.f32(float %x)
  ret float %a
saturate:
  ; NaN gives 0.
  %s0 = call float @llvm.maxnum.f32(float %x, float 0.0)
  %s1 = call float @llvm.minnum.f32(float %s0, float 1.0)
  ret float %s1
cos:
  %c = call float @llvm.cos.f32(float %x)
  ret float %c
sin:
  %si = call float @llvm.sin.f32(float %x)
  ret float %si
exp:
  %e = call float @llvm.exp2.f32(float %x)
  ret float %e
frc:
  %fl = call float @llvm.floor.f32(float %x)
  %f = fsub float %x, %fl
  ret float %f
log:
  %l = call float @llvm.log2.f32(float %x)
  ret float %l
sqrt:
  %q = call float @llvm.sqrt.f32(float %x)
  ret float %q
rsqrt:
  %rq = call float @llvm.sqrt.f32(float %x)
  %r = fdiv float 1.0, %rq
  ret float %r
round_ne:
  %ne = call float @llvm.roundeven.f32(float %x)
  ret float %ne
round_ni:
  %ni = call float @llvm.floor.f32(float %x)
  ret float %ni
round_pi:
  %pi = call float @llvm.ceil.f32(float %x)
  ret float %pi
round_z:
  %z = call float @llvm.trunc.f32(float %x)
  ret float %z
bad:
  call void @unknown(i32 %op)
  unreachable
}

define float @dx.op.binary.f32(i32 %op, float %a, float %b) {
  switch i32 %op, label %bad [
    i32 35, label %max
    i32 36, label %min
  ]
max:
  %x = call float @llvm.maxnum.f32(float %a, float %b)
  ret float %x
min:
  %n = call float @llvm.minnum.f32(float %a, float %b)
  ret float %n
bad:
  call void @unknown(i32 %op)
  unreachable
}

define float @dx.op.tertiary.f32(i32 %op, float %a, float %b, float %c) {
  switch i32 %op, label %bad [
    i32 46, label %fmad
    i32 47, label %fma
  ]
fmad:
  ; Not fused: the product rounds, then the sum. (A driver may fuse FMad,
  ; so a translation that needs one rounding cannot be checked here.)
  %p = fmul float %a, %b
  %s = fadd float %p, %c
  ret float %s
fma:
  %f = call float @llvm.fma.f32(float %a, float %b, float %c)
  ret float %f
bad:
  call void @unknown(i32 %op)
  unreachable
}

define i32 @dx.op.unary.i32(i32 %op, i32 %x) {
  switch i32 %op, label %bad [
    i32 30, label %bfrev
  ]
bfrev:
  %r = call i32 @llvm.bitreverse.i32(i32 %x)
  ret i32 %r
bad:
  call void @unknown(i32 %op)
  unreachable
}

; Countbits; FirstbitLo; FirstbitHi and FirstbitSHi count from the top bit
; (DXC subtracts from 31 for HLSL's firstbithigh); -1 when no bit is found.
define i32 @dx.op.unaryBits.i32(i32 %op, i32 %x) {
  %neg = icmp slt i32 %x, 0
  %inv = xor i32 %x, -1
  %sx = select i1 %neg, i32 %inv, i32 %x
  switch i32 %op, label %bad [
    i32 31, label %count
    i32 32, label %lo
    i32 33, label %hi
    i32 34, label %shi
  ]
count:
  %c = call i32 @llvm.ctpop.i32(i32 %x)
  ret i32 %c
lo:
  %z0 = icmp eq i32 %x, 0
  %t = call i32 @llvm.cttz.i32(i32 %x, i1 false)
  %l = select i1 %z0, i32 -1, i32 %t
  ret i32 %l
hi:
  %z1 = icmp eq i32 %x, 0
  %h = call i32 @llvm.ctlz.i32(i32 %x, i1 false)
  %hr = select i1 %z1, i32 -1, i32 %h
  ret i32 %hr
shi:
  %z2 = icmp eq i32 %sx, 0
  %sh = call i32 @llvm.ctlz.i32(i32 %sx, i1 false)
  %sr = select i1 %z2, i32 -1, i32 %sh
  ret i32 %sr
bad:
  call void @unknown(i32 %op)
  unreachable
}

define i32 @dx.op.binary.i32(i32 %op, i32 %a, i32 %b) {
  switch i32 %op, label %bad [
    i32 37, label %imax
    i32 38, label %imin
    i32 39, label %umax
    i32 40, label %umin
  ]
imax:
  %r0 = call i32 @llvm.smax.i32(i32 %a, i32 %b)
  ret i32 %r0
imin:
  %r1 = call i32 @llvm.smin.i32(i32 %a, i32 %b)
  ret i32 %r1
umax:
  %r2 = call i32 @llvm.umax.i32(i32 %a, i32 %b)
  ret i32 %r2
umin:
  %r3 = call i32 @llvm.umin.i32(i32 %a, i32 %b)
  ret i32 %r3
bad:
  call void @unknown(i32 %op)
  unreachable
}

; Doubles: MakeDouble (101) and SplitDouble (102) join and split the dword
; pair; Fma (47) and DMax/DMin (35/36) as for floats.
%dx.types.splitdouble = type { i32, i32 }
declare double @llvm.fma.f64(double, double, double)
declare double @llvm.maxnum.f64(double, double)
declare double @llvm.minnum.f64(double, double)
define double @dx.op.makeDouble.f64(i32 %op, i32 %lo, i32 %hi) {
  %l = zext i32 %lo to i64
  %h = zext i32 %hi to i64
  %hs = shl i64 %h, 32
  %b = or i64 %hs, %l
  %d = bitcast i64 %b to double
  ret double %d
}
define %dx.types.splitdouble @dx.op.splitDouble.f64(i32 %op, double %d) {
  %b = bitcast double %d to i64
  %l = trunc i64 %b to i32
  %hs = lshr i64 %b, 32
  %h = trunc i64 %hs to i32
  %r0 = insertvalue %dx.types.splitdouble undef, i32 %l, 0
  %r1 = insertvalue %dx.types.splitdouble %r0, i32 %h, 1
  ret %dx.types.splitdouble %r1
}
define double @dx.op.tertiary.f64(i32 %op, double %a, double %b, double %c) {
  %f = call double @llvm.fma.f64(double %a, double %b, double %c)
  ret double %f
}
define double @dx.op.binary.f64(i32 %op, double %a, double %b) {
  switch i32 %op, label %bad [
    i32 35, label %max
    i32 36, label %min
  ]
max:
  %x = call double @llvm.maxnum.f64(double %a, double %b)
  ret double %x
min:
  %n = call double @llvm.minnum.f64(double %a, double %b)
  ret double %n
bad:
  call void @unknown(i32 %op)
  unreachable
}

; Dot4AddI8Packed (163) and Dot4AddU8Packed (164): acc + the four byte products.
define i32 @dx.op.dot4AddPacked.i32(i32 %op, i32 %acc, i32 %a, i32 %b) {
  %va = bitcast i32 %a to <4 x i8>
  %vb = bitcast i32 %b to <4 x i8>
  %signed = icmp eq i32 %op, 163
  %sa = sext <4 x i8> %va to <4 x i32>
  %sb = sext <4 x i8> %vb to <4 x i32>
  %ua = zext <4 x i8> %va to <4 x i32>
  %ub = zext <4 x i8> %vb to <4 x i32>
  %xa = select i1 %signed, <4 x i32> %sa, <4 x i32> %ua
  %xb = select i1 %signed, <4 x i32> %sb, <4 x i32> %ub
  %p = mul <4 x i32> %xa, %xb
  %p0 = extractelement <4 x i32> %p, i32 0
  %p1 = extractelement <4 x i32> %p, i32 1
  %p2 = extractelement <4 x i32> %p, i32 2
  %p3 = extractelement <4 x i32> %p, i32 3
  %s0 = add i32 %acc, %p0
  %s1 = add i32 %s0, %p1
  %s2 = add i32 %s1, %p2
  %s3 = add i32 %s2, %p3
  ret i32 %s3
}

; An opcode this runtime does not know: stop, and say which.
@bad = private constant [30 x i8] c"cpu runtime: no dx.op %u\0A\00\00\00\00\00"
declare void @exit(i32)
define void @unknown(i32 %op) {
  %x = call i32 (ptr, ...) @printf(ptr @bad, i32 %op)
  call void @exit(i32 3)
  ret void
}
"#;
