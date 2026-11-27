//! The decoder against LLVM's: one encoding of every opcode of every
//! encoding family, with and without a literal, and what
//! `llvm-mc -triple=amdgcn -mcpu=gfx1030 --disassemble` makes of it,
//! checked in as `llvm-mc-gfx1030.txt` (regenerate with
//! `cargo test -p victory-shader regenerate_llvm_fixture -- --ignored`,
//! which needs `llvm-mc` on PATH or in `LLVM_MC`).
//!
//! The name (without LLVM's `_e32`/`_e64` suffixes) and the length in
//! dwords must agree, except for the cases in [`KNOWN`]. A wrong length
//! puts every later instruction out of step, which is the bug this catches
//! first.

use super::decode;

const FIXTURE: &str = include_str!("llvm-mc-gfx1030.txt");
const S_ENDPGM: u32 = 0xBF81_0000;

/// Disagreements known today, by family and the case's first word, with
/// why: reported, but not a failure.
const KNOWN: &[(&str, &str, &str)] = &[
    (
        "sop2",
        "bb0a0604",
        "the test's branch target is past the program",
    ),
    (
        "sop2 literal",
        "bb0a06ff",
        "the test's branch target is past the program",
    ),
    (
        "vop1 literal",
        "7e0204ff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "sop1 literal",
        "be8a20ff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "sop1 literal",
        "be8a22ff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "sop1 literal",
        "be8a2eff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "sop1 literal",
        "be8a2fff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop1 literal",
        "7e0286ff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop1 literal",
        "7e0288ff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop1 literal",
        "7e0290ff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "mimg",
        "f1840108",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "vop1 literal",
        "7e02caff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop1 literal",
        "7e02d0ff",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "mimg",
        "f1a00109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "mimg",
        "f1a40109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "mimg",
        "f1a80109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "mimg",
        "f1ac0109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "mimg",
        "f1b00109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "mimg",
        "f1b40109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "mimg",
        "f1b80109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "mimg",
        "f1bc0109",
        "no name yet (#69 adds the cd_g16 forms and gather4h)",
    ),
    (
        "vop2",
        "0c020702",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop2 literal",
        "0c0206ff",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop3 literal src2",
        "d5010001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3",
        "d5060001",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop3 literal",
        "d5060001",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop3 literal src2",
        "d5060001",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop3 literal src2",
        "d5280001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal src2",
        "d5290001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal src2",
        "d52a0001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3",
        "d5400001",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop3 literal",
        "d5400001",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop3 literal src2",
        "d5400001",
        "GFX10.1 name; gfx1030 has the fused fmac/fma_legacy here",
    ),
    (
        "vop3 literal src2",
        "d5750001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d5c30001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d5c40001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d5c80001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d6000001",
        "an attribute, not a literal (#70)",
    ),
    (
        "vop3 literal",
        "d6010001",
        "an attribute, not a literal (#70)",
    ),
    (
        "vop3 literal",
        "d6020001",
        "an attribute, not a literal (#70)",
    ),
    (
        "vop3 literal",
        "d7420001",
        "an attribute, not a literal (#70)",
    ),
    (
        "vop3 literal",
        "d7430001",
        "an attribute, not a literal (#70)",
    ),
    (
        "vop3 literal src2",
        "d7430001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d75a0001",
        "an attribute, not a literal (#70)",
    ),
    (
        "vop3 literal src2",
        "d75a0001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d7600001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d7770001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
    (
        "vop3 literal",
        "d7780001",
        "a register-only operand: LLVM reads no literal there, code never has one",
    ),
];

/// The encodings: (family, words) for every opcode of each family.
fn cases() -> Vec<(&'static str, Vec<u32>)> {
    let lit = 0x1122_3344u32;
    let mut c: Vec<(&'static str, Vec<u32>)> = Vec::new();
    for op in 0..128u32 {
        c.push(("sop2", vec![0x8000_0000 | op << 23 | 10 << 16 | 6 << 8 | 4]));
        c.push((
            "sop2 literal",
            vec![0x8000_0000 | op << 23 | 10 << 16 | 6 << 8 | 0xFF, lit],
        ));
        c.push(("sopc", vec![0xBF00_0000 | op << 16 | 6 << 8 | 4]));
        c.push((
            "sopc literal",
            vec![0xBF00_0000 | op << 16 | 6 << 8 | 0xFF, lit],
        ));
        c.push(("sopp", vec![0xBF80_0000 | op << 16]));
        c.push((
            "vop3p",
            vec![
                0xCC00_4001 | op << 16,
                0x1810_0000 | 0x104 << 18 | 0x103 << 9 | 0x102,
            ],
        ));
        c.push((
            "vop3p literal",
            vec![
                0xCC00_4001 | op << 16,
                0x1810_0000 | 0x104 << 18 | 0x103 << 9 | 0xFF,
                lit,
            ],
        ));
        c.push((
            "flat",
            vec![
                0xDC00_0010 | op << 18,
                0x0600_0000 | 0x7D << 16 | 4 << 8 | 2,
            ],
        ));
        c.push((
            "scratch",
            vec![
                0xDC00_4010 | op << 18,
                0x0600_0000 | 0x7D << 16 | 4 << 8 | 2,
            ],
        ));
        c.push((
            "global",
            vec![
                0xDC00_8010 | op << 18,
                0x0600_0000 | 0x7D << 16 | 4 << 8 | 2,
            ],
        ));
    }
    for op in 0..32u32 {
        c.push((
            "sopk",
            vec![0xB000_0000 | op << 23 | 10 << 16 | 0x0010, lit],
        ));
    }
    for op in 0..256u32 {
        c.push(("sop1", vec![0xBE80_0000 | 10 << 16 | op << 8 | 4]));
        c.push((
            "sop1 literal",
            vec![0xBE80_0000 | 10 << 16 | op << 8 | 0xFF, lit],
        ));
        c.push(("vop1", vec![0x7E00_0000 | 1 << 17 | op << 9 | 0x102]));
        c.push((
            "vop1 literal",
            vec![0x7E00_0000 | 1 << 17 | op << 9 | 0xFF, lit],
        ));
        c.push(("vopc", vec![0x7C00_0000 | op << 17 | 3 << 9 | 0x102]));
        c.push((
            "vopc literal",
            vec![0x7C00_0000 | op << 17 | 3 << 9 | 0xFF, lit],
        ));
        c.push((
            "smem",
            vec![0xF400_0000 | op << 18 | 10 << 6 | 4, 0xFA00_0010],
        ));
        c.push(("ds", vec![0xD800_2010 | op << 18, 0x0806_0402]));
        let mubuf = 0xE000_1010 | (op & 0x7F) << 18 | (op >> 7) << 25;
        c.push(("mubuf", vec![mubuf, 0x8002_0402]));
        let mimg = 0xF000_0108 | (op & 0x7F) << 18 | (op >> 7);
        c.push(("mimg", vec![mimg, 0x0082_0402]));
    }
    for op in 0..64u32 {
        c.push(("vop2", vec![op << 25 | 1 << 17 | 3 << 9 | 0x102]));
        c.push((
            "vop2 literal",
            vec![op << 25 | 1 << 17 | 3 << 9 | 0xFF, lit],
        ));
    }
    for op in 0..1024u32 {
        c.push((
            "vop3",
            vec![0xD400_0001 | op << 16, 0x104 << 18 | 0x103 << 9 | 0x102],
        ));
        c.push((
            "vop3 literal",
            vec![0xD400_0001 | op << 16, 0x104 << 18 | 0x103 << 9 | 0xFF, lit],
        ));
        c.push((
            "vop3 literal src2",
            vec![0xD400_0001 | op << 16, 0xFF << 18 | 0x103 << 9 | 0x102, lit],
        ));
    }
    for op in 0..16u32 {
        c.push((
            "mtbuf",
            vec![
                0xE800_1010 | (op & 7) << 16 | 0x16 << 19,
                0x8002_0402 | (op >> 3) << 21,
            ],
        ));
    }
    for op in 0..4u32 {
        c.push((
            "vintrp",
            vec![0xC800_0002 | op << 16 | 1 << 18 | 3 << 10 | 1 << 8],
        ));
    }
    c.push(("exp", vec![0xF800_080F, 0x0403_0201]));
    c
}

fn hex(words: &[u32]) -> String {
    words
        .iter()
        .map(|w| format!("{w:08x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Our name (or `-`), length in dwords, and how many source operands we
/// decode (for an ALU instruction) for `words`.
fn ours(words: &[u32]) -> (String, usize, Option<usize>) {
    // Room behind it: an instruction that wants a literal it lacks takes
    // the next word, and a branch lands in the padding.
    let mut code = words.to_vec();
    code.extend([S_ENDPGM; 40]);
    let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let Some(i) = decode(&bytes)
        .ok()
        .and_then(|insts| insts.into_iter().find(|i| i.offset == 0))
    else {
        return ("-".into(), 0, None);
    };
    let sources = match i.fields(false) {
        super::Fields::Valu(v) => Some(v.src.len()),
        super::Fields::Sop { src, .. } => Some(src.len()),
        _ => None,
    };
    (i.name().unwrap_or("-").to_string(), i.words.len(), sources)
}

/// LLVM's spelling without the encoding-size suffix.
fn plain(name: &str) -> &str {
    name.strip_suffix("_e32")
        .or_else(|| name.strip_suffix("_e64"))
        .unwrap_or(name)
}

/// Every case where we and LLVM disagree on a name LLVM decodes:
/// `family<TAB>words<TAB>what`.
fn disagreements() -> Vec<String> {
    let mut out = Vec::new();
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let mut f = line.split('\t');
        let (Some(family), Some(words), Some(name), Some(len)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        if name == "-" {
            continue; // LLVM has no such instruction either
        }
        let words: Vec<u32> = words
            .split(' ')
            .map(|w| u32::from_str_radix(w, 16).expect("hex word"))
            .collect();
        let len: usize = len.parse().expect("dword count");
        let (our_name, our_len, sources) = ours(&words);
        // 0xFF in a source field the instruction does not have: LLVM (and
        // the compiler, which never writes it) reads no literal there; we
        // do. Not a decoding of real code.
        let field = if family.ends_with("src2") { 2 } else { 0 };
        if family.contains("literal")
            && our_name == plain(name)
            && our_len == len + 1
            && sources.is_some_and(|n| field >= n)
        {
            continue;
        }
        let what = match (our_name == plain(name), our_len == len) {
            (true, true) => continue,
            (true, false) => format!("{name}: {our_len} dwords, LLVM {len}"),
            (false, true) if our_name == "-" => format!("{name}: we have no name"),
            (false, true) => format!("{name}: we say {our_name}"),
            (false, false) => format!("{name}: we say {our_name}, {our_len} dwords, LLVM {len}"),
        };
        out.push(format!("{family}\t{}\t{what}", hex(&words)));
    }
    out
}

#[test]
fn decoder_agrees_with_llvm() {
    assert!(FIXTURE.lines().count() > 3000, "the fixture is missing");
    let mut unexpected = Vec::new();
    for d in disagreements() {
        let first = d
            .split('\t')
            .nth(1)
            .unwrap_or("")
            .split(' ')
            .next()
            .unwrap_or("");
        let family = d.split('\t').next().unwrap_or("");
        if KNOWN.iter().any(|&(f, w, _)| f == family && w == first) {
            eprintln!("known: {d}");
        } else {
            unexpected.push(d);
        }
    }
    assert!(
        unexpected.is_empty(),
        "the decoder disagrees with llvm-mc (fix it, or list the case in KNOWN with why):\n{}",
        unexpected.join("\n")
    );
}

/// Writes `llvm-mc-gfx1030.txt` from llvm-mc's disassembly of [`cases`].
#[test]
#[ignore]
fn regenerate_llvm_fixture() {
    let mc = std::env::var("LLVM_MC").unwrap_or_else(|_| "llvm-mc".into());
    let mut out = String::from(
        "# llvm-mc -triple=amdgcn -mcpu=gfx1030 --disassemble of each case in\n\
         # llvm_tests.rs: family, words, LLVM's name (- if none), dwords.\n",
    );
    for (family, words) in cases() {
        let mut code = words.clone();
        code.push(S_ENDPGM);
        let input: Vec<String> = code
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .map(|b| format!("{b:#04x}"))
            .collect();
        let run = std::process::Command::new(&mc)
            .args([
                "--disassemble",
                "-triple=amdgcn",
                "-mcpu=gfx1030",
                "-show-encoding",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .take()
                    .expect("stdin")
                    .write_all(input.join(",").as_bytes())?;
                child.wait_with_output()
            })
            .expect("running llvm-mc");
        let text = String::from_utf8_lossy(&run.stdout);
        // LLVM warns about a word it cannot decode and goes on with the
        // next; a warning at line 1, column 1 is about the case's own
        // first word. Otherwise the first instruction printed is the case.
        let invalid = String::from_utf8_lossy(&run.stderr).contains("<stdin>:1:1: warning");
        let first = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('.'))
            .filter(|_| !invalid)
            .and_then(|l| {
                let name = l.split_whitespace().next()?;
                let enc = l.split("encoding: [").nth(1)?.split(']').next()?;
                Some((name.to_string(), enc.split(',').count() / 4))
            });
        let (name, len) = first.unwrap_or(("-".into(), 0));
        out.push_str(&format!("{family}\t{}\t{name}\t{len}\n", hex(&words)));
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/gfx10/llvm-mc-gfx1030.txt");
    std::fs::write(path, out).expect("writing the fixture");
}
