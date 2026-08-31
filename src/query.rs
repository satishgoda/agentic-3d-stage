//! Spatial / color look queries over the live document (camera-aware).

use crate::camera::{self, DEFAULT_ASPECT};
use crate::document::{Document, Entity};
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

/// Entities smaller than this (max mesh extent × scale) are treated as bits/noise
/// and dropped from query hits unless the caller lowers the floor.
pub const DEFAULT_MIN_EXTENT: f32 = 0.35;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum QuerySpec {
    /// Entities to the camera-left of `entity_id` (not world −X).
    LeftOf {
        #[serde(rename = "entityId")]
        entity_id: String,
        #[serde(default = "default_min_extent")]
        min_extent: f32,
    },
    /// Entities whose center projects inside the current authoring frustum.
    OnScreen {
        #[serde(default = "default_min_extent")]
        min_extent: f32,
        #[serde(default = "default_aspect")]
        aspect: f32,
    },
    /// Match a named color or RGBA (small RGB distance tolerance).
    ColorOf {
        color: ColorQuery,
        #[serde(default = "default_min_extent")]
        min_extent: f32,
        #[serde(default = "default_tolerance")]
        tolerance: f32,
    },
    /// Name-prefix assembly: the entity itself plus every id that starts with `{id}-`.
    /// No parent field, no min_extent filter (smileys/spikes/parked hair all count).
    AssemblyOf {
        #[serde(rename = "entityId")]
        entity_id: String,
    },
    /// What entity covers this beauty-pixel (objectId pass).
    Pixel {
        x: u32,
        y: u32,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
    /// D4: entity filters (not vertex meshElements — we only have boxes).
    Elements {
        #[serde(default, rename = "bboxMin")]
        bbox_min: Option<[f32; 3]>,
        #[serde(default, rename = "bboxMax")]
        bbox_max: Option<[f32; 3]>,
        #[serde(default, rename = "yMin")]
        y_min: Option<f32>,
        #[serde(default, rename = "yMax")]
        y_max: Option<f32>,
        #[serde(default, rename = "notAdjacentTo")]
        not_adjacent_to: Option<String>,
        #[serde(default = "default_min_extent")]
        min_extent: f32,
    },
}

fn default_min_extent() -> f32 {
    DEFAULT_MIN_EXTENT
}
fn default_aspect() -> f32 {
    DEFAULT_ASPECT
}
fn default_tolerance() -> f32 {
    0.32
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ColorQuery {
    Name(String),
    Rgba([f32; 4]),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueryHit {
    pub id: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occluded: Option<bool>,
}

impl QueryHit {
    pub fn new(id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            reason: reason.into(),
            screen: None,
            depth: None,
            occluded: None,
        }
    }

    fn with_projection(mut self, ndc: Vec3, occluded: bool) -> Self {
        self.screen = Some([(ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5]);
        self.depth = Some(ndc.z);
        self.occluded = Some(occluded);
        self
    }
}

fn project_ndc(view_proj: Mat4, p: Vec3) -> Option<Vec3> {
    let clip = view_proj * p.extend(1.0);
    if clip.w.abs() < 1e-6 {
        return None;
    }
    Some(clip.truncate() / clip.w)
}

fn occluded_by_others(doc: &Document, self_id: &str, ndc: Vec3, aspect: f32) -> bool {
    let view_proj = camera::view_proj_matrix(aspect);
    for entity in doc.entities() {
        if entity.id == self_id {
            continue;
        }
        let Some(other) = project_ndc(view_proj, world_center(doc, entity)) else {
            continue;
        };
        let dx = other.x - ndc.x;
        let dy = other.y - ndc.y;
        let closer = other.z < ndc.z - 0.01;
        if closer && dx * dx + dy * dy < 0.15 * 0.15 {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub ok: bool,
    pub query: String,
    pub hits: Vec<QueryHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl QueryResult {
    fn fail(query: &str, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            query: query.into(),
            hits: Vec::new(),
            error: Some(error.into()),
        }
    }
}

pub fn run_query(doc: &Document, spec: &QuerySpec) -> QueryResult {
    match spec {
        QuerySpec::LeftOf {
            entity_id,
            min_extent,
        } => query_left_of(doc, entity_id, *min_extent),
        QuerySpec::OnScreen { min_extent, aspect } => query_on_screen(doc, *min_extent, *aspect),
        QuerySpec::ColorOf {
            color,
            min_extent,
            tolerance,
        } => query_color_of(doc, color, *min_extent, *tolerance),
        QuerySpec::AssemblyOf { entity_id } => query_assembly_of(doc, entity_id),
        QuerySpec::Pixel {
            x,
            y,
            width,
            height,
        } => query_pixel(doc, *x, *y, *width, *height),
        QuerySpec::Elements {
            bbox_min,
            bbox_max,
            y_min,
            y_max,
            not_adjacent_to,
            min_extent,
        } => query_elements(
            doc,
            *bbox_min,
            *bbox_max,
            *y_min,
            *y_max,
            not_adjacent_to.as_deref(),
            *min_extent,
        ),
    }
}

fn query_pixel(
    doc: &Document,
    x: u32,
    y: u32,
    width: Option<u32>,
    height: Option<u32>,
) -> QueryResult {
    let w = width.unwrap_or(crate::beauty::DEFAULT_WIDTH);
    let h = height.unwrap_or(crate::beauty::DEFAULT_HEIGHT);
    match crate::beauty::render_ids(doc, w, h) {
        Ok(buf) => match buf.pick(x, y) {
            Some(id) => QueryResult {
                ok: true,
                query: "pixel".into(),
                hits: vec![QueryHit::new(id, format!("objectId@({x},{y})"))],
                error: None,
            },
            None => QueryResult {
                ok: true,
                query: "pixel".into(),
                hits: Vec::new(),
                error: None,
            },
        },
        Err(e) => QueryResult::fail("pixel", e),
    }
}

fn query_elements(
    doc: &Document,
    bbox_min: Option<[f32; 3]>,
    bbox_max: Option<[f32; 3]>,
    y_min: Option<f32>,
    y_max: Option<f32>,
    not_adjacent_to: Option<&str>,
    min_extent: f32,
) -> QueryResult {
    let adj = not_adjacent_to.and_then(|id| {
        doc.find_entity(id)
            .map(|e| (id, world_center(doc, e), entity_extent(e)))
    });
    let mut hits = Vec::new();
    for entity in doc.entities() {
        if entity.mesh.recipe == "empty" {
            continue;
        }
        if entity_extent(entity) < min_extent {
            continue;
        }
        let w = world_center(doc, entity);
        if let Some(mn) = bbox_min {
            if w.x < mn[0] || w.y < mn[1] || w.z < mn[2] {
                continue;
            }
        }
        if let Some(mx) = bbox_max {
            if w.x > mx[0] || w.y > mx[1] || w.z > mx[2] {
                continue;
            }
        }
        if let Some(y0) = y_min {
            if w.y < y0 {
                continue;
            }
        }
        if let Some(y1) = y_max {
            if w.y > y1 {
                continue;
            }
        }
        if let Some((aid, ac, ae)) = &adj {
            if entity.id == *aid {
                continue;
            }
            let gap = ae + entity_extent(entity) + 0.05;
            if (w - *ac).length() < gap {
                continue;
            }
        }
        hits.push(QueryHit::new(
            entity.id.clone(),
            format!("elements;world=[{:.2},{:.2},{:.2}]", w.x, w.y, w.z),
        ));
    }
    QueryResult {
        ok: true,
        query: "elements".into(),
        hits,
        error: None,
    }
}

fn query_left_of(doc: &Document, entity_id: &str, min_extent: f32) -> QueryResult {
    let Some(anchor) = doc.find_entity(entity_id) else {
        return QueryResult::fail("left_of", format!("unknown_entity:{entity_id}"));
    };
    let view = camera::view_matrix();
    let anchor_x = view_x(view, world_center(doc, anchor));

    let mut scored: Vec<(f32, f32, QueryHit)> = Vec::new();
    for entity in doc.entities() {
        if entity.id == entity_id {
            continue;
        }
        let extent = entity_extent(entity);
        if extent < min_extent {
            continue;
        }
        let x = view_x(view, world_center(doc, entity));
        let dx = anchor_x - x; // positive => entity is to the left of anchor
        if dx <= 1e-3 {
            continue;
        }
        let ndc = project_ndc(
            camera::view_proj_matrix(DEFAULT_ASPECT),
            world_center(doc, entity),
        );
        let mut hit = QueryHit::new(
            entity.id.clone(),
            format!("camera_left_of:{entity_id};dx={dx:.3}"),
        );
        if let Some(ndc) = ndc {
            hit = hit.with_projection(
                ndc,
                occluded_by_others(doc, &entity.id, ndc, DEFAULT_ASPECT),
            );
        }
        scored.push((extent, dx, hit));
    }
    // Prefer primary (larger) bodies, then the ones furthest left.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    QueryResult {
        ok: true,
        query: "left_of".into(),
        hits: scored.into_iter().map(|(_, _, h)| h).collect(),
        error: None,
    }
}

fn query_on_screen(doc: &Document, min_extent: f32, aspect: f32) -> QueryResult {
    let view_proj = camera::view_proj_matrix(aspect);
    let mut scored: Vec<(f32, QueryHit)> = Vec::new();
    for entity in doc.entities() {
        let extent = entity_extent(entity);
        if extent < min_extent {
            continue;
        }
        let clip = view_proj * world_center(doc, entity).extend(1.0);
        if clip.w.abs() < 1e-6 {
            continue;
        }
        let ndc = clip.truncate() / clip.w;
        // Slight margin so near-edge centers still count as visible.
        if ndc.x.abs() <= 1.05 && ndc.y.abs() <= 1.05 && ndc.z >= -1.0 && ndc.z <= 1.0 {
            scored.push((
                extent,
                QueryHit::new(
                    entity.id.clone(),
                    format!("ndc=[{:.2},{:.2},{:.2}]", ndc.x, ndc.y, ndc.z),
                )
                .with_projection(ndc, occluded_by_others(doc, &entity.id, ndc, aspect)),
            ));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    QueryResult {
        ok: true,
        query: "on_screen".into(),
        hits: scored.into_iter().map(|(_, h)| h).collect(),
        error: None,
    }
}

fn query_color_of(
    doc: &Document,
    color: &ColorQuery,
    min_extent: f32,
    tolerance: f32,
) -> QueryResult {
    let (label, targets) = match color {
        ColorQuery::Name(name) => {
            let Some(samples) = named_color_samples(name) else {
                return QueryResult::fail("color_of", format!("unknown_color_name:{name}"));
            };
            (name.to_ascii_lowercase(), samples)
        }
        ColorQuery::Rgba(rgba) => ("rgba".into(), vec![[rgba[0], rgba[1], rgba[2]]]),
    };

    let mut scored: Vec<(f32, f32, QueryHit)> = Vec::new();
    for entity in doc.entities() {
        let extent = entity_extent(entity);
        if extent < min_extent {
            continue;
        }
        let rgb = {
            let c = doc.resolved_surface(entity).color;
            [c[0], c[1], c[2]]
        };
        let dist = targets
            .iter()
            .map(|t| color_distance(rgb, *t))
            .fold(f32::MAX, f32::min);
        if dist <= tolerance {
            scored.push((
                extent,
                dist,
                QueryHit::new(
                    entity.id.clone(),
                    format!("color≈{label};dist={dist:.3}"),
                ),
            ));
        }
    }
    scored.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
    });
    QueryResult {
        ok: true,
        query: "color_of".into(),
        hits: scored.into_iter().map(|(_, _, h)| h).collect(),
        error: None,
    }
}

fn query_assembly_of(doc: &Document, entity_id: &str) -> QueryResult {
    if doc.find_entity(entity_id).is_none() {
        return QueryResult::fail("assembly_of", format!("unknown_entity:{entity_id}"));
    }
    let prefix = format!("{entity_id}-");
    let hits: Vec<QueryHit> = doc
        .entities()
        .filter(|entity| entity.id == entity_id || entity.id.starts_with(&prefix))
        .map(|entity| QueryHit::new(entity.id.clone(), format!("name_prefix:{entity_id}")))
        .collect();
    QueryResult {
        ok: true,
        query: "assembly_of".into(),
        hits,
        error: None,
    }
}

fn named_color_samples(name: &str) -> Option<Vec<[f32; 3]>> {
    Some(match name.to_ascii_lowercase().as_str() {
        "yellow" | "amber" | "mustard" => vec![
            [0.90, 0.75, 0.12],
            [0.85, 0.65, 0.15],
            [0.95, 0.80, 0.20],
            [1.00, 1.00, 0.00],
        ],
        "red" => vec![
            [0.80, 0.18, 0.14],
            [0.75, 0.20, 0.15],
            [1.00, 0.00, 0.00],
            [0.70, 0.15, 0.12],
        ],
        "blue" => vec![
            [0.20, 0.45, 0.90],
            [0.45, 0.65, 0.95],
            [0.55, 0.75, 0.95],
            [0.35, 0.55, 0.85],
            [0.00, 0.00, 1.00],
        ],
        "terracotta" | "orange" => vec![[0.86, 0.34, 0.22], [0.90, 0.40, 0.18]],
        "gray" | "grey" | "dark" => vec![[0.25, 0.25, 0.27], [0.20, 0.20, 0.22]],
        _ => return None,
    })
}

fn color_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    (dr * dr + dg * dg + db * db).sqrt()
}

fn world_center(doc: &Document, entity: &Entity) -> Vec3 {
    Vec3::from_array(
        doc.world_translation(&entity.id)
            .unwrap_or(entity.transform.translation),
    )
}

fn view_x(view: Mat4, world: Vec3) -> f32 {
    view.transform_point3(world).x
}

pub fn entity_extent(entity: &Entity) -> f32 {
    let size = Vec3::from_array(entity.mesh.size);
    let scale = Vec3::from_array(entity.transform.scale);
    let extents = size * scale;
    extents.x.max(extents.y).max(extents.z)
}

#[cfg(test)]
mod sit_tests {
    use super::*;
    use crate::document::{
        ApplyChange, ApplyRequest, Material, MeshRecipe, Scene, Transform,
    };

    /// Live check sit: amber box-1 at origin (right/front), red box-2 at x=-2 (left),
    /// blue ground, tiny dark smiley bits. Matches the rev-6 still.
    fn sit_rev6() -> Document {
        let yellow = [0.88, 0.68, 0.14, 1.0];
        let red = [0.78, 0.18, 0.14, 1.0];
        let blue = [0.50, 0.70, 0.92, 1.0];
        let dark = [0.22, 0.22, 0.24, 1.0];

        let mut entities = vec![
            Entity {
                id: "ground".into(),
                kind: "mesh".into(),
                transform: Transform {
                    translation: [0.0, -0.05, 0.0],
                    ..Default::default()
                },
                mesh: MeshRecipe {
                    recipe: "box".into(),
                    size: [8.0, 0.1, 8.0],
                },
                material: Material { color: blue },
                parent: None,
                    graph_id: None,
            },
            Entity {
                id: "box-1".into(),
                kind: "mesh".into(),
                transform: Transform {
                    translation: [0.0, 0.5, 0.0],
                    ..Default::default()
                },
                mesh: MeshRecipe {
                    recipe: "box".into(),
                    size: [1.0, 1.0, 1.0],
                },
                material: Material { color: yellow },
                parent: None,
                    graph_id: None,
            },
            Entity {
                id: "box-2".into(),
                kind: "mesh".into(),
                transform: Transform {
                    translation: [-2.0, 0.5, 0.0],
                    ..Default::default()
                },
                mesh: MeshRecipe {
                    recipe: "box".into(),
                    size: [1.0, 1.0, 1.0],
                },
                material: Material { color: red },
                parent: None,
                    graph_id: None,
            },
        ];

        // Tiny smiley bits on both cubes — noise for ranking/min_extent.
        for (cube, origin) in [("box-1", [0.0_f32, 0.5, 0.0]), ("box-2", [-2.0, 0.5, 0.0])] {
            let bits = [
                ("eye-l", [origin[0] - 0.18, origin[1] + 0.18, origin[2] + 0.51]),
                ("eye-r", [origin[0] + 0.18, origin[1] + 0.18, origin[2] + 0.51]),
                ("mouth-0", [origin[0] - 0.18, origin[1] - 0.15, origin[2] + 0.51]),
                ("mouth-1", [origin[0], origin[1] - 0.18, origin[2] + 0.51]),
                ("mouth-2", [origin[0] + 0.18, origin[1] - 0.15, origin[2] + 0.51]),
            ];
            for (name, t) in bits {
                entities.push(Entity {
                    id: format!("{cube}-{name}"),
                    kind: "mesh".into(),
                    transform: Transform {
                        translation: t,
                        ..Default::default()
                    },
                    mesh: MeshRecipe {
                        recipe: "box".into(),
                        size: [0.12, 0.12, 0.08],
                    },
                    material: Material { color: dark },
                    parent: None,
                    graph_id: None,
                });
            }
        }

        Document {
            revision: 6,
            scenes: vec![Scene {
                id: "main".into(),
                entities,
            }],
            idempotency_log: vec![],
            graphs: vec![],
            light: crate::light::SceneLight::default(),
        }
    }

    #[test]
    fn yellow_is_in_front_and_right_of_red() {
        let doc = sit_rev6();
        let view = camera::view_matrix();
        let yellow = world_center(&doc, doc.find_entity("box-1").unwrap());
        let red = world_center(&doc, doc.find_entity("box-2").unwrap());
        let y = view.transform_point3(yellow);
        let r = view.transform_point3(red);
        // Camera-right is +X in view space; closer to camera is less-negative Z (RH, -Z forward).
        assert!(
            y.x > r.x,
            "yellow should be camera-right of red: y.x={:.3} r.x={:.3}",
            y.x,
            r.x
        );
        assert!(
            y.z > r.z,
            "yellow should be in front of red (larger view z): y.z={:.3} r.z={:.3}",
            y.z,
            r.z
        );
    }

    #[test]
    fn left_of_yellow_returns_red_cube() {
        let doc = sit_rev6();
        let result = run_query(
            &doc,
            &QuerySpec::LeftOf {
                entity_id: "box-1".into(),
                min_extent: DEFAULT_MIN_EXTENT,
            },
        );
        assert!(result.ok, "{result:?}");
        assert_eq!(
            result.hits.first().map(|h| h.id.as_str()),
            Some("box-2"),
            "expected box-2 first, got {:?}",
            result.hits
        );
        assert!(
            !result.hits.iter().any(|h| h.id.contains("eye") || h.id.contains("mouth")),
            "smiley bits should be filtered by min_extent: {:?}",
            result.hits
        );
    }

    #[test]
    fn color_of_red_yellow_blue_match_ids() {
        let doc = sit_rev6();

        let red = run_query(
            &doc,
            &QuerySpec::ColorOf {
                color: ColorQuery::Name("red".into()),
                min_extent: DEFAULT_MIN_EXTENT,
                tolerance: 0.32,
            },
        );
        assert_eq!(red.hits[0].id, "box-2");

        let yellow = run_query(
            &doc,
            &QuerySpec::ColorOf {
                color: ColorQuery::Name("yellow".into()),
                min_extent: DEFAULT_MIN_EXTENT,
                tolerance: 0.32,
            },
        );
        assert_eq!(yellow.hits[0].id, "box-1");

        let blue = run_query(
            &doc,
            &QuerySpec::ColorOf {
                color: ColorQuery::Name("blue".into()),
                min_extent: DEFAULT_MIN_EXTENT,
                tolerance: 0.32,
            },
        );
        assert_eq!(blue.hits[0].id, "ground");
    }

    #[test]
    fn on_screen_hits_include_screen_depth_occlusion() {
        let doc = sit_rev6();
        let result = run_query(
            &doc,
            &QuerySpec::OnScreen {
                min_extent: DEFAULT_MIN_EXTENT,
                aspect: DEFAULT_ASPECT,
            },
        );
        let box1 = result.hits.iter().find(|h| h.id == "box-1").unwrap();
        assert!(box1.screen.is_some());
        assert!(box1.depth.is_some());
        assert_eq!(box1.occluded, Some(false));
    }

    #[test]
    fn on_screen_includes_both_cubes() {
        let doc = sit_rev6();
        let result = run_query(
            &doc,
            &QuerySpec::OnScreen {
                min_extent: DEFAULT_MIN_EXTENT,
                aspect: DEFAULT_ASPECT,
            },
        );
        assert!(result.ok);
        let ids: Vec<_> = result.hits.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.contains(&"box-1"), "{ids:?}");
        assert!(ids.contains(&"box-2"), "{ids:?}");
        assert!(ids.contains(&"ground"), "{ids:?}");
    }

    #[test]
    fn move_the_one_left_of_yellow_via_query_ids() {
        let mut doc = sit_rev6();
        let left = run_query(
            &doc,
            &QuerySpec::LeftOf {
                entity_id: "box-1".into(),
                min_extent: DEFAULT_MIN_EXTENT,
            },
        );
        let target_id = left.hits[0].id.clone();
        assert_eq!(target_id, "box-2");

        let before = doc.revision;
        let apply = doc.apply(&ApplyRequest {
            base_revision: before,
            idempotency_key: "nudge-left-of-yellow".into(),
            label: "move the one left of yellow".into(),
            changes: vec![ApplyChange::PatchTranslation {
                entity_id: target_id.clone(),
                translation: [-2.0, 0.5, 1.0],
            }],
            dry_run: false,
        });
        assert!(apply.ok, "{apply:?}");
        assert_eq!(doc.revision, before + 1);
        assert_eq!(
            doc.find_entity(&target_id).unwrap().transform.translation,
            [-2.0, 0.5, 1.0]
        );
    }

    fn sit_assembly() -> Document {
        let yellow = [0.88, 0.68, 0.14, 1.0];
        let red = [0.78, 0.18, 0.14, 1.0];
        let blue = [0.50, 0.70, 0.92, 1.0];
        let dark = [0.22, 0.22, 0.24, 1.0];
        let spike = [0.20, 0.45, 0.90, 1.0];

        fn mesh(id: &str, t: [f32; 3], size: [f32; 3], color: [f32; 4]) -> Entity {
            Entity {
                id: id.into(),
                kind: "mesh".into(),
                transform: Transform {
                    translation: t,
                    ..Default::default()
                },
                mesh: MeshRecipe {
                    recipe: "box".into(),
                    size,
                },
                material: Material { color },
                parent: None,
                    graph_id: None,
            }
        }

        Document {
            revision: 15,
            scenes: vec![Scene {
                id: "main".into(),
                entities: vec![
                    mesh("ground", [0.0, -0.05, 0.0], [8.0, 0.1, 8.0], blue),
                    mesh("box-1", [0.0, 0.5, 0.0], [1.0, 1.0, 1.0], yellow),
                    mesh("box-1-hat-brim", [0.0, 1.12, 0.0], [1.2, 0.08, 1.2], yellow),
                    mesh("box-1-eye-l", [-0.18, 0.68, 0.51], [0.12, 0.12, 0.08], dark),
                    mesh("box-2", [-2.0, 0.5, 0.0], [1.0, 1.0, 1.0], red),
                    mesh("box-2-hair-0", [-2.0, -20.0, 0.0], [0.2, 0.4, 0.2], dark),
                    mesh("box-2-hair-1", [-1.85, -20.0, 0.1], [0.2, 0.4, 0.2], dark),
                    mesh("box-3", [2.0, 0.5, 0.0], [1.0, 1.0, 1.0], spike),
                    mesh("box-3-spike-0", [2.0, 1.3, 0.0], [0.15, 0.4, 0.15], spike),
                ],
            }],
            idempotency_log: vec![],
            graphs: vec![],
            light: crate::light::SceneLight::default(),
        }
    }

    fn assembly_ids(doc: &Document, entity_id: &str) -> Vec<String> {
        let result = run_query(
            doc,
            &QuerySpec::AssemblyOf {
                entity_id: entity_id.into(),
            },
        );
        assert!(result.ok, "{result:?}");
        result.hits.into_iter().map(|h| h.id).collect()
    }

    #[test]
    fn assembly_of_box_1_includes_hat_and_eye_not_box_2() {
        let doc = sit_assembly();
        let ids = assembly_ids(&doc, "box-1");
        assert!(ids.contains(&"box-1".into()), "{ids:?}");
        assert!(ids.contains(&"box-1-hat-brim".into()), "{ids:?}");
        assert!(ids.contains(&"box-1-eye-l".into()), "{ids:?}");
        assert!(!ids.iter().any(|id| id == "box-2" || id.starts_with("box-2-")), "{ids:?}");
        // min_extent does not apply: the tiny eye is part of the assembly.
        assert!(ids.contains(&"box-1-eye-l".into()));
    }

    #[test]
    fn assembly_of_missing_id_fails_clearly() {
        let doc = sit_assembly();
        let result = run_query(
            &doc,
            &QuerySpec::AssemblyOf {
                entity_id: "no-such".into(),
            },
        );
        assert!(!result.ok, "{result:?}");
        assert!(result.hits.is_empty());
        let err = result.error.expect("missing id must error");
        assert!(
            err.contains("unknown_entity:no-such"),
            "expected unknown_entity:no-such, got {err}"
        );
    }

    #[test]
    fn assembly_of_ground_is_just_ground() {
        let doc = sit_assembly();
        let ids = assembly_ids(&doc, "ground");
        assert_eq!(ids, vec!["ground".to_string()]);
    }

    #[test]
    fn assembly_of_includes_parked_hair_by_name() {
        let doc = sit_assembly();
        let ids = assembly_ids(&doc, "box-2");
        assert!(ids.contains(&"box-2".into()), "{ids:?}");
        assert!(ids.contains(&"box-2-hair-0".into()), "{ids:?}");
        assert!(ids.contains(&"box-2-hair-1".into()), "{ids:?}");
        let parked = doc.find_entity("box-2-hair-0").unwrap();
        assert!(parked.transform.translation[1] < -10.0);
        // parked still belongs; caller may skip y < -10 when moving.
        assert!(!ids.iter().any(|id| id.starts_with("box-1")), "{ids:?}");
    }

    #[test]
    fn assembly_of_box_3_includes_spikes() {
        let doc = sit_assembly();
        let ids = assembly_ids(&doc, "box-3");
        assert!(ids.contains(&"box-3".into()), "{ids:?}");
        assert!(ids.contains(&"box-3-spike-0".into()), "{ids:?}");
    }
}
