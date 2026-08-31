//! Document → Cycles standalone XML (sidecar, not linked).

use crate::camera::{self, LookAt};
use crate::document::{Document, Entity, MaterialGraph};
use std::collections::HashMap;
use std::path::Path;

fn vsub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn vdot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn vnorm(a: [f32; 3]) -> [f32; 3] {
    let l = vdot(a, a).sqrt().max(1e-8);
    [a[0] / l, a[1] / l, a[2] / l]
}

fn qmul_vec(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let [vx, vy, vz] = v;
    let tx = 2.0 * (y * vz - z * vy);
    let ty = 2.0 * (z * vx - x * vz);
    let tz = 2.0 * (x * vy - y * vx);
    [
        vx + w * tx + (y * tz - z * ty),
        vy + w * ty + (z * tx - x * tz),
        vz + w * tz + (x * ty - y * tx),
    ]
}

fn world_trs(
    entities: &[Entity],
    e: &Entity,
    cache: &mut HashMap<String, ([f32; 3], [f32; 4], [f32; 3])>,
) -> ([f32; 3], [f32; 4], [f32; 3]) {
    if let Some(v) = cache.get(&e.id) {
        return *v;
    }
    let mut t = e.transform.translation;
    let mut r = e.transform.rotation;
    let mut s = e.transform.scale;
    if let Some(pid) = e.parent.as_deref() {
        if let Some(p) = entities.iter().find(|x| x.id == pid) {
            let (pt, pr, ps) = world_trs(entities, p, cache);
            let local = [t[0] * ps[0], t[1] * ps[1], t[2] * ps[2]];
            let w = qmul_vec(pr, local);
            t = [pt[0] + w[0], pt[1] + w[1], pt[2] + w[2]];
            let [px, py, pz, pw] = pr;
            let [lx, ly, lz, lw] = r;
            r = [
                pw * lx + px * lw + py * lz - pz * ly,
                pw * ly - px * lz + py * lw + pz * lx,
                pw * lz + px * ly - py * lx + pz * lw,
                pw * lw - px * lx - py * ly - pz * lz,
            ];
            s = [ps[0] * s[0], ps[1] * s[1], ps[2] * s[2]];
        }
    }
    cache.insert(e.id.clone(), (t, r, s));
    (t, r, s)
}

fn q_axis_angle(q: [f32; 4]) -> (f32, f32, f32, f32) {
    let [x, y, z, w] = q;
    let n = (x * x + y * y + z * z).sqrt();
    if n < 1e-8 {
        return (0.0, 1.0, 0.0, 0.0);
    }
    let angle = 2.0 * n.atan2(w).to_degrees();
    (angle, x / n, y / n, z / n)
}

fn shader_color(e: &Entity, graphs: &[MaterialGraph]) -> (f32, f32, f32, f32, f32) {
    if let Some(gid) = e.graph_id.as_deref() {
        if let Some(g) = graphs.iter().find(|g| g.id == gid) {
            if let Some(n) = g.nodes.iter().find(|n| n.type_name == "principled") {
                let sock = |k, d| n.sockets.get(k).copied().unwrap_or(d);
                return (
                    sock("base_color_r", 0.8),
                    sock("base_color_g", 0.8),
                    sock("base_color_b", 0.8),
                    sock("roughness", 0.5),
                    sock("metallic", 0.0),
                );
            }
        }
    }
    let c = e.material.color;
    (c[0], c[1], c[2], 0.45, 0.0)
}

fn cam_xml(look: &LookAt) -> String {
    let lookv = vnorm(vsub(look.target, look.eye));
    let pitch = lookv[1].clamp(-1.0, 1.0).asin().to_degrees();
    let yaw = (-lookv[0]).atan2(-lookv[2]).to_degrees();
    let fov = look.fov_y_deg.to_radians();
    format!(
        "<transform translate=\"{} {} {}\">\n  <transform rotate=\"{yaw:.5} 0 1 0\">\n    <transform rotate=\"{pitch:.5} 1 0 0\" scale=\"1 1 -1\">\n      <camera type=\"perspective\" fov=\"{fov:.6}\" />\n    </transform>\n  </transform>\n</transform>",
        look.eye[0], look.eye[1], look.eye[2]
    )
}

pub fn cycles_root() -> String {
    if let Ok(p) = std::env::var("TF_CYCLES_ROOT") {
        return p;
    }
    let here = std::env::current_dir().unwrap_or_default();
    let bundled = here.join("third_party").join("cycles");
    if bundled.is_dir() {
        return bundled.display().to_string();
    }
    bundled.display().to_string()
}

pub fn write_document_xml(
    doc: &Document,
    dest: impl AsRef<Path>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let cube = "../examples/objects/cube.xml";
    let sphere = "../examples/objects/sphere.xml";
    let entities: Vec<Entity> = doc.entities().cloned().collect();
    let look = camera::LookAt::authored_default();
    let mut lines = vec![
        "<cycles>".into(),
        "<integrator min_bounce=\"0\" max_bounce=\"8\" />".into(),
        format!("<camera width=\"{width}\" height=\"{height}\" />"),
        cam_xml(&look),
        "<background>".into(),
        "  <sky_texture name=\"sky\" sky_type=\"hosek_wilkie\" />".into(),
        "  <background name=\"bg\" strength=\"4.0\" />".into(),
        "  <connect from=\"sky color\" to=\"bg color\" />".into(),
        "  <connect from=\"bg background\" to=\"output surface\" />".into(),
        "</background>".into(),
        "<shader name=\"sun\">".into(),
        "  <emission name=\"e\" color=\"1.0 0.95 0.85\" strength=\"4.0\" />".into(),
        "  <connect from=\"e emission\" to=\"output surface\" />".into(),
        "</shader>".into(),
        "<state shader=\"sun\">".into(),
        "  <light light_type=\"sun\" angle=\"0.02\" strength=\"1 1 1\" />".into(),
        "</state>".into(),
    ];
    let mut cache = HashMap::new();
    for e in &entities {
        let recipe = e.mesh.recipe.as_str();
        if recipe == "empty" {
            continue;
        }
        let (cr, cg, cb, rough, metal) = shader_color(e, &doc.graphs);
        let sid = format!("sh_{}", e.id.replace('-', "_"));
        lines.push(format!("<shader name=\"{sid}\">"));
        lines.push(format!(
            "  <principled_bsdf name=\"p\" base_color=\"{cr:.4} {cg:.4} {cb:.4}\" roughness=\"{rough:.4}\" metallic=\"{metal:.4}\" />"
        ));
        lines.push("  <connect from=\"p bsdf\" to=\"output surface\" />".into());
        lines.push("</shader>".into());
        let (t, r, s) = world_trs(&entities, e, &mut cache);
        let (ang, ax, ay, az) = q_axis_angle(r);
        match recipe {
            "plane" => {
                let hx = e.mesh.size[0] * s[0] * 0.5;
                let hz = e.mesh.size[2] * s[2] * 0.5;
                lines.push(format!(
                    "<transform translate=\"{:.5} {:.5} {:.5}\" rotate=\"{ang:.4} {ax:.5} {ay:.5} {az:.5}\">",
                    t[0], t[1], t[2]
                ));
                lines.push(format!("  <state shader=\"{sid}\">"));
                lines.push(format!(
                    "    <mesh P=\"{} 0 {}  {} 0 {}  {} 0 {}  {} 0 {}\" nverts=\"4\" verts=\"0 1 2 3\" />",
                    -hx, hz, hx, hz, hx, -hz, -hx, -hz
                ));
                lines.push("  </state>".into());
                lines.push("</transform>".into());
            }
            "box" | "sphere" => {
                let src = if recipe == "box" { &cube } else { &sphere };
                let (sx, sy, sz) = if recipe == "sphere" {
                    let m = e.mesh.size[0].max(e.mesh.size[1]).max(e.mesh.size[2])
                        * s[0].max(s[1]).max(s[2])
                        * 0.5;
                    (m, m, m)
                } else {
                    (
                        e.mesh.size[0] * s[0] * 0.5,
                        e.mesh.size[1] * s[1] * 0.5,
                        e.mesh.size[2] * s[2] * 0.5,
                    )
                };
                lines.push(format!(
                    "<transform translate=\"{:.5} {:.5} {:.5}\" rotate=\"{ang:.4} {ax:.5} {ay:.5} {az:.5}\" scale=\"{sx:.5} {sy:.5} {sz:.5}\">",
                    t[0], t[1], t[2]
                ));
                lines.push(format!("  <state interpolation=\"smooth\" shader=\"{sid}\">"));
                lines.push(format!("    <include src=\"{src}\" />"));
                lines.push("  </state>".into());
                lines.push("</transform>".into());
            }
            _ => {}
        }
    }
    lines.push("</cycles>".into());
    if let Some(dir) = dest.as_ref().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(dest.as_ref(), lines.join("\n") + "\n")
        .map_err(|e| format!("cycles_xml_write:{e}"))?;
    Ok(())
}
