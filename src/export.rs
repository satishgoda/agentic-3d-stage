//! Static glTF 2.0 export (boxes/planes/spheres + group nodes). No animation, no RTX.

use crate::document::Document;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

pub const DEFAULT_PATH: &str = "sits/export.gltf";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub mesh_count: usize,
    pub node_count: usize,
    pub byte_length: usize,
}

pub fn write_gltf(doc: &Document, path: impl AsRef<Path>) -> Result<ExportResult, String> {
    let path = path.as_ref();
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).map_err(|e| format!("export_mkdir:{e}"))?;
        }
    }
    let v = to_gltf(doc)?;
    let text = serde_json::to_string_pretty(&v).map_err(|e| format!("export_json:{e}"))?;
    fs::write(path, text).map_err(|e| format!("export_write:{e}"))?;
    Ok(ExportResult {
        path: path.display().to_string(),
        mesh_count: v["meshes"].as_array().map(|a| a.len()).unwrap_or(0),
        node_count: v["nodes"].as_array().map(|a| a.len()).unwrap_or(0),
        byte_length: v["buffers"][0]["byteLength"].as_u64().unwrap_or(0) as usize,
    })
}

pub fn to_gltf(doc: &Document) -> Result<Value, String> {
    let entities: Vec<_> = doc.entities().collect();
    let index_of: std::collections::HashMap<&str, usize> = entities
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.as_str(), i))
        .collect();

    let mut bin = Vec::new();
    let mut buffer_views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut materials = Vec::new();
    let mut nodes = Vec::new();
    let mut mesh_for_entity = vec![None; entities.len()];

    for (i, e) in entities.iter().enumerate() {
        if e.mesh.recipe == "empty" || e.kind == "group" {
            continue;
        }
        let (pos, nrm, idx) = crate::geom::mesh(&e.mesh.recipe, e.mesh.size)?;
        let pos_off = bin.len();
        for p in &pos {
            bin.extend_from_slice(&p[0].to_le_bytes());
            bin.extend_from_slice(&p[1].to_le_bytes());
            bin.extend_from_slice(&p[2].to_le_bytes());
        }
        pad4(&mut bin);
        let nrm_off = bin.len();
        for n in &nrm {
            bin.extend_from_slice(&n[0].to_le_bytes());
            bin.extend_from_slice(&n[1].to_le_bytes());
            bin.extend_from_slice(&n[2].to_le_bytes());
        }
        pad4(&mut bin);
        let idx_off = bin.len();
        for x in &idx {
            bin.extend_from_slice(&x.to_le_bytes());
        }
        pad4(&mut bin);

        let pos_view = buffer_views.len();
        buffer_views.push(json!({
            "buffer": 0,
            "byteOffset": pos_off,
            "byteLength": pos.len() * 12,
            "target": 34962
        }));
        let nrm_view = buffer_views.len();
        buffer_views.push(json!({
            "buffer": 0,
            "byteOffset": nrm_off,
            "byteLength": nrm.len() * 12,
            "target": 34962
        }));
        let idx_view = buffer_views.len();
        buffer_views.push(json!({
            "buffer": 0,
            "byteOffset": idx_off,
            "byteLength": idx.len() * 2,
            "target": 34963
        }));

        let (pmin, pmax) = bounds(&pos);
        let pos_acc = accessors.len();
        accessors.push(json!({
            "bufferView": pos_view,
            "componentType": 5126,
            "count": pos.len(),
            "type": "VEC3",
            "min": pmin,
            "max": pmax
        }));
        let nrm_acc = accessors.len();
        accessors.push(json!({
            "bufferView": nrm_view,
            "componentType": 5126,
            "count": nrm.len(),
            "type": "VEC3"
        }));
        let idx_acc = accessors.len();
        accessors.push(json!({
            "bufferView": idx_view,
            "componentType": 5123,
            "count": idx.len(),
            "type": "SCALAR"
        }));

        let surf = doc.resolved_surface(e);
        let mat_i = materials.len();
        materials.push(json!({
            "name": e.id,
            "pbrMetallicRoughness": {
                "baseColorFactor": surf.color,
                "metallicFactor": surf.metallic,
                "roughnessFactor": surf.roughness
            }
        }));
        let mesh_i = meshes.len();
        meshes.push(json!({
            "name": e.id,
            "primitives": [{
                "attributes": { "POSITION": pos_acc, "NORMAL": nrm_acc },
                "indices": idx_acc,
                "material": mat_i
            }]
        }));
        mesh_for_entity[i] = Some(mesh_i);
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); entities.len()];
    let mut roots = Vec::new();
    for (i, e) in entities.iter().enumerate() {
        match e.parent.as_deref().and_then(|p| index_of.get(p).copied()) {
            Some(pi) => children[pi].push(i),
            None => roots.push(i),
        }
    }

    for (i, e) in entities.iter().enumerate() {
        let mut node = json!({
            "name": e.id,
            "translation": e.transform.translation,
            "rotation": quat_xyzw_to_gltf(e.transform.rotation),
            "scale": e.transform.scale
        });
        if let Some(mi) = mesh_for_entity[i] {
            node["mesh"] = json!(mi);
        }
        if !children[i].is_empty() {
            node["children"] = json!(children[i]);
        }
        node["extras"] = json!({
            "thinnerFloor": {
                "recipe": e.mesh.recipe,
                "size": e.mesh.size,
                "kind": e.kind,
                "graphId": e.graph_id,
            }
        });
        nodes.push(node);
    }

    let b64 = base64_encode(&bin);
    Ok(json!({
        "asset": {
            "version": "2.0",
            "generator": "thinner-floor",
            "extras": { "thinnerFloor": { "graphs": doc.graphs } }
        },
        "scene": 0,
        "scenes": [{ "nodes": roots }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": buffer_views,
        "buffers": [{
            "byteLength": bin.len(),
            "uri": format!("data:application/octet-stream;base64,{b64}")
        }]
    }))
}

fn quat_xyzw_to_gltf(q: [f32; 4]) -> [f32; 4] {
    // glam/document is xyzw; glTF is xyzw too (xyzw).
    q
}

fn bounds(pos: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for p in pos {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    (min, max)
}

fn pad4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub path: String,
    pub entity_count: usize,
    pub graph_count: usize,
    pub revision: u64,
}

pub fn read_gltf(path: impl AsRef<Path>) -> Result<Document, String> {
    let text = fs::read_to_string(path.as_ref()).map_err(|e| format!("import_read:{e}"))?;
    from_gltf(&text)
}

pub fn from_gltf(text: &str) -> Result<Document, String> {
    use crate::document::{Entity, Material, MaterialGraph, MeshRecipe, Scene, Transform};
    let v: Value = serde_json::from_str(text).map_err(|e| format!("import_json:{e}"))?;
    let gen = v["asset"]["generator"].as_str().unwrap_or("");
    if gen != "thinner-floor" && v["asset"]["extras"]["thinnerFloor"].is_null() {
        return Err("import_not_thinner_floor".into());
    }
    let graphs: Vec<MaterialGraph> = serde_json::from_value(
        v["asset"]["extras"]["thinnerFloor"]["graphs"].clone(),
    )
    .unwrap_or_default();
    let nodes = v["nodes"].as_array().cloned().unwrap_or_default();
    let materials = v["materials"].as_array().cloned().unwrap_or_default();
    let mut entities: Vec<Entity> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let extras = &n["extras"]["thinnerFloor"];
        if extras.is_null() {
            return Err(format!("import_missing_extras:node_{i}"));
        }
        let id = n["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("node-{i}"));
        let recipe = extras["recipe"]
            .as_str()
            .unwrap_or("box")
            .to_string();
        let size = json_vec3(&extras["size"]).unwrap_or([1.0, 1.0, 1.0]);
        let kind = extras["kind"]
            .as_str()
            .unwrap_or(if recipe == "empty" { "group" } else { "mesh" })
            .to_string();
        let graph_id = extras["graphId"].as_str().map(|s| s.to_string());
        let translation = json_vec3(&n["translation"]).unwrap_or([0.0, 0.0, 0.0]);
        let rotation = json_vec4(&n["rotation"]).unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let scale = json_vec3(&n["scale"]).unwrap_or([1.0, 1.0, 1.0]);
        let color = n
            .get("mesh")
            .and_then(|m| m.as_u64())
            .and_then(|mi| v["meshes"].get(mi as usize))
            .and_then(|mesh| mesh["primitives"][0]["material"].as_u64())
            .and_then(|mati| materials.get(mati as usize))
            .and_then(|mat| json_vec4(&mat["pbrMetallicRoughness"]["baseColorFactor"]))
            .unwrap_or([0.8, 0.8, 0.8, 1.0]);
        entities.push(Entity {
            id,
            kind,
            transform: Transform {
                translation,
                rotation,
                scale,
            },
            mesh: MeshRecipe { recipe, size },
            material: Material { color },
            parent: None,
            graph_id,
        });
    }
    for (i, n) in nodes.iter().enumerate() {
        if let Some(kids) = n["children"].as_array() {
            let pid = entities[i].id.clone();
            for k in kids {
                let ki = k.as_u64().ok_or_else(|| "import_bad_child".to_string())? as usize;
                if ki < entities.len() {
                    entities[ki].parent = Some(pid.clone());
                }
            }
        }
    }
    Ok(Document {
        revision: 1,
        scenes: vec![Scene {
            id: "main".into(),
            entities,
        }],
        idempotency_log: Vec::new(),
        graphs,
        light: crate::light::SceneLight::default(),
    })
}

fn json_vec3(v: &Value) -> Option<[f32; 3]> {
    let a = v.as_array()?;
    if a.len() != 3 {
        return None;
    }
    Some([
        a[0].as_f64()? as f32,
        a[1].as_f64()? as f32,
        a[2].as_f64()? as f32,
    ])
}

fn json_vec4(v: &Value) -> Option<[f32; 4]> {
    let a = v.as_array()?;
    if a.len() != 4 {
        return None;
    }
    Some([
        a[0].as_f64()? as f32,
        a[1].as_f64()? as f32,
        a[2].as_f64()? as f32,
        a[3].as_f64()? as f32,
    ])
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i];
        let b1 = if i + 1 < data.len() { data[i + 1] } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] } else { 0 };
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if i + 1 < data.len() {
            out.push(T[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < data.len() {
            out.push(T[(b2 & 63) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    #[test]
    fn bootstrap_gltf_has_two_meshes_and_scene() {
        let doc = Document::bootstrap();
        let g = to_gltf(&doc).unwrap();
        assert_eq!(g["asset"]["version"], "2.0");
        assert_eq!(g["meshes"].as_array().unwrap().len(), 2);
        assert!(!g["scenes"][0]["nodes"].as_array().unwrap().is_empty());
        assert!(g["buffers"][0]["uri"].as_str().unwrap().starts_with("data:"));
    }

    #[test]
    fn write_gltf_creates_file() {
        let dir = std::env::temp_dir().join(format!("tf-gltf-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.gltf");
        let r = write_gltf(&Document::bootstrap(), &path).unwrap();
        assert_eq!(r.mesh_count, 2);
        assert!(r.node_count >= 2);
        assert!(r.byte_length > 0);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"POSITION\""));
        assert!(text.contains("thinnerFloor"));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn gltf_roundtrip_preserves_ids_and_graphs() {
        use crate::document::{ApplyChange, ApplyRequest};
        let mut doc = Document::bootstrap();
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 1,
                idempotency_key: "gc".into(),
                label: "g".into(),
                changes: vec![
                    ApplyChange::GraphCreate {
                        graph_id: "mat-1".into(),
                    },
                    ApplyChange::GraphBind {
                        graph_id: "mat-1".into(),
                        entity_id: "box-1".into(),
                    },
                ],
                dry_run: false,
            })
            .ok);
        let g = to_gltf(&doc).unwrap();
        let text = serde_json::to_string(&g).unwrap();
        let back = from_gltf(&text).unwrap();
        assert_eq!(back.entity_count(), 2);
        assert_eq!(back.find_entity("box-1").unwrap().graph_id.as_deref(), Some("mat-1"));
        assert_eq!(back.graphs.len(), 1);
        assert_eq!(back.graphs[0].id, "mat-1");
        let s = back.resolved_surface(back.find_entity("box-1").unwrap());
        assert!((s.color[0] - 0.86).abs() < 1e-4);
    }
}
