//! D1 layout.pattern — positions only (boxes spawned in apply).

pub fn positions(
    pattern: &str,
    count: u32,
    origin: [f32; 3],
    spacing: f32,
    columns: u32,
    seed: u64,
) -> Result<Vec<[f32; 3]>, String> {
    if count == 0 || count > 128 {
        return Err("layout_count".into());
    }
    let n = count as usize;
    let s = spacing.max(0.05);
    let pts = match pattern {
        "linear" => (0..n)
            .map(|i| [origin[0] + i as f32 * s, origin[1], origin[2]])
            .collect(),
        "grid" => {
            let cols = columns.max(1) as usize;
            (0..n)
                .map(|i| {
                    let x = (i % cols) as f32;
                    let z = (i / cols) as f32;
                    [origin[0] + x * s, origin[1], origin[2] + z * s]
                })
                .collect()
        }
        "radial" => {
            let r = s * (n as f32).max(1.0) / std::f32::consts::TAU;
            (0..n)
                .map(|i| {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    [
                        origin[0] + r * a.cos(),
                        origin[1],
                        origin[2] + r * a.sin(),
                    ]
                })
                .collect()
        }
        "seeded_scatter" => (0..n)
            .map(|i| {
                let h = hash_u64(seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                let x = (h as f32 / u64::MAX as f32) * 2.0 - 1.0;
                let z = ((h >> 32) as f32 / u32::MAX as f32) * 2.0 - 1.0;
                [origin[0] + x * s, origin[1], origin[2] + z * s]
            })
            .collect(),
        other => return Err(format!("unknown_layout_pattern:{other}")),
    };
    Ok(pts)
}

fn hash_u64(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x.wrapping_mul(0x94d0_49bb_1331_11eb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_evenly_spaced() {
        let p = positions("linear", 3, [0.0, 0.5, 0.0], 2.0, 0, 0).unwrap();
        assert_eq!(p[0], [0.0, 0.5, 0.0]);
        assert_eq!(p[2], [4.0, 0.5, 0.0]);
    }
}
