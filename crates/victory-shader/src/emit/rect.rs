//! Rectangle lists on the host. The console draws a rectangle from three
//! corners, the second between the other two, and makes the fourth itself
//! from the vertex shader's outputs: `v0 + v2 - v1`. The host has no such
//! primitive; [`rectangle_gs`] makes a geometry shader that does the same
//! between a translated vertex shader and its pixel shader. It takes each
//! rectangle as a triangle and emits the strip `v0, v1, v3, v2`, which
//! keeps the triangle's winding and covers the rectangle.
//!
//! Grand Theft Auto VI's world draws its screen-space passes (hundreds of
//! pipelines, one rectangle each, positions from the vertex index) as
//! indirect rectangle lists, which the host could not expand on the CPU.

use super::bitcode::{Attr, Bin, Term};
use super::shader::Shader;
use super::{HostShader, signature};
use crate::ShaderError;
use crate::dxbc::{Container, FourCC};

/// A geometry shader expanding each triangle `v0, v1, v2` of `vs`'s outputs
/// into the rectangle with the fourth corner `v0 + v2 - v1` (integer
/// outputs, which do not interpolate, take `v0`'s values).
pub fn rectangle_gs(vs: &HostShader) -> Result<Vec<u8>, ShaderError> {
    let c = Container::parse(&vs.dxil)?;
    let osg1 = c
        .part(FourCC::OSG1)
        .ok_or(ShaderError::Malformed("a vertex shader without outputs"))?;
    let mut elements = signature(osg1.data)?;
    if elements.iter().any(|e| e.row.is_none()) {
        return Err(ShaderError::Unsupported {
            offset: 0,
            what: "a rectangle-list output without a register",
        });
    }
    for e in &mut elements {
        e.usage = (1 << e.cols) - 1;
    }
    let mut s = Shader::new(false);
    s.gs = true;
    s.inputs = elements.clone();
    s.outputs = elements.clone();
    let b = s.entry;
    let void = s.t.void;
    // Each element's columns, at each of the three corners.
    let mut corners = Vec::new();
    for (id, e) in elements.iter().enumerate() {
        let float = e.comp == 9;
        let (suffix, ty) = if float {
            (".f32", s.t.f32)
        } else {
            (".i32", s.t.i32)
        };
        for col in 0..e.cols {
            let mut at = [None; 3];
            for (vertex, slot) in at.iter_mut().enumerate() {
                let args = [s.m.i32(id as u32), s.m.i32(0), s.m.i8(col), s.m.i32(vertex as u32)];
                *slot = Some(s.op(b, &format!("loadInput{suffix}"), Attr::ReadNone, ty, 4, &args));
            }
            let [v0, v1, v2] = at.map(|v| v.expect("loaded"));
            let v3 = if float {
                let sum = s.m.bin(b, Bin::Add, v0, v2);
                s.m.bin(b, Bin::Sub, sum, v1)
            } else {
                v0
            };
            corners.push((id as u32, col, suffix, [v0, v1, v3, v2]));
        }
    }
    let stream = s.m.i8(0);
    for corner in 0..4 {
        for &(id, col, suffix, values) in &corners {
            let args = [s.m.i32(id), s.m.i32(0), s.m.i8(col), values[corner]];
            s.op(b, &format!("storeOutput{suffix}"), Attr::NoUnwind, void, 5, &args);
        }
        s.op(b, "emitStream", Attr::NoUnwind, void, 97, &[stream]);
    }
    s.op(b, "cutStream", Attr::NoUnwind, void, 98, &[stream]);
    s.m.terminate(b, Term::Ret);
    Ok(s.finish())
}

/// Diagnostics: a pixel shader writing opaque white to `SV_Target0` and
/// nothing else, to tell a pass that covers no pixels from one whose
/// pixel shaders write nothing.
pub fn white_ps() -> Vec<u8> {
    use super::container::SigElement;
    let mut s = Shader::new(true);
    let b = s.entry;
    let void = s.t.void;
    let one = s.m.f32(1.0f32.to_bits());
    for col in 0..4 {
        let args = [s.m.i32(0), s.m.i32(0), s.m.i8(col), one];
        s.op(b, "storeOutput.f32", Attr::NoUnwind, void, 5, &args);
    }
    s.outputs = vec![SigElement {
        name: "SV_Target".into(),
        index: 0,
        kind: 16,
        comp: 9,
        interp: 0,
        row: Some(0),
        col: 0,
        cols: 4,
        usage: 0xf,
        half: false,
    }];
    s.m.terminate(b, Term::Ret);
    s.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::container::SigElement;
    use crate::emit::validator;

    /// A vertex shader with a position and a texture coordinate, as the
    /// stub generator makes one.
    fn vertex_shader() -> HostShader {
        let el = |name: &str, kind, row, cols| SigElement {
            name: name.into(),
            index: 0,
            kind,
            comp: 9,
            interp: 0,
            row: Some(row),
            col: 0,
            cols,
            usage: 0,
            half: false,
        };
        super::super::host::stub_vertex(vec![el("SV_Position", 3, 0, 4), el("TEXCOORD", 0, 1, 2)])
    }

    #[test]
    fn the_white_pixel_shader_validates() {
        match validator::Validator::load(validator::Validator::default_path().as_deref()) {
            Ok(mut v) => v.validate(&mut white_ps()).expect("DXIL validation"),
            Err(_) => eprintln!("no dxil.dll; validation skipped"),
        }
    }

    #[test]
    fn rectangle_geometry_shaders_validate() {
        let gs = rectangle_gs(&vertex_shader()).expect("made");
        let c = Container::parse(&gs).unwrap();
        assert!(c.part(FourCC::ISG1).is_some() && c.part(FourCC::OSG1).is_some());
        match validator::Validator::load(validator::Validator::default_path().as_deref()) {
            Ok(mut v) => v.validate(&mut gs.clone()).expect("DXIL validation"),
            Err(_) => eprintln!("no dxil.dll; validation skipped"),
        }
    }
}
