use std::fmt::Write;
use std::sync::OnceLock;

use crate::{ACESCG_TO_REC709_F32, REC709_TO_ACESCG_F32};

/// The conversions as a WGSL snippet to prepend to a shader. Everything is
/// prefixed `fc_` and works on straight (non-premultiplied) `vec3<f32>`:
///
/// ```wgsl
/// fn fc_rec709_to_acescg(c: vec3<f32>) -> vec3<f32>
/// fn fc_acescg_to_rec709(c: vec3<f32>) -> vec3<f32>
/// fn fc_srgb_to_linear(v: vec3<f32>) -> vec3<f32>     fn fc_linear_to_srgb(l: vec3<f32>) -> vec3<f32>
/// fn fc_bt709_to_linear(v: vec3<f32>) -> vec3<f32>    fn fc_linear_to_bt709(l: vec3<f32>) -> vec3<f32>
/// fn fc_gamma22_to_linear(v: vec3<f32>) -> vec3<f32>  fn fc_linear_to_gamma22(l: vec3<f32>) -> vec3<f32>
/// fn fc_gamma24_to_linear(v: vec3<f32>) -> vec3<f32>  fn fc_linear_to_gamma24(l: vec3<f32>) -> vec3<f32>
/// const FC_REC709_TO_ACESCG: mat3x3<f32>   const FC_ACESCG_TO_REC709: mat3x3<f32>
/// ```
///
/// Same breakpoints and out-of-range behavior as [`Transfer`](crate::Transfer).
/// Matrices are the f32 roundings of the crate's f64 constants, printed
/// round-trip exact. GPU `pow` is not correctly rounded, so expect ~1e-6
/// relative differences from the CPU (f64) functions.
pub fn wgsl() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(build)
}

fn mat(name: &str, m: &[[f32; 3]; 3]) -> String {
    // WGSL matrices are column-major: mat3x3(col0, col1, col2).
    let col = |j: usize| format!("vec3<f32>({:?}, {:?}, {:?})", m[0][j], m[1][j], m[2][j]);
    format!("const {name} = mat3x3<f32>({}, {}, {});\n", col(0), col(1), col(2))
}

fn build() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "// {} (generated; do not edit)", crate::VERSION);
    s += &mat("FC_REC709_TO_ACESCG", &REC709_TO_ACESCG_F32);
    s += &mat("FC_ACESCG_TO_REC709", &ACESCG_TO_REC709_F32);
    s += r#"
fn fc_rec709_to_acescg(c: vec3<f32>) -> vec3<f32> { return FC_REC709_TO_ACESCG * c; }
fn fc_acescg_to_rec709(c: vec3<f32>) -> vec3<f32> { return FC_ACESCG_TO_REC709 * c; }

fn fc_srgb_to_linear(v: vec3<f32>) -> vec3<f32> {
    let hi = pow((max(v, vec3<f32>(0.04045)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, v / 12.92, v <= vec3<f32>(0.04045));
}
fn fc_linear_to_srgb(l: vec3<f32>) -> vec3<f32> {
    let hi = 1.055 * pow(max(l, vec3<f32>(0.0031308)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, l * 12.92, l <= vec3<f32>(0.0031308));
}
fn fc_bt709_to_linear(v: vec3<f32>) -> vec3<f32> {
    let hi = pow((max(v, vec3<f32>(0.081)) + 0.099) / 1.099, vec3<f32>(1.0 / 0.45));
    return select(hi, v / 4.5, v < vec3<f32>(0.081));
}
fn fc_linear_to_bt709(l: vec3<f32>) -> vec3<f32> {
    let hi = 1.099 * pow(max(l, vec3<f32>(0.018)), vec3<f32>(0.45)) - 0.099;
    return select(hi, l * 4.5, l < vec3<f32>(0.018));
}
fn fc_gamma22_to_linear(v: vec3<f32>) -> vec3<f32> { return pow(max(v, vec3<f32>(0.0)), vec3<f32>(2.2)); }
fn fc_linear_to_gamma22(l: vec3<f32>) -> vec3<f32> { return pow(max(l, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2)); }
fn fc_gamma24_to_linear(v: vec3<f32>) -> vec3<f32> { return pow(max(v, vec3<f32>(0.0)), vec3<f32>(2.4)); }
fn fc_linear_to_gamma24(l: vec3<f32>) -> vec3<f32> { return pow(max(l, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)); }
"#;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_parses_and_validates_with_naga() {
        let src = format!(
            "{}\n@compute @workgroup_size(1) fn main() {{ let c = fc_linear_to_srgb(fc_acescg_to_rec709(fc_rec709_to_acescg(fc_srgb_to_linear(vec3<f32>(0.5))))); _ = fc_bt709_to_linear(fc_linear_to_bt709(c)) + fc_gamma22_to_linear(fc_linear_to_gamma22(c)) + fc_gamma24_to_linear(fc_linear_to_gamma24(c)); }}",
            wgsl()
        );
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{}\n{src}", e.emit_to_string(&src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}\n{src}"));
    }

    #[test]
    fn matrix_literals_round_trip_exactly() {
        for (name, m) in [("FC_REC709_TO_ACESCG", REC709_TO_ACESCG_F32), ("FC_ACESCG_TO_REC709", ACESCG_TO_REC709_F32)] {
            let line = wgsl().lines().find(|l| l.starts_with(&format!("const {name} "))).unwrap();
            let vals: Vec<f32> = line
                .split("vec3<f32>(")
                .skip(1)
                .flat_map(|g| g.split(')').next().unwrap().split(", ").map(|t| t.parse::<f32>().unwrap()).collect::<Vec<_>>())
                .collect();
            assert_eq!(vals.len(), 9, "{line}");
            for (k, v) in vals.iter().enumerate() {
                let (col, row) = (k / 3, k % 3);
                assert_eq!(v.to_bits(), m[row][col].to_bits(), "{name}[{row}][{col}]");
            }
        }
    }
}
