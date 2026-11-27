//! Ported from fxceless/victory-gta6 PR #1189 (Series S interface plane).
//! A present's second plane laid over its first, with AMD FidelityFX Super
//! Resolution 1.0 (FSR1 EASU + RCAS) when upscaling the game plane to the
//! interface plane's size.
//!
//! A Series S title presents two planes (plane 0 the game, e.g. 1920x1080;
//! plane 1 the interface, e.g. 3840x2160). When upscaling (`ui` larger than
//! `game`) and `VICTORY_FSR` is enabled (default `1`), EASU spatially upscales
//! `game` to the output resolution and RCAS sharpens it (configured by
//! `VICTORY_FSR_SHARPNESS`, default `0.2` stops) before `ui` is composited
//! over it at native resolution. Otherwise a single compute pass composites
//! `ui` over `game`.

use std::sync::Mutex;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, s};

/// Frames of descriptors in the ring (five descriptors each): presents in
/// flight must not find theirs rewritten.
const SLOTS: u32 = 16;
const DESCRIPTORS_PER_SLOT: u32 = 5;

/// AMD FSR 1.0 EASU (Edge-Adaptive Spatial Upsampling) compute shader.
const EASU_SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=5, b0), DescriptorTable(SRV(t0, numDescriptors=1), UAV(u0, numDescriptors=1))"
cbuffer Params : register(b0) {
    uint in_w;
    uint in_h;
    uint out_w;
    uint out_h;
    uint game_srgb;
};
Texture2D<float4> game : register(t0);
RWTexture2D<float4> easu_out : register(u0);

float3 linear_to_srgb(float3 c) {
    c = saturate(c);
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(max(c, 1e-6), 1.0 / 2.4) - 0.055;
    return lerp(lo, hi, step(0.0031308, c));
}

float3 load_in(int2 p, int2 max_coord) {
    float3 c = game.Load(int3(clamp(p, int2(0, 0), max_coord), 0)).rgb;
    if (game_srgb != 0) c = linear_to_srgb(c);
    return c;
}

float fsr_luma(float3 c) {
    return c.b * 0.5 + (c.r * 0.5 + c.g);
}

void fsr_easu_set(
    inout float2 dir,
    inout float len,
    float2 pp,
    bool biS,
    bool biT,
    bool biU,
    bool biV,
    float lA,
    float lB,
    float lC,
    float lD,
    float lE
) {
    float w = 0.0;
    if (biS) w = (1.0 - pp.x) * (1.0 - pp.y);
    if (biT) w = pp.x * (1.0 - pp.y);
    if (biU) w = (1.0 - pp.x) * pp.y;
    if (biV) w = pp.x * pp.y;

    float dc = lD - lC;
    float cb = lC - lB;
    float lenX = max(abs(dc), abs(cb));
    lenX = lenX > 0.0 ? rcp(lenX) : 0.0;
    float dirX = lD - lB;
    dir.x += dirX * w;
    lenX = saturate(abs(dirX) * lenX);
    lenX *= lenX;
    len += lenX * w;

    float ec = lE - lC;
    float ca = lC - lA;
    float lenY = max(abs(ec), abs(ca));
    lenY = lenY > 0.0 ? rcp(lenY) : 0.0;
    float dirY = lE - lA;
    dir.y += dirY * w;
    lenY = saturate(abs(dirY) * lenY);
    lenY *= lenY;
    len += lenY * w;
}

void fsr_easu_tap(
    inout float3 aC,
    inout float aW,
    float2 off,
    float2 dir,
    float2 len2,
    float lob,
    float clp,
    float3 c
) {
    float2 v;
    v.x = (off.x * dir.x) + (off.y * dir.y);
    v.y = (off.x * (-dir.y)) + (off.y * dir.x);
    v *= len2;
    float d2 = min(v.x * v.x + v.y * v.y, clp);
    float wB = (2.0 / 5.0) * d2 - 1.0;
    float wA = lob * d2 - 1.0;
    wB *= wB;
    wA *= wA;
    wB = (25.0 / 16.0) * wB - (25.0 / 16.0 - 1.0);
    float w = wB * wA;
    aC += c * w;
    aW += w;
}

[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= out_w || id.y >= out_h) return;

    int2 max_coord = int2(int(in_w) - 1, int(in_h) - 1);
    float2 pp = (float2(id.xy) + 0.5) * (float2(in_w, in_h) / float2(out_w, out_h)) - 0.5;
    float2 fp = floor(pp);
    pp -= fp;
    int2 ip = int2(fp);

    float3 b = load_in(ip + int2( 0, -1), max_coord);
    float3 c = load_in(ip + int2( 1, -1), max_coord);
    float3 e = load_in(ip + int2(-1,  0), max_coord);
    float3 f = load_in(ip + int2( 0,  0), max_coord);
    float3 g = load_in(ip + int2( 1,  0), max_coord);
    float3 h = load_in(ip + int2( 2,  0), max_coord);
    float3 i = load_in(ip + int2(-1,  1), max_coord);
    float3 j = load_in(ip + int2( 0,  1), max_coord);
    float3 k = load_in(ip + int2( 1,  1), max_coord);
    float3 l = load_in(ip + int2( 2,  1), max_coord);
    float3 n = load_in(ip + int2( 0,  2), max_coord);
    float3 o = load_in(ip + int2( 1,  2), max_coord);

    float bL = fsr_luma(b);
    float cL = fsr_luma(c);
    float eL = fsr_luma(e);
    float fL = fsr_luma(f);
    float gL = fsr_luma(g);
    float hL = fsr_luma(h);
    float iL = fsr_luma(i);
    float jL = fsr_luma(j);
    float kL = fsr_luma(k);
    float lL = fsr_luma(l);
    float nL = fsr_luma(n);
    float oL = fsr_luma(o);

    float2 dir = float2(0.0, 0.0);
    float len = 0.0;
    fsr_easu_set(dir, len, pp, true,  false, false, false, bL, eL, fL, gL, jL);
    fsr_easu_set(dir, len, pp, false, true,  false, false, cL, fL, gL, hL, kL);
    fsr_easu_set(dir, len, pp, false, false, true,  false, fL, iL, jL, kL, nL);
    fsr_easu_set(dir, len, pp, false, false, false, true,  gL, jL, kL, lL, oL);

    float2 dir2 = dir * dir;
    float dirR = dir2.x + dir2.y;
    bool zro = dirR < (1.0 / 32768.0);
    dirR = rsqrt(max(dirR, 1.0 / 32768.0));
    dir = zro ? float2(1.0, 0.0) : dir * dirR;

    len = len * 0.5;
    len *= len;
    float stretch = (dir.x * dir.x + dir.y * dir.y) * rcp(max(abs(dir.x), abs(dir.y)));
    float2 len2 = float2(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    float lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    float clp = rcp(lob);

    float3 aC = float3(0.0, 0.0, 0.0);
    float aW = 0.0;
    fsr_easu_tap(aC, aW, float2( 0.0, -1.0) - pp, dir, len2, lob, clp, b);
    fsr_easu_tap(aC, aW, float2( 1.0, -1.0) - pp, dir, len2, lob, clp, c);
    fsr_easu_tap(aC, aW, float2(-1.0,  0.0) - pp, dir, len2, lob, clp, e);
    fsr_easu_tap(aC, aW, float2( 0.0,  0.0) - pp, dir, len2, lob, clp, f);
    fsr_easu_tap(aC, aW, float2( 1.0,  0.0) - pp, dir, len2, lob, clp, g);
    fsr_easu_tap(aC, aW, float2( 2.0,  0.0) - pp, dir, len2, lob, clp, h);
    fsr_easu_tap(aC, aW, float2(-1.0,  1.0) - pp, dir, len2, lob, clp, i);
    fsr_easu_tap(aC, aW, float2( 0.0,  1.0) - pp, dir, len2, lob, clp, j);
    fsr_easu_tap(aC, aW, float2( 1.0,  1.0) - pp, dir, len2, lob, clp, k);
    fsr_easu_tap(aC, aW, float2( 2.0,  1.0) - pp, dir, len2, lob, clp, l);
    fsr_easu_tap(aC, aW, float2( 0.0,  2.0) - pp, dir, len2, lob, clp, n);
    fsr_easu_tap(aC, aW, float2( 1.0,  2.0) - pp, dir, len2, lob, clp, o);

    float3 min4 = min(min(f, g), min(j, k));
    float3 max4 = max(max(f, g), max(j, k));
    float3 pix = clamp(aC / max(aW, 1e-6), min4, max4);
    easu_out[id.xy] = float4(saturate(pix), 1.0);
}
"#;

/// AMD FSR 1.0 RCAS (Robust Contrast-Adaptive Sharpening) + UI overlay compute shader.
const RCAS_COMPOSITE_SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=5, b0), DescriptorTable(SRV(t0, numDescriptors=2), UAV(u0, numDescriptors=1)), StaticSampler(s0, filter=FILTER_MIN_MAG_MIP_LINEAR, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP)"
cbuffer Params : register(b0) {
    uint w;
    uint h;
    uint premultiplied;
    uint ui_srgb;
    float rcas_con;
};
Texture2D<float4> game : register(t0);
Texture2D<float4> ui : register(t1);
RWTexture2D<float4> outp : register(u0);
SamplerState linear_clamp : register(s0);

float3 linear_to_srgb(float3 c) {
    c = saturate(c);
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(max(c, 1e-6), 1.0 / 2.4) - 0.055;
    return lerp(lo, hi, step(0.0031308, c));
}

float fsr_luma(float3 c) {
    return c.b * 0.5 + (c.r * 0.5 + c.g);
}

[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= w || id.y >= h) return;

    int2 p = int2(id.xy);
    int2 max_p = int2(int(w) - 1, int(h) - 1);
    float3 b = game.Load(int3(clamp(p + int2( 0, -1), int2(0, 0), max_p), 0)).rgb;
    float3 d = game.Load(int3(clamp(p + int2(-1,  0), int2(0, 0), max_p), 0)).rgb;
    float3 e = game.Load(int3(p, 0)).rgb;
    float3 f = game.Load(int3(clamp(p + int2( 1,  0), int2(0, 0), max_p), 0)).rgb;
    float3 h_px = game.Load(int3(clamp(p + int2( 0,  1), int2(0, 0), max_p), 0)).rgb;

    float bL = fsr_luma(b);
    float dL = fsr_luma(d);
    float eL = fsr_luma(e);
    float fL = fsr_luma(f);
    float hL = fsr_luma(h_px);

    float nz = 0.25 * (bL + dL + fL + hL) - eL;
    float mxL = max(max(bL, dL), max(eL, max(fL, hL)));
    float mnL = min(min(bL, dL), min(eL, min(fL, hL)));
    nz = saturate(abs(nz) * rcp(max(mxL - mnL, 1e-5)));
    nz = -0.5 * nz + 1.0;

    float3 mn4R = min(min(b, d), min(f, h_px));
    float3 mx4R = max(max(b, d), max(f, h_px));
    float3 mn4 = min(mn4R, e);
    float3 mx4 = max(mx4R, e);
    float3 hitMinR = mn4 / max(4.0 * mx4R, 1e-5);
    float3 hitMaxR = (1.0 - mx4) / min(4.0 * mn4R - 4.0, -1e-5);
    float3 lobeRGB = max(-hitMinR, hitMaxR);
    float lobe = max(-(0.25 - 1.0 / 16.0), min(max(lobeRGB.r, max(lobeRGB.g, lobeRGB.b)), 0.0)) * rcas_con;
    lobe *= nz;
    float rcpL = rcp(4.0 * lobe + 1.0);
    float3 g = saturate((lobe * (b + d + f + h_px) + e) * rcpL);

    float2 uv = (float2(id.xy) + 0.5) / float2(w, h);
    float4 u = ui.SampleLevel(linear_clamp, uv, 0);
    if (ui_srgb != 0) u.rgb = linear_to_srgb(u.rgb);
    float3 c = premultiplied != 0 ? g * (1.0 - u.a) + u.rgb : lerp(g, u.rgb, u.a);
    outp[id.xy] = float4(c, 1.0);
}
"#;

/// Single-pass bilinear/direct overlay when FSR is disabled or not upscaling.
const SIMPLE_COMPOSITE_SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=5, b0), DescriptorTable(SRV(t0, numDescriptors=2), UAV(u0, numDescriptors=1)), StaticSampler(s0, filter=FILTER_MIN_MAG_MIP_LINEAR, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP)"
cbuffer Params : register(b0) {
    uint w;
    uint h;
    uint premultiplied;
    uint game_srgb;
    uint ui_srgb;
};
Texture2D<float4> game : register(t0);
Texture2D<float4> ui : register(t1);
RWTexture2D<float4> outp : register(u0);
SamplerState linear_clamp : register(s0);

float3 linear_to_srgb(float3 c) {
    c = saturate(c);
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(max(c, 1e-6), 1.0 / 2.4) - 0.055;
    return lerp(lo, hi, step(0.0031308, c));
}

[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= w || id.y >= h) return;
    float2 uv = (float2(id.xy) + 0.5) / float2(w, h);
    float4 g = game.SampleLevel(linear_clamp, uv, 0);
    if (game_srgb != 0) g.rgb = linear_to_srgb(g.rgb);
    float4 u = ui.SampleLevel(linear_clamp, uv, 0);
    if (ui_srgb != 0) u.rgb = linear_to_srgb(u.rgb);
    float3 c = premultiplied != 0 ? g.rgb * (1.0 - u.a) + u.rgb : lerp(g.rgb, u.rgb, u.a);
    outp[id.xy] = float4(c, 1.0);
}
"#;

#[derive(Clone, Copy)]
struct Options {
    premultiplied: bool,
    fsr: bool,
    sharpness: f32,
}

impl Options {
    fn from_env(premultiplied_override: Option<bool>) -> Self {
        Self {
            premultiplied: effective_premultiplied(premultiplied_override),
            fsr: fsr_enabled(),
            sharpness: fsr_sharpness(),
        }
    }
}

struct Pass {
    easu_root: ID3D12RootSignature,
    easu_pso: ID3D12PipelineState,
    comp_root: ID3D12RootSignature,
    rcas_pso: ID3D12PipelineState,
    simple_pso: ID3D12PipelineState,
    views: ID3D12DescriptorHeap,
    cpu: usize,
    gpu: u64,
    step: u32,
    next: u32,
    /// Intermediate EASU output texture (`NON_PIXEL_SHADER_RESOURCE` at rest).
    easu: Option<(u32, u32, DXGI_FORMAT, ID3D12Resource)>,
    /// Final composited texture (`COPY_SOURCE` at rest).
    out: Option<(u32, u32, DXGI_FORMAT, ID3D12Resource)>,
}
// SAFETY: D3D12 objects are free-threaded; the pass is used under a lock.
unsafe impl Send for Pass {}

/// The pass: not tried yet, could not be made, or made.
static PASS: Mutex<Option<Option<Pass>>> = Mutex::new(None);

fn compile_cs(source: &str, name: windows::core::PCSTR) -> windows::core::Result<Vec<u8>> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers are ours.
    let res = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            name,
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = res {
        if let Some(err) = errors {
            // SAFETY: the blob's own bytes.
            let msg = unsafe {
                std::slice::from_raw_parts(err.GetBufferPointer().cast::<u8>(), err.GetBufferSize())
            };
            tracing::warn!(shader = ?name, error = %String::from_utf8_lossy(msg), "compose shader compile failed");
        }
        return Err(e);
    }
    let code = code.ok_or_else(windows::core::Error::empty)?;
    // SAFETY: the blob's own bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
    };
    Ok(bytes.to_vec())
}

impl Pass {
    fn new(device: &ID3D12Device) -> windows::core::Result<Self> {
        let easu_bytes = compile_cs(EASU_SOURCE, s!("present easu"))?;
        let rcas_bytes = compile_cs(RCAS_COMPOSITE_SOURCE, s!("present rcas compose"))?;
        let simple_bytes = compile_cs(SIMPLE_COMPOSITE_SOURCE, s!("present simple compose"))?;
        // SAFETY: the blobs' own bytes contain embedded root signatures; plain D3D12 creation.
        unsafe {
            let easu_root: ID3D12RootSignature = device.CreateRootSignature(0, &easu_bytes)?;
            let easu_pso: ID3D12PipelineState =
                device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&easu_root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: easu_bytes.as_ptr().cast(),
                        BytecodeLength: easu_bytes.len(),
                    },
                    ..Default::default()
                })?;

            let comp_root: ID3D12RootSignature = device.CreateRootSignature(0, &rcas_bytes)?;
            let rcas_pso: ID3D12PipelineState =
                device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&comp_root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: rcas_bytes.as_ptr().cast(),
                        BytecodeLength: rcas_bytes.len(),
                    },
                    ..Default::default()
                })?;
            let simple_pso: ID3D12PipelineState =
                device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&comp_root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: simple_bytes.as_ptr().cast(),
                        BytecodeLength: simple_bytes.len(),
                    },
                    ..Default::default()
                })?;

            let kind = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
            let views: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: kind,
                NumDescriptors: SLOTS * DESCRIPTORS_PER_SLOT,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })?;
            Ok(Self {
                cpu: views.GetCPUDescriptorHandleForHeapStart().ptr,
                gpu: views.GetGPUDescriptorHandleForHeapStart().ptr,
                step: device.GetDescriptorHandleIncrementSize(kind),
                easu_root,
                easu_pso,
                comp_root,
                rcas_pso,
                simple_pso,
                views,
                next: 0,
                easu: None,
                out: None,
            })
        }
    }

    fn record(
        &mut self,
        device: &ID3D12Device,
        list: &ID3D12GraphicsCommandList,
        game: &ID3D12Resource,
        ui: &ID3D12Resource,
        ui_state: u32,
        opts: Options,
    ) -> Option<ID3D12Resource> {
        // SAFETY: live resources; plain queries.
        let (gd, ud) = unsafe { (game.GetDesc(), ui.GetDesc()) };
        let Some((game_srv_fmt, game_srgb)) = srv_format(gd.Format) else {
            return None;
        };
        let Some((ui_srv_fmt, ui_srgb)) = srv_format(ud.Format) else {
            return None;
        };
        let (in_w, in_h) = (gd.Width as u32, gd.Height);
        let (out_w, out_h) = output_dimensions(&gd, &ud);
        let out_fmt = output_format(device, gd.Format)?;
        let upscaling = out_w > in_w || out_h > in_h;
        let use_fsr = opts.fsr && upscaling;

        if self
            .out
            .as_ref()
            .is_none_or(|o| (o.0, o.1, o.2) != (out_w, out_h, out_fmt))
        {
            let tex = create_uav_texture(
                device,
                out_w,
                out_h,
                out_fmt,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )?;
            if let Some((_, _, _, old)) = self.out.replace((out_w, out_h, out_fmt, tex)) {
                crate::host::retire(old);
            }
        }
        let out = self.out.as_ref()?.3.clone();

        let easu_tex = if use_fsr {
            let easu_fmt = easu_format(gd.Format);
            if self
                .easu
                .as_ref()
                .is_none_or(|e| (e.0, e.1, e.2) != (out_w, out_h, easu_fmt))
            {
                let tex = create_uav_texture(
                    device,
                    out_w,
                    out_h,
                    easu_fmt,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                )?;
                if let Some((_, _, _, old)) = self.easu.replace((out_w, out_h, easu_fmt, tex)) {
                    crate::host::retire(old);
                }
            }
            Some((easu_fmt, self.easu.as_ref()?.3.clone()))
        } else {
            None
        };

        static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
            tracing::info!(
                game_w = in_w,
                game_h = in_h,
                game_fmt = gd.Format.0,
                ui_w = ud.Width,
                ui_h = ud.Height,
                ui_fmt = ud.Format.0,
                out_w,
                out_h,
                out_fmt = out_fmt.0,
                fsr = use_fsr,
                sharpness = opts.sharpness,
                premultiplied = opts.premultiplied,
                "present planes: compositing interface over game"
            );
        }

        let first = (self.next % SLOTS) * DESCRIPTORS_PER_SLOT;
        self.next = self.next.wrapping_add(1);
        let cpu = |i: u32| D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.cpu + ((first + i) * self.step) as usize,
        };
        let gpu = |i: u32| D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: self.gpu + u64::from((first + i) * self.step),
        };
        let srv_desc = |format: DXGI_FORMAT| D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    ..Default::default()
                },
            },
        };
        let uav_desc = |format: DXGI_FORMAT| D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_UAV {
                    MipSlice: 0,
                    PlaneSlice: 0,
                },
            },
        };

        let read = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        let uav_state = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
        let copy_src = D3D12_RESOURCE_STATE_COPY_SOURCE;
        let ui_state = D3D12_RESOURCE_STATES(ui_state as i32);
        let same_tex = game.as_raw() == ui.as_raw();
        let t = crate::host::transition;

        if let Some((easu_fmt, easu_res)) = easu_tex {
            let easu_constants: [u32; 5] = [in_w, in_h, out_w, out_h, u32::from(game_srgb)];
            let rcas_con = (-opts.sharpness.clamp(0.0, 2.0)).exp2();
            let rcas_constants: [u32; 5] = [
                out_w,
                out_h,
                u32::from(opts.premultiplied),
                u32::from(ui_srgb),
                rcas_con.to_bits(),
            ];
            let easu_before = [
                t(game, copy_src, read),
                t(&easu_res, read, uav_state),
            ];
            let mut mid = vec![
                t(&easu_res, uav_state, read),
                t(&out, copy_src, uav_state),
            ];
            if ui_state != read && !same_tex {
                mid.push(t(ui, ui_state, read));
            }
            let mut after = vec![
                t(game, read, copy_src),
                t(&out, uav_state, copy_src),
            ];
            if ui_state != read && !same_tex {
                after.push(t(ui, read, ui_state));
            }
            // SAFETY: open list, heap slots owned by this ring slot, live resources.
            unsafe {
                device.CreateShaderResourceView(game, Some(&srv_desc(game_srv_fmt)), cpu(0));
                device.CreateUnorderedAccessView(
                    &easu_res,
                    None::<&ID3D12Resource>,
                    Some(&uav_desc(easu_fmt)),
                    cpu(1),
                );
                device.CreateShaderResourceView(&easu_res, Some(&srv_desc(easu_fmt)), cpu(2));
                device.CreateShaderResourceView(ui, Some(&srv_desc(ui_srv_fmt)), cpu(3));
                device.CreateUnorderedAccessView(
                    &out,
                    None::<&ID3D12Resource>,
                    Some(&uav_desc(out_fmt)),
                    cpu(4),
                );

                list.SetDescriptorHeaps(&[Some(self.views.clone())]);

                list.ResourceBarrier(&easu_before);
                list.SetComputeRootSignature(&self.easu_root);
                list.SetPipelineState(&self.easu_pso);
                list.SetComputeRoot32BitConstants(0, 5, easu_constants.as_ptr().cast(), 0);
                list.SetComputeRootDescriptorTable(1, gpu(0));
                list.Dispatch(out_w.div_ceil(8), out_h.div_ceil(8), 1);

                list.ResourceBarrier(&mid);
                list.SetComputeRootSignature(&self.comp_root);
                list.SetPipelineState(&self.rcas_pso);
                list.SetComputeRoot32BitConstants(0, 5, rcas_constants.as_ptr().cast(), 0);
                list.SetComputeRootDescriptorTable(1, gpu(2));
                list.Dispatch(out_w.div_ceil(8), out_h.div_ceil(8), 1);

                list.ResourceBarrier(&after);
            }
        } else {
            let constants: [u32; 5] = [
                out_w,
                out_h,
                u32::from(opts.premultiplied),
                u32::from(game_srgb),
                u32::from(ui_srgb),
            ];
            let mut before = vec![
                t(game, copy_src, read),
                t(&out, copy_src, uav_state),
            ];
            let mut after = vec![
                t(game, read, copy_src),
                t(&out, uav_state, copy_src),
            ];
            if ui_state != read && !same_tex {
                before.push(t(ui, ui_state, read));
                after.push(t(ui, read, ui_state));
            }
            // SAFETY: open list, heap slots owned by this ring slot, live resources.
            unsafe {
                device.CreateShaderResourceView(game, Some(&srv_desc(game_srv_fmt)), cpu(0));
                device.CreateShaderResourceView(ui, Some(&srv_desc(ui_srv_fmt)), cpu(1));
                device.CreateUnorderedAccessView(
                    &out,
                    None::<&ID3D12Resource>,
                    Some(&uav_desc(out_fmt)),
                    cpu(2),
                );
                list.ResourceBarrier(&before);
                list.SetDescriptorHeaps(&[Some(self.views.clone())]);
                list.SetComputeRootSignature(&self.comp_root);
                list.SetPipelineState(&self.simple_pso);
                list.SetComputeRoot32BitConstants(0, 5, constants.as_ptr().cast(), 0);
                list.SetComputeRootDescriptorTable(1, gpu(0));
                list.Dispatch(out_w.div_ceil(8), out_h.div_ceil(8), 1);
                list.ResourceBarrier(&after);
            }
        }
        Some(out)
    }
}

fn create_uav_texture(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    initial_state: D3D12_RESOURCE_STATES,
) -> Option<ID3D12Resource> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: u64::from(width),
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut tex: Option<ID3D12Resource> = None;
    // SAFETY: plain creation of a device-local texture.
    unsafe {
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                initial_state,
                None,
                &mut tex,
            )
            .ok()?;
    }
    tex
}

/// Maps a supported DXGI format to its concrete SRV format and whether it is sRGB.
pub(crate) fn srv_format(format: DXGI_FORMAT) -> Option<(DXGI_FORMAT, bool)> {
    match format.0 {
        27 | 28 => Some((DXGI_FORMAT_R8G8B8A8_UNORM, false)),
        29 => Some((DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, true)),
        87 | 90 => Some((DXGI_FORMAT_B8G8R8A8_UNORM, false)),
        91 => Some((DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, true)),
        23 | 24 => Some((DXGI_FORMAT_R10G10B10A2_UNORM, false)),
        _ => None,
    }
}

fn supports_typed_uav(device: &ID3D12Device, format: DXGI_FORMAT) -> bool {
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    // SAFETY: plain feature query on a live device.
    let ok = unsafe {
        device
            .CheckFeatureSupport(
                D3D12_FEATURE_FORMAT_SUPPORT,
                (&raw mut support).cast(),
                size_of_val(&support) as u32,
            )
            .is_ok()
    };
    ok && (support.Support1.0 & D3D12_FORMAT_SUPPORT1_TYPED_UNORDERED_ACCESS_VIEW.0 != 0)
        && (support.Support2.0 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE.0 != 0)
}

fn output_format(device: &ID3D12Device, game_format: DXGI_FORMAT) -> Option<DXGI_FORMAT> {
    let desired = match game_format.0 {
        27 | 28 | 29 => DXGI_FORMAT_R8G8B8A8_UNORM,
        87 | 90 | 91 => DXGI_FORMAT_B8G8R8A8_UNORM,
        23 | 24 => DXGI_FORMAT_R10G10B10A2_UNORM,
        _ => return None,
    };
    if desired == DXGI_FORMAT_R8G8B8A8_UNORM || supports_typed_uav(device, desired) {
        Some(desired)
    } else {
        Some(DXGI_FORMAT_R8G8B8A8_UNORM)
    }
}

fn easu_format(game_format: DXGI_FORMAT) -> DXGI_FORMAT {
    if matches!(game_format.0, 23 | 24) {
        DXGI_FORMAT_R16G16B16A16_FLOAT
    } else {
        DXGI_FORMAT_R8G8B8A8_UNORM
    }
}

fn output_dimensions(gd: &D3D12_RESOURCE_DESC, ud: &D3D12_RESOURCE_DESC) -> (u32, u32) {
    let w = (gd.Width as u32).max(ud.Width as u32).max(1);
    let h = gd.Height.max(ud.Height).max(1);
    (w, h)
}

fn premultiplied_env() -> Option<bool> {
    static ENV: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *ENV.get_or_init(|| match std::env::var("VICTORY_PLANE_PREMULTIPLIED").ok().as_deref() {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    })
}

fn effective_premultiplied(plane_override: Option<bool>) -> bool {
    premultiplied_env().or(plane_override).unwrap_or(false)
}

fn fsr_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FSR").is_none_or(|v| v != "0"))
}

fn fsr_sharpness() -> f32 {
    static SHARPNESS: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SHARPNESS.get_or_init(|| {
        std::env::var("VICTORY_FSR_SHARPNESS")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(0.2)
            .clamp(0.0, 2.0)
    })
}

fn ensure_pass<'a>(
    kept: &'a mut Option<Option<Pass>>,
    device: &ID3D12Device,
) -> Option<&'a mut Pass> {
    if kept.is_none() {
        *kept = Some(match Pass::new(device) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "present planes: the pass that lays them over each other could not be made");
                None
            }
        });
    }
    kept.as_mut()?.as_mut()
}

/// Returns the output `(width, height, format)` that [`record`] will produce
/// for `game` and `ui` when composition is supported and its pass is ready.
/// Used by `host.rs` so the swap chain matches the composed output size/format.
pub(crate) fn target(
    device: &ID3D12Device,
    game: &D3D12_RESOURCE_DESC,
    ui: &D3D12_RESOURCE_DESC,
) -> Option<(u32, u32, DXGI_FORMAT)> {
    if srv_format(game.Format).is_none() || srv_format(ui.Format).is_none() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            tracing::warn!(
                game = game.Format.0,
                ui = ui.Format.0,
                "present planes not laid over each other: a format this pass does not read"
            );
        });
        return None;
    }
    let out_fmt = output_format(device, game.Format)?;
    let mut kept = PASS.lock().unwrap_or_else(|e| e.into_inner());
    ensure_pass(&mut kept, device)?;
    let (w, h) = output_dimensions(game, ui);
    Some((w, h, out_fmt))
}

/// Records, into the present's `list`, `ui` laid over `game` (with AMD FSR1
/// EASU + RCAS upscaling when `ui` is larger than `game` and `VICTORY_FSR` is
/// enabled) into a texture of ours, and gives that texture, left in the
/// copy-source state. `game` is in the copy-source state (the present put it
/// there); `ui` is in `ui_state` and is left in it. `None` (nothing recorded)
/// if a format is unsupported or the pass could not be made.
pub(crate) fn record(
    device: &ID3D12Device,
    list: &ID3D12GraphicsCommandList,
    game: &ID3D12Resource,
    ui: &ID3D12Resource,
    ui_state: u32,
    premultiplied: Option<bool>,
) -> Option<ID3D12Resource> {
    // SAFETY: live resources; plain queries.
    let (gd, ud) = unsafe { (game.GetDesc(), ui.GetDesc()) };
    if srv_format(gd.Format).is_none() || srv_format(ud.Format).is_none() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            tracing::warn!(
                game = gd.Format.0,
                ui = ud.Format.0,
                "present planes not laid over each other: a format this pass does not read"
            );
        });
        return None;
    }
    let mut kept = PASS.lock().unwrap_or_else(|e| e.into_inner());
    let pass = ensure_pass(&mut kept, device)?;
    pass.record(device, list, game, ui, ui_state, Options::from_env(premultiplied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};

    struct WarpCompose {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        event: HANDLE,
        pass: Pass,
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl WarpCompose {
        fn new() -> Option<Self> {
            let serial = crate::host::WARP_SERIAL
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // SAFETY: plain DXGI WARP query and D3D12 test setup.
            unsafe {
                let factory: IDXGIFactory4 =
                    CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
                let adapter: IDXGIAdapter = factory.EnumWarpAdapter().ok()?;
                let desc = adapter.cast::<IDXGIAdapter1>().ok()?.GetDesc1().ok()?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                    return None;
                }
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .ok()?;
                let allocator: ID3D12CommandAllocator = device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .ok()?;
                let list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;
                let pass = Pass::new(&device).expect("FSR1 compose shaders must compile on WARP");
                Some(Self {
                    device,
                    queue,
                    allocator,
                    list,
                    fence,
                    event,
                    pass,
                    _serial: serial,
                })
            }
        }

        fn make_texture_2d(
            &self,
            width: u32,
            height: u32,
            format: DXGI_FORMAT,
            pixels_rgba_or_bgra: &[u8],
            initial_state: D3D12_RESOURCE_STATES,
        ) -> ID3D12Resource {
            let heap_def = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let tex_desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: u64::from(width),
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_NONE,
                ..Default::default()
            };
            let mut tex: Option<ID3D12Resource> = None;
            // SAFETY: plain texture creation on WARP.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &heap_def,
                        D3D12_HEAP_FLAG_NONE,
                        &tex_desc,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        None,
                        &mut tex,
                    )
                    .unwrap();
            }
            let tex = tex.unwrap();

            let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let mut row_bytes = 0u64;
            let mut total_bytes = 0u64;
            // SAFETY: plain footprint query.
            unsafe {
                self.device.GetCopyableFootprints(
                    &tex_desc,
                    0,
                    1,
                    0,
                    Some(&mut footprint),
                    None,
                    Some(&mut row_bytes),
                    Some(&mut total_bytes),
                );
            }
            let upload = self.upload_buffer(total_bytes);
            let mut mapped = std::ptr::null_mut();
            // SAFETY: upload buffer mapping and row copy.
            unsafe {
                upload.Map(0, None, Some(&mut mapped)).unwrap();
                let dst = mapped.cast::<u8>();
                let pitch = footprint.Footprint.RowPitch as usize;
                let src_row = width as usize * 4;
                for y in 0..height as usize {
                    std::ptr::copy_nonoverlapping(
                        pixels_rgba_or_bgra.as_ptr().add(y * src_row),
                        dst.add(y * pitch),
                        src_row,
                    );
                }
                upload.Unmap(0, None);

                let dst_loc = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&tex),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                };
                let src_loc = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&upload),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: footprint,
                    },
                };
                self.list.CopyTextureRegion(&dst_loc, 0, 0, 0, &src_loc, None);
                if initial_state != D3D12_RESOURCE_STATE_COPY_DEST {
                    self.list.ResourceBarrier(&[crate::host::transition(
                        &tex,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        initial_state,
                    )]);
                }
            }
            self.run_and_reset();
            drop(upload);
            tex
        }

        fn upload_buffer(&self, bytes: u64) -> ID3D12Resource {
            self.buffer(bytes, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ)
        }

        fn readback_buffer(&self, bytes: u64) -> ID3D12Resource {
            self.buffer(bytes, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST)
        }

        fn buffer(
            &self,
            bytes: u64,
            heap_type: D3D12_HEAP_TYPE,
            state: D3D12_RESOURCE_STATES,
        ) -> ID3D12Resource {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: heap_type,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: bytes,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_UNKNOWN,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            let mut buf: Option<ID3D12Resource> = None;
            // SAFETY: plain buffer creation.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        state,
                        None,
                        &mut buf,
                    )
                    .unwrap();
            }
            buf.unwrap()
        }

        fn run_and_reset(&self) {
            // SAFETY: open command list and direct queue.
            unsafe {
                self.list.Close().unwrap();
                let lists = [Some(self.list.cast::<ID3D12CommandList>().unwrap())];
                self.queue.ExecuteCommandLists(&lists);
                static NEXT_FENCE: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(1);
                let val = NEXT_FENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.queue.Signal(&self.fence, val).unwrap();
                self.fence.SetEventOnCompletion(val, self.event).unwrap();
                assert_eq!(WaitForSingleObject(self.event, INFINITE), WAIT_OBJECT_0);
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
            }
        }

        fn readback_texture(&self, tex: &ID3D12Resource) -> (u32, u32, Vec<u8>) {
            // SAFETY: live texture and open list.
            unsafe {
                let desc = tex.GetDesc();
                let w = desc.Width as u32;
                let h = desc.Height;
                let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
                let mut total_bytes = 0u64;
                self.device.GetCopyableFootprints(
                    &desc,
                    0,
                    1,
                    0,
                    Some(&mut footprint),
                    None,
                    None,
                    Some(&mut total_bytes),
                );
                let rb = self.readback_buffer(total_bytes);
                let dst = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&rb),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: footprint,
                    },
                };
                let src = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(tex),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                };
                self.list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
                self.run_and_reset();

                let mut mapped = std::ptr::null_mut();
                rb.Map(0, None, Some(&mut mapped)).unwrap();
                let pitch = footprint.Footprint.RowPitch as usize;
                let row_bytes = w as usize * 4;
                let mut out = vec![0u8; row_bytes * h as usize];
                for y in 0..h as usize {
                    std::ptr::copy_nonoverlapping(
                        mapped.cast::<u8>().add(y * pitch),
                        out.as_mut_ptr().add(y * row_bytes),
                        row_bytes,
                    );
                }
                rb.Unmap(0, None);
                (w, h, out)
            }
        }
    }

    #[test]
    fn srv_format_supports_expected_dxgi_formats() {
        assert_eq!(srv_format(DXGI_FORMAT(27)), Some((DXGI_FORMAT_R8G8B8A8_UNORM, false)));
        assert_eq!(srv_format(DXGI_FORMAT(28)), Some((DXGI_FORMAT_R8G8B8A8_UNORM, false)));
        assert_eq!(srv_format(DXGI_FORMAT(29)), Some((DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, true)));
        assert_eq!(srv_format(DXGI_FORMAT(87)), Some((DXGI_FORMAT_B8G8R8A8_UNORM, false)));
        assert_eq!(srv_format(DXGI_FORMAT(90)), Some((DXGI_FORMAT_B8G8R8A8_UNORM, false)));
        assert_eq!(srv_format(DXGI_FORMAT(91)), Some((DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, true)));
        assert_eq!(srv_format(DXGI_FORMAT(23)), Some((DXGI_FORMAT_R10G10B10A2_UNORM, false)));
        assert_eq!(srv_format(DXGI_FORMAT(24)), Some((DXGI_FORMAT_R10G10B10A2_UNORM, false)));
        assert_eq!(srv_format(DXGI_FORMAT(61)), None);
    }

    #[test]
    fn fsr1_easu_rcas_upscales_and_composites_ui_on_warp() {
        let Some(mut w) = WarpCompose::new() else {
            return;
        };
        // 8x8 game texture with sharp diagonal split (dark top-left, bright red/green bottom-right).
        let mut game_px = vec![0u8; 8 * 8 * 4];
        for y in 0..8usize {
            for x in 0..8usize {
                let i = (y * 8 + x) * 4;
                if x + y >= 7 {
                    game_px[i..i + 4].copy_from_slice(&[220, 120, 40, 255]);
                } else {
                    game_px[i..i + 4].copy_from_slice(&[10, 15, 20, 255]);
                }
            }
        }
        // 16x16 UI texture: left 2 columns opaque blue, rest transparent.
        let mut ui_px = vec![0u8; 16 * 16 * 4];
        for y in 0..16usize {
            for x in 0..16usize {
                let i = (y * 16 + x) * 4;
                if x < 2 {
                    ui_px[i..i + 4].copy_from_slice(&[0, 0, 255, 255]);
                } else if x == 2 {
                    // 50% semi-transparent white edge
                    ui_px[i..i + 4].copy_from_slice(&[255, 255, 255, 128]);
                } else {
                    ui_px[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let game = w.make_texture_2d(
            8,
            8,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            &game_px,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let ui = w.make_texture_2d(
            16,
            16,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            &ui_px,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let out = w
            .pass
            .record(
                &w.device,
                &w.list,
                &game,
                &ui,
                D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32,
                Options {
                    premultiplied: false,
                    fsr: true,
                    sharpness: 0.2,
                },
            )
            .expect("FSR compose should return an output texture");
        w.run_and_reset();

        let (ow, oh, pixels) = w.readback_texture(&out);
        assert_eq!((ow, oh), (16, 16));
        // Opaque blue UI bar at (0, 0).
        assert_eq!(&pixels[0..4], &[0, 0, 255, 255]);
        // Bright game region at (14, 14) with transparent UI stays bright orange-red.
        let bright = (14 * 16 + 14) * 4;
        assert!(pixels[bright] > 200 && pixels[bright + 1] > 100, "got {:?}", &pixels[bright..bright + 4]);
        // Dark game region at (3, 0) with transparent UI stays dark.
        let dark = 3 * 4;
        assert!(pixels[dark] < 30 && pixels[dark + 1] < 30, "got {:?}", &pixels[dark..dark + 4]);

        // Also verify bilinear fallback when fsr=false.
        let out_no_fsr = w
            .pass
            .record(
                &w.device,
                &w.list,
                &game,
                &ui,
                D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32,
                Options {
                    premultiplied: false,
                    fsr: false,
                    sharpness: 0.2,
                },
            )
            .expect("non-FSR compose should return an output texture");
        w.run_and_reset();
        let (ow2, oh2, pixels2) = w.readback_texture(&out_no_fsr);
        assert_eq!((ow2, oh2), (16, 16));
        assert_eq!(&pixels2[0..4], &[0, 0, 255, 255]);
        assert!(pixels2[bright] > 200);
    }

    #[test]
    fn bgra8_srgb_and_r10g10b10a2_formats_compose_on_warp() {
        let Some(mut w) = WarpCompose::new() else {
            return;
        };
        // B8G8R8A8_UNORM game: solid red in BGRA byte order = [B=0, G=0, R=240, A=255].
        let mut game_bgra = vec![0u8; 4 * 4 * 4];
        for px in game_bgra.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 240, 255]);
        }
        // B8G8R8A8_UNORM_SRGB ui: top half opaque green [0, 200, 0, 255], bottom half transparent.
        let mut ui_bgra_srgb = vec![0u8; 8 * 8 * 4];
        for y in 0..8usize {
            for x in 0..8usize {
                let i = (y * 8 + x) * 4;
                if y < 4 {
                    ui_bgra_srgb[i..i + 4].copy_from_slice(&[0, 200, 0, 255]);
                } else {
                    ui_bgra_srgb[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let game = w.make_texture_2d(
            4,
            4,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            &game_bgra,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let ui = w.make_texture_2d(
            8,
            8,
            DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
            &ui_bgra_srgb,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let out = w
            .pass
            .record(
                &w.device,
                &w.list,
                &game,
                &ui,
                D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32,
                Options {
                    premultiplied: false,
                    fsr: true,
                    sharpness: 0.2,
                },
            )
            .expect("BGRA8/BGRA8_SRGB compose should succeed");
        w.run_and_reset();
        let (ow, oh, px) = w.readback_texture(&out);
        assert_eq!((ow, oh), (8, 8));
        // Determine output byte order based on actual output format chosen for this device.
        let out_fmt = unsafe { out.GetDesc().Format };
        let (r_idx, g_idx, b_idx) = if out_fmt == DXGI_FORMAT_B8G8R8A8_UNORM {
            (2, 1, 0)
        } else {
            (0, 1, 2)
        };
        // Top row: UI green (~200).
        assert!(
            (i32::from(px[g_idx]) - 200).abs() <= 4 && px[r_idx] < 5 && px[b_idx] < 5,
            "expected green UI, got {:?} (fmt={})",
            &px[0..4],
            out_fmt.0
        );
        // Bottom row: game red (~240).
        let bot = (6 * 8 + 4) * 4;
        assert!(
            (i32::from(px[r_idx + bot]) - 240).abs() <= 4 && px[g_idx + bot] < 5 && px[b_idx + bot] < 5,
            "expected red game, got {:?} (fmt={})",
            &px[bot..bot + 4],
            out_fmt.0
        );

        // R10G10B10A2_UNORM (24) game + R8G8B8A8_UNORM (28) UI.
        let packed: u32 = 900 | (100 << 10) | (50 << 20) | (3 << 30);
        let mut r10_bytes = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..(4 * 4) {
            r10_bytes.extend_from_slice(&packed.to_le_bytes());
        }
        let ui_rgba = vec![0u8; 8 * 8 * 4];
        let game10 = w.make_texture_2d(
            4,
            4,
            DXGI_FORMAT_R10G10B10A2_UNORM,
            &r10_bytes,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let ui8 = w.make_texture_2d(
            8,
            8,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            &ui_rgba,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let out10 = w
            .pass
            .record(
                &w.device,
                &w.list,
                &game10,
                &ui8,
                D3D12_RESOURCE_STATE_COPY_SOURCE.0 as u32,
                Options {
                    premultiplied: false,
                    fsr: true,
                    sharpness: 0.2,
                },
            )
            .expect("R10G10B10A2_UNORM compose should succeed");
        w.run_and_reset();
        let (ow10, oh10, px10) = w.readback_texture(&out10);
        assert_eq!((ow10, oh10), (8, 8));
        assert_ne!(&px10[0..4], &[0, 0, 0, 0]);
    }
}
