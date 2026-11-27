//! Which ALU instructions the translator lowers, against a checked-in list.
//!
//! Every VOP1, VOP2, VOP3, VOP3P, SOP1 and SOP2 opcode the decoder names
//! goes alone into a one-thread compute program (its operands from root
//! constants, its result stored to a buffer) and is lowered. Whether it
//! lowers, is refused, or panics is compared with `lowering-coverage.txt`,
//! so a merge or refactor that loses a lowering fails here rather than in
//! a title. A new lowering changes the list too: regenerate it with
//! `VICTORY_BLESS=1 cargo test -p victory-shader coverage` and commit it.
//! A change is reported as a warning, and fails the test only with
//! `VICTORY_STRICT_TESTS=1`, so a PR that adds a lowering is not blocked
//! by a stale list.

use super::lower::{Parts, lower};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const SNAPSHOT: &str = include_str!("lowering-coverage.txt");
const S_ENDPGM: u32 = 0xBF81_0000;
/// `v_mov_b32 v0, s20`: a scalar result into the stored register.
const V_MOV_V0_S20: u32 = 0x7E00_0214;

/// The instruction words of each case: (family, opcode, body).
fn cases() -> Vec<(&'static str, u32, Vec<u32>)> {
    // Vector sources v1, v2, v3 (loaded from s0..s2), result in v0; scalar
    // sources s1 (and s1), result in s20 (clear of the descriptor in s[8:11]).
    let vsrc = 0x101 | 0x102 << 9 | 0x103 << 18;
    let mut c = Vec::new();
    for op in 0..64 {
        c.push(("vop2", op, vec![op << 25 | 2 << 9 | 0x101]));
    }
    for op in 0..128 {
        c.push(("vop1", op, vec![0x7E00_0000 | op << 9 | 0x101]));
    }
    for op in 0..1024 {
        c.push(("vop3", op, vec![0xD400_0000 | op << 16, vsrc]));
    }
    for op in 0..128 {
        c.push(("vop3p", op, vec![0xCC00_0000 | op << 16, vsrc | 3 << 27]));
    }
    for op in 0..128 {
        c.push((
            "sop2",
            op,
            vec![0x8000_0000 | op << 23 | 20 << 16 | 1 << 8 | 1, V_MOV_V0_S20],
        ));
    }
    for op in 0..256 {
        c.push((
            "sop1",
            op,
            vec![0xBE80_0000 | 20 << 16 | op << 8 | 1, V_MOV_V0_S20],
        ));
    }
    c
}

/// `body` between a prologue that loads v1..v4 from root constants and
/// the buffer descriptor into s[8:11], and a store of v0.
fn program(body: &[u32]) -> Vec<u32> {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0xF408_0202, // s_load_dwordx4 s[8:11], s[4:5], 0
        0xFA00_0000,
    ];
    // v_mov_b32 v1..v4, s0..s3
    words.extend((0..4u32).map(|i| 0x7E00_0200 | (i + 1) << 17 | i));
    words.extend_from_slice(body);
    words.extend([
        0xE070_0000, // buffer_store_dword v0, off, s[8:11], 0
        0x8002_0000,
        S_ENDPGM,
    ]);
    words
}

/// The decoder's name for the instruction `body` starts with, if any.
fn name(body: &[u32]) -> Option<&'static str> {
    let words = program(body);
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let insts = crate::gfx10::decode(&code).ok()?;
    insts.iter().find(|i| i.offset == 28)?.name()
}

/// Lowers `body` in the compute program; `Err` with the stage's reason.
fn lowers(body: &[u32]) -> Result<(), crate::ShaderError> {
    let words = program(body);
    let root = |root, user_data, slot| RootParam {
        root,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data,
        slot,
    };
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 256,
        layout: vec![
            RootParam {
                sgprs: 4,
                ..root(RootType::Constants, 0, 0)
            },
            root(RootType::Table, 4, 1),
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
            // Five user SGPRs, 512 bytes of LDS.
            rsrc2: 1 << 15 | 5 << 1,
            threads: [1, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, false, 256)?;
    let st = structure::structure(&p)?;
    let res = resource::track(&p, &native.layout, native.stage, Some(&st))?;
    let io = io::raise(&p, &native)?;
    lower(&Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: vec![],
        outputs: vec![],
        prims: vec![],
        mesh_stride: 0,
    })
    .map(|_| ())
}

/// One line per named opcode: `family opcode name status`.
fn coverage() -> String {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut out = String::new();
    for (family, op, body) in cases() {
        let Some(name) = name(&body) else {
            continue;
        };
        let status = match std::panic::catch_unwind(|| lowers(&body)) {
            Ok(Ok(())) => "lowers",
            Ok(Err(_)) => "refused",
            Err(_) => "panics",
        };
        out.push_str(&format!("{family} {op:#05x} {name} {status}\n"));
    }
    std::panic::set_hook(hook);
    out
}

#[test]
fn lowering_coverage_matches_the_snapshot() {
    let now = coverage();
    if std::env::var_os("VICTORY_BLESS").is_some() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/emit/lowering-coverage.txt"
        );
        std::fs::write(path, &now).expect("writing the snapshot");
        return;
    }
    let was: std::collections::BTreeMap<_, _> = SNAPSHOT
        .lines()
        .filter_map(|l| l.rsplit_once(' '))
        .collect();
    let mut changes = Vec::new();
    for line in now.lines() {
        let (case, status) = line.rsplit_once(' ').expect("case and status");
        match was.get(case) {
            Some(&old) if old == status => {}
            Some(&old) => changes.push(format!("{case}: {old} -> {status}")),
            None => changes.push(format!("{case}: new, {status}")),
        }
    }
    let now_cases: std::collections::BTreeSet<_> = now
        .lines()
        .filter_map(|l| l.rsplit_once(' '))
        .map(|(c, _)| c)
        .collect();
    changes.extend(
        was.keys()
            .filter(|c| !now_cases.contains(*c))
            .map(|c| format!("{c}: no longer decoded")),
    );
    if changes.is_empty() {
        return;
    }
    let report = format!(
        "lowering coverage changed (VICTORY_BLESS=1 rewrites lowering-coverage.txt):\n{}",
        changes.join("\n")
    );
    let strict = std::env::var("VICTORY_STRICT_TESTS").is_ok_and(|v| v == "1");
    assert!(!strict, "{report}");
    eprintln!("warning: {report}");
}
