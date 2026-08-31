//! Box / plane / sphere triangle soups (positions + normals + u16 indices).

pub fn mesh(
    recipe: &str,
    size: [f32; 3],
) -> Result<(Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u16>), String> {
    match recipe {
        "box" => Ok(box_mesh(size)),
        "plane" => Ok(plane_mesh(size)),
        "sphere" => Ok(sphere_mesh(size)),
        "empty" => Err("empty_has_no_triangles".into()),
        other => Err(format!("unsupported_mesh_recipe:{other}")),
    }
}

fn box_mesh(size: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u16>) {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        (
            [0.0, 0.0, 1.0],
            [[-hx, -hy, hz], [hx, -hy, hz], [hx, hy, hz], [-hx, hy, hz]],
        ),
        (
            [0.0, 0.0, -1.0],
            [[hx, -hy, -hz], [-hx, -hy, -hz], [-hx, hy, -hz], [hx, hy, -hz]],
        ),
        (
            [0.0, 1.0, 0.0],
            [[-hx, hy, -hz], [-hx, hy, hz], [hx, hy, hz], [hx, hy, -hz]],
        ),
        (
            [0.0, -1.0, 0.0],
            [[-hx, -hy, hz], [-hx, -hy, -hz], [hx, -hy, -hz], [hx, -hy, hz]],
        ),
        (
            [1.0, 0.0, 0.0],
            [[hx, -hy, hz], [hx, -hy, -hz], [hx, hy, -hz], [hx, hy, hz]],
        ),
        (
            [-1.0, 0.0, 0.0],
            [[-hx, -hy, -hz], [-hx, -hy, hz], [-hx, hy, hz], [-hx, hy, -hz]],
        ),
    ];
    let mut pos = Vec::new();
    let mut nrm = Vec::new();
    let mut idx = Vec::new();
    for (normal, corners) in faces {
        let base = pos.len() as u16;
        for p in corners {
            pos.push(p);
            nrm.push(normal);
        }
        idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (pos, nrm, idx)
}

fn plane_mesh(size: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u16>) {
    let hx = size[0].abs() * 0.5;
    let hz = size[2].abs().max(size[1].abs()) * 0.5;
    let n = [0.0, 1.0, 0.0];
    // CCW when looking down +Y so backface cull keeps the pad visible from the authored cam.
    (
        vec![[-hx, 0.0, -hz], [hx, 0.0, -hz], [hx, 0.0, hz], [-hx, 0.0, hz]],
        vec![n, n, n, n],
        vec![0, 2, 1, 0, 3, 2],
    )
}

fn sphere_mesh(size: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u16>) {
    let segs = 12u16;
    let rings = 8u16;
    let rx = size[0].abs() * 0.5;
    let ry = size[1].abs() * 0.5;
    let rz = size[2].abs() * 0.5;
    let mut pos = Vec::new();
    let mut nrm = Vec::new();
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let phi = v * std::f32::consts::PI;
        let (sp, cp) = phi.sin_cos();
        for s in 0..=segs {
            let u = s as f32 / segs as f32;
            let theta = u * std::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            let n = [st * sp, cp, ct * sp];
            pos.push([n[0] * rx, n[1] * ry, n[2] * rz]);
            nrm.push(n);
        }
    }
    let mut idx = Vec::new();
    let stride = segs + 1;
    for r in 0..rings {
        for s in 0..segs {
            let a = r * stride + s;
            let b = a + stride;
            idx.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    (pos, nrm, idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_has_two_tris() {
        let (_, _, idx) = mesh("plane", [2.0, 0.0, 2.0]).unwrap();
        assert_eq!(idx.len(), 6);
    }

    #[test]
    fn plane_winding_faces_plus_y() {
        let (pos, nrm, idx) = mesh("plane", [2.0, 0.0, 2.0]).unwrap();
        assert!(nrm[0][1] > 0.0);
        let a = pos[idx[0] as usize];
        let b = pos[idx[1] as usize];
        let c = pos[idx[2] as usize];
        let ux = b[0] - a[0];
        let uz = b[2] - a[2];
        let vx = c[0] - a[0];
        let vz = c[2] - a[2];
        let ny = uz * vx - ux * vz;
        assert!(
            ny > 0.0,
            "plane first tri must face +Y (got ny={ny}); authored cam looks from above"
        );
    }

    #[test]
    fn sphere_has_many_tris() {
        let (p, n, idx) = mesh("sphere", [1.0, 1.0, 1.0]).unwrap();
        assert_eq!(p.len(), n.len());
        assert!(idx.len() > 24);
    }
}
