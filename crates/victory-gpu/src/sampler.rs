//! Samplers as the host describes them, from the console's sampler
//! description (`CreateSamplerX`), which names the hardware's sampler
//! fields. (Immediate samplers in shader code come decoded with the
//! translation.)

use windows::Win32::Graphics::Direct3D12::*;

/// The fields both forms share.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fields {
    /// Point 0, bilinear 1, anisotropic point 2, anisotropic bilinear 3.
    mag: u32,
    min: u32,
    /// None 0, point 1, linear 2.
    mip: u32,
    /// Plain 0, minimum 1, maximum 2.
    mode: u32,
    /// Host address modes.
    address: [u32; 3],
    anisotropy: u32,
    /// Host comparison function.
    compare: u32,
    min_lod: f32,
    max_lod: f32,
    bias: f32,
    border: [f32; 4],
}

impl Fields {
    fn host(&self) -> D3D12_SAMPLER_DESC {
        let linear = |f: u32| u32::from(f == 1 || f == 3);
        let aniso = self.mag >= 2 || self.min >= 2;
        // The console compares when the instruction says so, with the
        // sampler's function; the host needs a comparison sampler for that.
        // A sampler given a real test (less ... greater-or-equal) is one;
        // never, always and none are plain samplers.
        const LESS: u32 = 2;
        const GREATER_EQUAL: u32 = 7;
        let compares = (LESS..=GREATER_EQUAL).contains(&self.compare);
        let reduction = match self.mode {
            1 => 2,
            2 => 3,
            _ => u32::from(compares),
        };
        let filter = if aniso {
            0x55
        } else {
            linear(self.min) << 4 | linear(self.mag) << 2 | u32::from(self.mip == 2)
        };
        // The host refuses values outside its ranges, which the title's
        // words can hold: NaN, a bias past 16 levels, an address mode it
        // has no name for.
        let min_lod = lod(self.min_lod, 0.0);
        // Without a mip filter the console samples mip 0 only.
        let max_lod = if self.mip == 0 {
            min_lod
        } else {
            lod(self.max_lod, min_lod).max(min_lod)
        };
        let address = |mode: u32| match mode {
            1..=5 => D3D12_TEXTURE_ADDRESS_MODE(mode as i32),
            _ => D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        };
        D3D12_SAMPLER_DESC {
            Filter: D3D12_FILTER((reduction << 7 | filter) as i32),
            AddressU: address(self.address[0]),
            AddressV: address(self.address[1]),
            AddressW: address(self.address[2]),
            MipLODBias: if self.bias.is_nan() {
                0.0
            } else {
                self.bias
                    .clamp(D3D12_MIP_LOD_BIAS_MIN, D3D12_MIP_LOD_BIAS_MAX)
            },
            MaxAnisotropy: if aniso {
                self.anisotropy.clamp(1, 16)
            } else {
                1
            },
            // Only a comparison sampler reads the function.
            ComparisonFunc: if reduction == 1 {
                D3D12_COMPARISON_FUNC(self.compare as i32)
            } else {
                D3D12_COMPARISON_FUNC_NEVER
            },
            BorderColor: self.border,
            MinLOD: min_lod,
            MaxLOD: max_lod,
        }
    }
}

/// A LOD bound the host takes: `fallback` for NaN, infinities clamped to
/// the largest finite float.
fn lod(value: f32, fallback: f32) -> f32 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(-D3D12_FLOAT32_MAX, D3D12_FLOAT32_MAX)
    }
}

/// The console's sampler description (27 words): flags, magnification,
/// minification, mip and depth filters, filter mode, three address modes,
/// minimum and maximum LOD, LOD bias, secondary bias, anisotropy, its bias
/// and threshold, comparison, border color, then tuning words.
pub(crate) fn console(words: &[u32; 27]) -> D3D12_SAMPLER_DESC {
    let f = |i: usize| f32::from_bits(words[i]);
    Fields {
        mag: words[1],
        min: words[2],
        mip: words[3],
        mode: words[5],
        address: [words[6], words[7], words[8]],
        min_lod: f(9),
        max_lod: f(10),
        bias: f(11),
        anisotropy: words[13],
        compare: words[16],
        border: [f(17), f(18), f(19), f(20)],
    }
    .host()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_descriptions_decode() {
        let mut w = [0u32; 27];
        w[1] = 1; // bilinear magnification
        w[2] = 0; // point minification
        w[3] = 2; // linear mips
        w[6..9].copy_from_slice(&[3, 3, 3]);
        w[10] = 1000.0f32.to_bits();
        let d = console(&w);
        assert_eq!(d.Filter.0, 0x5, "min point, mag and mip linear");
        assert_eq!(d.AddressW, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        assert_eq!(d.MaxLOD, 1000.0);
        // A less-or-equal test makes a comparison sampler; never does not.
        w[16] = 4;
        assert_eq!(console(&w).Filter.0, 0x85);
        assert_eq!(console(&w).ComparisonFunc, D3D12_COMPARISON_FUNC_LESS_EQUAL);
        w[16] = 1;
        assert_eq!(console(&w).Filter.0, 0x5);
    }

    #[test]
    fn lod_bounds_reach_the_host_finite_and_ordered() {
        let mut w = [0u32; 27];
        w[3] = 2; // linear mips
        w[9] = 4.0f32.to_bits();
        w[10] = 2.0f32.to_bits();
        // A maximum below the minimum is raised to it.
        assert_eq!((console(&w).MinLOD, console(&w).MaxLOD), (4.0, 4.0));
        // NaN: the minimum falls back to 0, the maximum to the minimum.
        w[9] = f32::NAN.to_bits();
        w[10] = f32::NAN.to_bits();
        assert_eq!((console(&w).MinLOD, console(&w).MaxLOD), (0.0, 0.0));
        // Infinities become the largest finite float.
        w[9] = f32::NEG_INFINITY.to_bits();
        w[10] = f32::INFINITY.to_bits();
        let d = console(&w);
        assert_eq!(
            (d.MinLOD, d.MaxLOD),
            (-D3D12_FLOAT32_MAX, D3D12_FLOAT32_MAX)
        );
        // Without a mip filter both are the (sanitized) minimum.
        w[3] = 0;
        w[9] = 3.0f32.to_bits();
        w[10] = 8.0f32.to_bits();
        assert_eq!((console(&w).MinLOD, console(&w).MaxLOD), (3.0, 3.0));
        w[9] = f32::NAN.to_bits();
        assert_eq!((console(&w).MinLOD, console(&w).MaxLOD), (0.0, 0.0));
    }

    #[test]
    fn bias_address_and_comparison_stay_in_host_ranges() {
        let mut w = [0u32; 27];
        w[11] = 100.0f32.to_bits();
        assert_eq!(console(&w).MipLODBias, D3D12_MIP_LOD_BIAS_MAX);
        w[11] = (-100.0f32).to_bits();
        assert_eq!(console(&w).MipLODBias, D3D12_MIP_LOD_BIAS_MIN);
        w[11] = f32::NAN.to_bits();
        assert_eq!(console(&w).MipLODBias, 0.0);
        // Address modes 1..=5 pass; others become clamp.
        w[6..9].copy_from_slice(&[0, 5, 9]);
        let d = console(&w);
        assert_eq!(d.AddressU, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        assert_eq!(d.AddressV, D3D12_TEXTURE_ADDRESS_MODE_MIRROR_ONCE);
        assert_eq!(d.AddressW, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        // A minimum or maximum reduction takes no comparison function.
        w[5] = 1;
        w[16] = 4;
        assert_eq!(console(&w).ComparisonFunc, D3D12_COMPARISON_FUNC_NEVER);
        w[16] = 99;
        w[5] = 0;
        assert_eq!(console(&w).ComparisonFunc, D3D12_COMPARISON_FUNC_NEVER);
    }
}
