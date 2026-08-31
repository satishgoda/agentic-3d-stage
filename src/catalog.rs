//! Compact live op index. Honesty: graphs evaluate at draw (not WGSL compile); RTX is not active.

use serde_json::{json, Value};

pub fn live() -> Value {
    json!({
        "apply": [
            "create_mesh",
            "patch_color",
            "patch_translation",
            "patch_rotation",
            "layout_pattern",
            "group",
            "ungroup",
            "graph_create",
            "graph_patch",
            "graph_bind",
            "patch_light"
        ],
        "query": [
            "left_of",
            "on_screen",
            "color_of",
            "assembly_of",
            "pixel",
            "elements"
        ],
        "project": ["list", "save", "open", "create"],
        "graphs": {
            "compiledToGpu": false,
            "evaluatedAtDraw": true,
            "nodeTypes": {
                "principled": ["base_color_r", "base_color_g", "base_color_b", "roughness", "metallic", "transmission"]
            }
        },
        "not": [
            "rtx.active",
            "play.gameplay",
            "jobs",
            "eval",
            "whole_document_replace",
            "vertex_meshElements",
            "gltf.animation"
        ],
        "export": {
            "gltf": true,
            "animation": false,
            "rtx": false
        },
        "import": {
            "gltf": true,
            "requiresExtras": true
        },
        "lighting": {
            "model": "lambert+spec",
            "shadows": "directionalMap",
            "rtx": false
        },
        "rtx": {
            "supported": false,
            "requested": false,
            "configured": false,
            "building": false,
            "active": false,
            "stale": false,
            "failed": false
        },
        "notes": "directional shadow map (not RTX); graph sockets evaluate at draw; Cycles stream is a sidecar (Ctrl+Shift+C)"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_does_not_advertise_rtx_active_or_whole_replace() {
        let c = live();
        let not: Vec<&str> = c["not"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(not.contains(&"rtx.active"));
        assert!(not.contains(&"whole_document_replace"));
        assert!(c["rtx"]["active"].as_bool() == Some(false));
        assert!(c["graphs"]["compiledToGpu"].as_bool() == Some(false));
        assert!(c["graphs"]["evaluatedAtDraw"].as_bool() == Some(true));
        assert!(c["export"]["gltf"].as_bool() == Some(true));
        assert!(c["import"]["gltf"].as_bool() == Some(true));
        assert!(c["lighting"]["rtx"].as_bool() == Some(false));
        assert_eq!(c["lighting"]["shadows"], "directionalMap");
        assert!(c["export"]["animation"].as_bool() == Some(false));
        assert!(c["apply"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "graph_patch"));
    }
}
