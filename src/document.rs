//! Versioned scene document — source of truth on disk.

use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_DOCUMENT_PATH: &str = "thinner-floor.json";
pub const DEFAULT_MAILBOX_ADDR: &str = "127.0.0.1:17421";
pub const DEFAULT_TOKEN_PATH: &str = "thinner-floor.token";
pub const MAX_ENTITIES: usize = 20_000;
const HISTORY_CAP: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub revision: u64,
    pub scenes: Vec<Scene>,
    /// Keys already applied; replaying the same key is a no-op success.
    #[serde(default)]
    pub idempotency_log: Vec<String>,
    /// D6: authored graphs (not compiled to GPU).
    #[serde(default)]
    pub graphs: Vec<MaterialGraph>,
    /// Directional sun + shadow map. Not RTX.
    #[serde(default)]
    pub light: crate::light::SceneLight,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub id: String,
    pub entities: Vec<Entity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub id: String,
    pub kind: String,
    pub transform: Transform,
    pub mesh: MeshRecipe,
    pub material: Material,
    /// Parent id. World matrix = parent world × local TRS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Bound principled graph. When set, draw/query/export use evaluated sockets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    pub translation: [f32; 3],
    /// Quaternion xyzw.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeshRecipe {
    pub recipe: String,
    pub size: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Material {
    /// RGBA 0..1
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ApplyChange {
    CreateMesh { entity: Entity },
    PatchColor {
        #[serde(rename = "entityId")]
        entity_id: String,
        color: [f32; 4],
    },
    /// Absolute world translation for an existing entity (boxes only; still thin).
    PatchTranslation {
        #[serde(rename = "entityId")]
        entity_id: String,
        translation: [f32; 3],
    },
    /// D1: spawn count boxes. pattern = linear | grid | radial | seeded_scatter.
    LayoutPattern {
        pattern: String,
        count: u32,
        origin: [f32; 3],
        #[serde(default = "default_spacing")]
        spacing: f32,
        #[serde(default)]
        columns: u32,
        #[serde(default)]
        seed: u64,
        #[serde(rename = "idPrefix")]
        id_prefix: String,
        color: [f32; 4],
        #[serde(default)]
        size: Option<[f32; 3]>,
    },
    PatchRotation {
        #[serde(rename = "entityId")]
        entity_id: String,
        /// Quaternion xyzw.
        rotation: [f32; 4],
    },
    /// D3: group. Members keep world TRS (parent × local).
    Group {
        #[serde(rename = "groupId")]
        group_id: String,
        #[serde(rename = "memberIds")]
        member_ids: Vec<String>,
    },
    Ungroup {
        #[serde(rename = "groupId")]
        group_id: String,
    },
    GraphCreate {
        #[serde(rename = "graphId")]
        graph_id: String,
    },
    /// Socket-level patch. Never a whole-graph replace.
    GraphPatch {
        #[serde(rename = "graphId")]
        graph_id: String,
        #[serde(rename = "nodeId")]
        node_id: String,
        socket: String,
        value: f32,
    },
    /// Bind (or unbind with empty graphId) a principled graph to one entity.
    GraphBind {
        #[serde(rename = "graphId")]
        graph_id: String,
        #[serde(rename = "entityId")]
        entity_id: String,
    },
    PatchLight {
        #[serde(default)]
        direction: Option<[f32; 3]>,
        #[serde(default)]
        shadows: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MaterialGraph {
    pub id: String,
    pub nodes: Vec<GraphNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub sockets: std::collections::BTreeMap<String, f32>,
}

pub fn principled_defaults() -> std::collections::BTreeMap<String, f32> {
    let mut m = std::collections::BTreeMap::new();
    m.insert("base_color_r".into(), 0.86);
    m.insert("base_color_g".into(), 0.34);
    m.insert("base_color_b".into(), 0.22);
    m.insert("roughness".into(), 0.5);
    m.insert("metallic".into(), 0.0);
    m.insert("transmission".into(), 0.0);
    m
}

/// CPU-evaluated surface used at draw. Not a compiled WGSL graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
}

fn surface_from_principled(n: &GraphNode) -> Surface {
    let sock = |k: &str, d: f32| n.sockets.get(k).copied().unwrap_or(d);
    Surface {
        color: [
            sock("base_color_r", 0.86),
            sock("base_color_g", 0.34),
            sock("base_color_b", 0.22),
            1.0,
        ],
        roughness: sock("roughness", 0.5).clamp(0.04, 1.0),
        metallic: sock("metallic", 0.0).clamp(0.0, 1.0),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValidateDiag {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValidateResult {
    pub ok: bool,
    pub diagnostics: Vec<ValidateDiag>,
}

fn default_spacing() -> f32 {
    1.5
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRequest {
    pub base_revision: u64,
    pub idempotency_key: String,
    pub label: String,
    pub changes: Vec<ApplyChange>,
    /// Document-diff only in Phase B (no pixelForecast yet).
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub ok: bool,
    pub revision: u64,
    pub label: String,
    pub idempotent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Machine code: revision_conflict | idempotency_reused | …
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u64>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_forecast: Option<crate::beauty::PixelForecast>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub revision: u64,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResult {
    pub ok: bool,
    pub revision: u64,
    pub label: String,
    pub kind: String,
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub entries: Vec<HistoryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectResult {
    pub ok: bool,
    pub action: String,
    pub path: String,
    pub revision: u64,
    pub entity_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectSummary {
    pub revision: u64,
    pub entity_count: usize,
    pub entities: Vec<InspectEntity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectEntity {
    pub id: String,
    pub translation: [f32; 3],
    pub color: [f32; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_id: Option<String>,
}

/// `$lamp` and `lamp` are the same id. `$` is a same-transaction alias prefix.
pub fn canonical_id(raw: &str) -> String {
    raw.strip_prefix('$').unwrap_or(raw).to_string()
}

impl Document {
    pub fn bootstrap() -> Self {
        Self {
            revision: 1,
            scenes: vec![Scene {
                id: "main".into(),
                entities: vec![
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
                        material: Material {
                            color: [0.32, 0.34, 0.38, 1.0],
                        },
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
                        material: Material {
                            color: [0.86, 0.34, 0.22, 1.0],
                        },
                        parent: None,
                    graph_id: None,
                    },
                ],
            }],
            idempotency_log: Vec::new(),
            graphs: Vec::new(),
            light: crate::light::SceneLight::default(),
        }
    }

    /// Bounding sphere for the directional shadow ortho.
    pub fn light_fit(&self) -> (glam::Vec3, f32) {
        use glam::Vec3;
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        let mut any = false;
        for e in self.entities() {
            if e.mesh.recipe == "empty" {
                continue;
            }
            let max_e = e.mesh.size[0]
                .abs()
                .max(e.mesh.size[1].abs())
                .max(e.mesh.size[2].abs());
            if max_e < 0.35 {
                continue;
            }
            let t = self
                .world_translation(&e.id)
                .map(Vec3::from_array)
                .unwrap_or_else(|| Vec3::from_array(e.transform.translation));
            let r = e.mesh.size.iter().copied().fold(0.1_f32, f32::max) * 0.5;
            min = min.min(t - Vec3::splat(r));
            max = max.max(t + Vec3::splat(r));
            any = true;
        }
        if !any || !min.is_finite() {
            return (Vec3::new(0.0, 0.5, 0.0), 8.0);
        }
        let c = (min + max) * 0.5;
        let radius = ((max - min).length() * 0.5 + 1.0).clamp(4.0, 20.0);
        (c, radius)
    }

    pub fn entity_count(&self) -> usize {
        self.scenes.iter().map(|s| s.entities.len()).sum()
    }

    pub fn find_entity_mut(&mut self, entity_id: &str) -> Option<&mut Entity> {
        for scene in &mut self.scenes {
            if let Some(e) = scene.entities.iter_mut().find(|e| e.id == entity_id) {
                return Some(e);
            }
        }
        None
    }

    pub fn local_matrix(t: &Transform) -> Mat4 {
        Mat4::from_scale_rotation_translation(
            Vec3::from_array(t.scale),
            Quat::from_xyzw(t.rotation[0], t.rotation[1], t.rotation[2], t.rotation[3]),
            Vec3::from_array(t.translation),
        )
    }

    pub fn world_matrix(&self, entity_id: &str) -> Option<Mat4> {
        let mut locals = Vec::new();
        let mut cur = Some(entity_id.to_string());
        for _ in 0..32 {
            let Some(cid) = cur else {
                break;
            };
            let e = self.find_entity(&cid)?;
            locals.push(Self::local_matrix(&e.transform));
            cur = e.parent.clone();
        }
        locals.reverse();
        Some(locals.into_iter().fold(Mat4::IDENTITY, |a, b| a * b))
    }

    pub fn world_translation(&self, entity_id: &str) -> Option<[f32; 3]> {
        Some(
            self.world_matrix(entity_id)?
                .to_scale_rotation_translation()
                .2
                .to_array(),
        )
    }

    fn set_local_from_world(&mut self, entity_id: &str, world: Mat4) -> Result<(), String> {
        let parent = self.find_entity(entity_id).and_then(|e| e.parent.clone());
        let local = match parent {
            Some(p) => {
                let pw = self
                    .world_matrix(&p)
                    .ok_or_else(|| format!("unknown_entity:{p}"))?;
                pw.inverse() * world
            }
            None => world,
        };
        let (scale, rot, trans) = local.to_scale_rotation_translation();
        let e = self
            .find_entity_mut(entity_id)
            .ok_or_else(|| format!("unknown_entity:{entity_id}"))?;
        e.transform.scale = scale.to_array();
        e.transform.rotation = [rot.x, rot.y, rot.z, rot.w];
        e.transform.translation = trans.to_array();
        Ok(())
    }

    pub fn find_entity(&self, entity_id: &str) -> Option<&Entity> {
        for scene in &self.scenes {
            if let Some(e) = scene.entities.iter().find(|e| e.id == entity_id) {
                return Some(e);
            }
        }
        None
    }

    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.scenes.iter().flat_map(|s| s.entities.iter())
    }

    /// Apply a labelled changeset. Rejects stale `base_revision`. Idempotent on key.
    pub fn apply(&mut self, req: &ApplyRequest) -> ApplyResult {
        let fail = |error: String, code: &str| ApplyResult {
            ok: false,
            revision: self.revision,
            label: req.label.clone(),
            idempotent: false,
            error: Some(error),
            code: Some(code.into()),
            current_revision: Some(self.revision),
            dry_run: req.dry_run,
            pixel_forecast: None,
        };

        if req.changes.len() > 128 {
            return fail(
                format!("too_many_ops:{}", req.changes.len()),
                "too_many_ops",
            );
        }

        if req.idempotency_key.is_empty() {
            return fail("empty_idempotency_key".into(), "empty_idempotency_key");
        }

        if self.idempotency_log.iter().any(|k| k == &req.idempotency_key) {
            return ApplyResult {
                ok: true,
                revision: self.revision,
                label: req.label.clone(),
                idempotent: true,
                error: None,
                code: Some("idempotency_reused".into()),
                current_revision: None,
                dry_run: req.dry_run,
                pixel_forecast: None,
            };
        }

        if req.base_revision != self.revision {
            return fail("revision_conflict".into(), "revision_conflict");
        }

        let mut working = self.clone();
        if let Err(err) = working.apply_changes(&req.changes) {
            return fail(err, "apply_failed");
        }
        if working.entity_count() > MAX_ENTITIES {
            return fail(
                format!("too_many_entities:{}", working.entity_count()),
                "too_many_entities",
            );
        }

        if req.dry_run {
            let pixel_forecast = crate::beauty::forecast_documents(self, &working);
            return ApplyResult {
                ok: true,
                revision: self.revision,
                label: req.label.clone(),
                idempotent: false,
                error: None,
                code: None,
                current_revision: Some(self.revision.saturating_add(1)),
                dry_run: true,
                pixel_forecast,
            };
        }

        working.revision = self.revision.saturating_add(1);
        working.idempotency_log.push(req.idempotency_key.clone());
        // Keep the log from growing without bound in this thin sit.
        if working.idempotency_log.len() > 256 {
            let drop_n = working.idempotency_log.len() - 256;
            working.idempotency_log.drain(0..drop_n);
        }

        *self = working;
        ApplyResult {
            ok: true,
            revision: self.revision,
            label: req.label.clone(),
            idempotent: false,
            error: None,
            code: None,
            current_revision: None,
            dry_run: false,
            pixel_forecast: None,
        }
    }

    pub fn inspect_summary(&self) -> InspectSummary {
        InspectSummary {
            revision: self.revision,
            entity_count: self.entity_count(),
            entities: self
                .entities()
                .map(|e| InspectEntity {
                    id: e.id.clone(),
                    translation: self
                        .world_translation(&e.id)
                        .unwrap_or(e.transform.translation),
                    color: self.resolved_surface(e).color,
                    graph_id: e.graph_id.clone(),
                })
                .collect(),
        }
    }

    pub fn resolved_surface(&self, entity: &Entity) -> Surface {
        if let Some(gid) = entity.graph_id.as_deref() {
            if let Some(g) = self.graphs.iter().find(|g| g.id == gid) {
                if let Some(n) = g.nodes.iter().find(|n| n.type_name == "principled") {
                    return surface_from_principled(n);
                }
            }
        }
        Surface {
            color: entity.material.color,
            roughness: 0.5,
            metallic: 0.0,
        }
    }

    pub fn validate(&self) -> ValidateResult {
        let mut diagnostics = Vec::new();
        let mut seen = HashSet::new();
        let ids: HashSet<String> = self.entities().map(|e| e.id.clone()).collect();
        for e in self.entities() {
            if !seen.insert(e.id.clone()) {
                diagnostics.push(ValidateDiag {
                    code: "duplicate_entity_id".into(),
                    message: format!("duplicate id {}", e.id),
                    entity_id: Some(e.id.clone()),
                });
            }
            if let Some(p) = e.parent.as_deref() {
                if !ids.contains(p) {
                    diagnostics.push(ValidateDiag {
                        code: "missing_parent".into(),
                        message: format!("{} parent {p} missing", e.id),
                        entity_id: Some(e.id.clone()),
                    });
                }
            }
            if let Some(gid) = e.graph_id.as_deref() {
                if !self.graphs.iter().any(|g| g.id == gid) {
                    diagnostics.push(ValidateDiag {
                        code: "missing_graph".into(),
                        message: format!("{} graph {gid} missing", e.id),
                        entity_id: Some(e.id.clone()),
                    });
                }
            }
            match e.mesh.recipe.as_str() {
                "box" | "plane" | "sphere" | "empty" => {}
                other => diagnostics.push(ValidateDiag {
                    code: "unknown_recipe".into(),
                    message: format!("{} recipe {other}", e.id),
                    entity_id: Some(e.id.clone()),
                }),
            }
            if self.parent_cycle(&e.id) {
                diagnostics.push(ValidateDiag {
                    code: "parent_cycle".into(),
                    message: format!("{} parent cycle", e.id),
                    entity_id: Some(e.id.clone()),
                });
            }
        }
        for g in &self.graphs {
            for n in &g.nodes {
                if n.type_name != "principled" {
                    diagnostics.push(ValidateDiag {
                        code: "unknown_node_type".into(),
                        message: format!("graph {} node {} type {}", g.id, n.id, n.type_name),
                        entity_id: None,
                    });
                }
            }
        }
        ValidateResult {
            ok: diagnostics.is_empty(),
            diagnostics,
        }
    }

    fn parent_cycle(&self, start: &str) -> bool {
        let mut seen = HashSet::new();
        let mut cur = Some(start.to_string());
        while let Some(id) = cur {
            if !seen.insert(id.clone()) {
                return true;
            }
            cur = self.find_entity(&id).and_then(|e| e.parent.clone());
        }
        false
    }

    fn apply_changes(&mut self, changes: &[ApplyChange]) -> Result<(), String> {
        let mut aliases: HashMap<String, String> = HashMap::new();
        for existing in self.entities() {
            let c = canonical_id(&existing.id);
            aliases.insert(existing.id.clone(), c.clone());
            aliases.insert(format!("${c}"), c.clone());
            aliases.insert(c, existing.id.clone());
        }
        let mut seen_new_ids = HashSet::new();
        for change in changes {
            match change {
                ApplyChange::CreateMesh { entity } => {
                    if entity.id.is_empty() || canonical_id(&entity.id).is_empty() {
                        return Err("empty_entity_id".into());
                    }
                    if !matches!(
                        entity.mesh.recipe.as_str(),
                        "box" | "plane" | "sphere" | "empty"
                    ) {
                        return Err(format!("unsupported_mesh_recipe:{}", entity.mesh.recipe));
                    }
                    let stored = canonical_id(&entity.id);
                    if self.find_entity(&stored).is_some() || seen_new_ids.contains(&stored) {
                        return Err(format!("duplicate_entity_id:{stored}"));
                    }
                    seen_new_ids.insert(stored.clone());
                    aliases.insert(entity.id.clone(), stored.clone());
                    aliases.insert(format!("${stored}"), stored.clone());
                    aliases.insert(stored.clone(), stored.clone());
                    let mut created = entity.clone();
                    created.id = stored;
                    let scene = self
                        .scenes
                        .first_mut()
                        .ok_or_else(|| "no_scene".to_string())?;
                    scene.entities.push(created);
                }
                ApplyChange::PatchColor { entity_id, color } => {
                    let id = aliases
                        .get(entity_id)
                        .cloned()
                        .unwrap_or_else(|| canonical_id(entity_id));
                    let gid = {
                        let entity = self
                            .find_entity_mut(&id)
                            .ok_or_else(|| format!("unknown_entity:{entity_id}"))?;
                        entity.material.color = *color;
                        entity.graph_id.clone()
                    };
                    if let Some(gid) = gid {
                        if let Some(g) = self.graphs.iter_mut().find(|g| g.id == gid) {
                            if let Some(n) =
                                g.nodes.iter_mut().find(|n| n.type_name == "principled")
                            {
                                n.sockets.insert("base_color_r".into(), color[0]);
                                n.sockets.insert("base_color_g".into(), color[1]);
                                n.sockets.insert("base_color_b".into(), color[2]);
                            }
                        }
                    }
                }
                ApplyChange::PatchTranslation {
                    entity_id,
                    translation,
                } => {
                    let id = aliases
                        .get(entity_id)
                        .cloned()
                        .unwrap_or_else(|| canonical_id(entity_id));
                    let mut world = self
                        .world_matrix(&id)
                        .ok_or_else(|| format!("unknown_entity:{entity_id}"))?;
                    let (scale, rot, _) = world.to_scale_rotation_translation();
                    world = Mat4::from_scale_rotation_translation(
                        scale,
                        rot,
                        Vec3::from_array(*translation),
                    );
                    self.set_local_from_world(&id, world)?;
                }
                ApplyChange::PatchRotation {
                    entity_id,
                    rotation,
                } => {
                    let id = aliases
                        .get(entity_id)
                        .cloned()
                        .unwrap_or_else(|| canonical_id(entity_id));
                    let world = self
                        .world_matrix(&id)
                        .ok_or_else(|| format!("unknown_entity:{entity_id}"))?;
                    let (scale, _, trans) = world.to_scale_rotation_translation();
                    let rot = Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]);
                    let world = Mat4::from_scale_rotation_translation(scale, rot, trans);
                    self.set_local_from_world(&id, world)?;
                }
                ApplyChange::LayoutPattern {
                    pattern,
                    count,
                    origin,
                    spacing,
                    columns,
                    seed,
                    id_prefix,
                    color,
                    size,
                } => {
                    let pts = crate::layout::positions(
                        pattern, *count, *origin, *spacing, *columns, *seed,
                    )?;
                    let sz = size.unwrap_or([0.8, 0.8, 0.8]);
                    let prefix = canonical_id(id_prefix);
                    for (i, t) in pts.into_iter().enumerate() {
                        let id = format!("{prefix}-{i}");
                        if self.find_entity(&id).is_some() || seen_new_ids.contains(&id) {
                            return Err(format!("duplicate_entity_id:{id}"));
                        }
                        seen_new_ids.insert(id.clone());
                        aliases.insert(id.clone(), id.clone());
                        aliases.insert(format!("${id}"), id.clone());
                        let scene = self.scenes.first_mut().ok_or_else(|| "no_scene".to_string())?;
                        scene.entities.push(Entity {
                            id,
                            kind: "mesh".into(),
                            transform: Transform {
                                translation: t,
                                ..Default::default()
                            },
                            mesh: MeshRecipe {
                                recipe: "box".into(),
                                size: sz,
                            },
                            material: Material { color: *color },
                            parent: None,
                    graph_id: None,
                        });
                    }
                }
                ApplyChange::Group {
                    group_id,
                    member_ids,
                } => {
                    let gid = canonical_id(group_id);
                    if self.find_entity(&gid).is_some() || seen_new_ids.contains(&gid) {
                        return Err(format!("duplicate_entity_id:{gid}"));
                    }
                    let mut worlds = Vec::new();
                    for raw in member_ids {
                        let id = aliases
                            .get(raw)
                            .cloned()
                            .unwrap_or_else(|| canonical_id(raw));
                        let w = self
                            .world_matrix(&id)
                            .ok_or_else(|| format!("unknown_entity:{id}"))?;
                        worlds.push((id, w));
                    }
                    if worlds.is_empty() {
                        return Err("group_empty".into());
                    }
                    let n = worlds.len() as f32;
                    let centroid = worlds
                        .iter()
                        .map(|(_, w)| w.to_scale_rotation_translation().2)
                        .fold(Vec3::ZERO, |a, b| a + b)
                        / n;
                    seen_new_ids.insert(gid.clone());
                    {
                        let scene = self.scenes.first_mut().ok_or_else(|| "no_scene".to_string())?;
                        scene.entities.push(Entity {
                            id: gid.clone(),
                            kind: "group".into(),
                            transform: Transform {
                                translation: centroid.to_array(),
                                ..Default::default()
                            },
                            mesh: MeshRecipe {
                                recipe: "empty".into(),
                                size: [0.0, 0.0, 0.0],
                            },
                            material: Material {
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            parent: None,
                    graph_id: None,
                        });
                    }
                    let group_world = Mat4::from_translation(centroid);
                    for (id, w) in worlds {
                        let local = group_world.inverse() * w;
                        let (scale, rot, trans) = local.to_scale_rotation_translation();
                        let e = self
                            .find_entity_mut(&id)
                            .ok_or_else(|| format!("unknown_entity:{id}"))?;
                        e.parent = Some(gid.clone());
                        e.transform.scale = scale.to_array();
                        e.transform.rotation = [rot.x, rot.y, rot.z, rot.w];
                        e.transform.translation = trans.to_array();
                    }
                }
                ApplyChange::Ungroup { group_id } => {
                    let gid = canonical_id(group_id);
                    let gw = self
                        .world_matrix(&gid)
                        .ok_or_else(|| format!("unknown_entity:{gid}"))?;
                    let kids: Vec<String> = self
                        .entities()
                        .filter(|e| e.parent.as_deref() == Some(gid.as_str()))
                        .map(|e| e.id.clone())
                        .collect();
                    for id in kids {
                        let local = Self::local_matrix(&self.find_entity(&id).unwrap().transform);
                        let world = gw * local;
                        let e = self.find_entity_mut(&id).unwrap();
                        e.parent = None;
                        let (scale, rot, trans) = world.to_scale_rotation_translation();
                        e.transform.scale = scale.to_array();
                        e.transform.rotation = [rot.x, rot.y, rot.z, rot.w];
                        e.transform.translation = trans.to_array();
                    }
                    if let Some(scene) = self.scenes.first_mut() {
                        scene.entities.retain(|e| e.id != gid);
                    }
                }
                ApplyChange::GraphCreate { graph_id } => {
                    let gid = canonical_id(graph_id);
                    if self.graphs.iter().any(|g| g.id == gid) {
                        return Err(format!("duplicate_graph_id:{gid}"));
                    }
                    self.graphs.push(MaterialGraph {
                        id: gid,
                        nodes: vec![GraphNode {
                            id: "principled".into(),
                            type_name: "principled".into(),
                            sockets: principled_defaults(),
                        }],
                    });
                }
                ApplyChange::GraphPatch {
                    graph_id,
                    node_id,
                    socket,
                    value,
                } => {
                    let gid = canonical_id(graph_id);
                    let nid = canonical_id(node_id);
                    let g = self
                        .graphs
                        .iter_mut()
                        .find(|g| g.id == gid)
                        .ok_or_else(|| format!("unknown_graph:{gid}"))?;
                    let n = g
                        .nodes
                        .iter_mut()
                        .find(|n| n.id == nid)
                        .ok_or_else(|| format!("unknown_node:{nid}"))?;
                    if !n.sockets.contains_key(socket) {
                        return Err(format!("unknown_socket:{socket}"));
                    }
                    n.sockets.insert(socket.clone(), *value);
                }
                ApplyChange::GraphBind {
                    graph_id,
                    entity_id,
                } => {
                    let eid = aliases
                        .get(entity_id)
                        .cloned()
                        .unwrap_or_else(|| canonical_id(entity_id));
                    let gid = canonical_id(graph_id);
                    if !gid.is_empty() && !self.graphs.iter().any(|g| g.id == gid) {
                        return Err(format!("unknown_graph:{gid}"));
                    }
                    let entity = self
                        .find_entity_mut(&eid)
                        .ok_or_else(|| format!("unknown_entity:{entity_id}"))?;
                    entity.graph_id = if gid.is_empty() { None } else { Some(gid) };
                }
                ApplyChange::PatchLight { direction, shadows } => {
                    if let Some(d) = direction {
                        let v = glam::Vec3::from_array(*d);
                        if v.length_squared() < 1e-8 {
                            return Err("light_direction_zero".into());
                        }
                        self.light.direction = *d;
                    }
                    if let Some(s) = shadows {
                        self.light.shadows = *s;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load_or_bootstrap(path: &Path) -> io::Result<Self> {
        match Self::load(path) {
            Ok(doc) => Ok(doc),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let doc = Self::bootstrap();
                doc.save(path)?;
                Ok(doc)
            }
            Err(e) => Err(e),
        }
    }
}

/// Shared store: document on disk + in-memory revision for the live window.
#[derive(Debug)]
pub struct DocumentStore {
    pub path: PathBuf,
    pub document: Document,
    undo: Vec<Document>,
    redo: Vec<Document>,
    journal: Vec<HistoryEntry>,
}

impl DocumentStore {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let document = Document::load_or_bootstrap(&path)?;
        Ok(Self {
            path,
            document,
            undo: Vec::new(),
            redo: Vec::new(),
            journal: Vec::new(),
        })
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    fn push_journal(&mut self, revision: u64, label: impl Into<String>, kind: &str) {
        self.journal.push(HistoryEntry {
            revision,
            label: label.into(),
            kind: kind.into(),
        });
        if self.journal.len() > HISTORY_CAP {
            let n = self.journal.len() - HISTORY_CAP;
            self.journal.drain(0..n);
        }
    }

    fn cap_undo(&mut self) {
        if self.undo.len() > HISTORY_CAP {
            let n = self.undo.len() - HISTORY_CAP;
            self.undo.drain(0..n);
        }
    }

    pub fn apply(&mut self, req: &ApplyRequest) -> io::Result<ApplyResult> {
        let before = self.document.clone();
        let result = self.document.apply(req);
        if result.ok && !result.idempotent && !result.dry_run {
            self.undo.push(before);
            self.cap_undo();
            self.redo.clear();
            self.push_journal(result.revision, req.label.clone(), "apply");
            self.document.save(&self.path)?;
        }
        Ok(result)
    }

    pub fn history_list(&self) -> HistoryResult {
        HistoryResult {
            ok: true,
            revision: self.document.revision,
            label: String::new(),
            kind: "list".into(),
            undo_depth: self.undo.len(),
            redo_depth: self.redo.len(),
            entries: self.journal.clone(),
            error: None,
        }
    }

    pub fn undo(&mut self) -> io::Result<HistoryResult> {
        let Some(prev) = self.undo.pop() else {
            return Ok(HistoryResult {
                ok: false,
                revision: self.document.revision,
                label: "undo".into(),
                kind: "undo".into(),
                undo_depth: 0,
                redo_depth: self.redo.len(),
                entries: self.journal.clone(),
                error: Some("empty_undo".into()),
            });
        };
        self.redo.push(self.document.clone());
        let new_rev = self.document.revision.saturating_add(1);
        let mut restored = prev;
        restored.revision = new_rev;
        self.document = restored;
        self.push_journal(new_rev, "undo", "undo");
        self.document.save(&self.path)?;
        Ok(self.ok_history("undo", "undo"))
    }

    pub fn redo(&mut self) -> io::Result<HistoryResult> {
        let Some(next) = self.redo.pop() else {
            return Ok(HistoryResult {
                ok: false,
                revision: self.document.revision,
                label: "redo".into(),
                kind: "redo".into(),
                undo_depth: self.undo.len(),
                redo_depth: 0,
                entries: self.journal.clone(),
                error: Some("empty_redo".into()),
            });
        };
        self.undo.push(self.document.clone());
        self.cap_undo();
        let new_rev = self.document.revision.saturating_add(1);
        let mut restored = next;
        restored.revision = new_rev;
        self.document = restored;
        self.push_journal(new_rev, "redo", "redo");
        self.document.save(&self.path)?;
        Ok(self.ok_history("redo", "redo"))
    }

    fn ok_history(&self, label: &str, kind: &str) -> HistoryResult {
        HistoryResult {
            ok: true,
            revision: self.document.revision,
            label: label.into(),
            kind: kind.into(),
            undo_depth: self.undo.len(),
            redo_depth: self.redo.len(),
            entries: self.journal.clone(),
            error: None,
        }
    }

    pub fn project_save(&mut self) -> io::Result<ProjectResult> {
        self.document.save(&self.path)?;
        Ok(ProjectResult {
            ok: true,
            action: "save".into(),
            path: self.path.display().to_string(),
            revision: self.document.revision,
            entity_count: self.document.entity_count(),
            error: None,
        })
    }

    pub fn project_open(&mut self, path: impl Into<PathBuf>) -> io::Result<ProjectResult> {
        let path = path.into();
        self.document = Document::load_or_bootstrap(&path)?;
        self.path = path;
        self.undo.clear();
        self.redo.clear();
        self.journal.clear();
        Ok(ProjectResult {
            ok: true,
            action: "open".into(),
            path: self.path.display().to_string(),
            revision: self.document.revision,
            entity_count: self.document.entity_count(),
            error: None,
        })
    }

    /// Cheap B create: write bootstrap JSON at `path` (must not exist), switch the live store to it.
    /// Same window. Revision 1. Undo/redo cleared.
    pub fn project_create(&mut self, path: impl Into<PathBuf>) -> io::Result<ProjectResult> {
        let path = path.into();
        if path.exists() {
            return Ok(ProjectResult {
                ok: false,
                action: "create".into(),
                path: path.display().to_string(),
                revision: self.document.revision,
                entity_count: self.document.entity_count(),
                error: Some("project_exists".into()),
            });
        }
        let doc = Document::bootstrap();
        doc.save(&path)?;
        self.document = doc;
        self.path = path;
        self.undo.clear();
        self.redo.clear();
        self.journal.clear();
        Ok(ProjectResult {
            ok: true,
            action: "create".into(),
            path: self.path.display().to_string(),
            revision: self.document.revision,
            entity_count: self.document.entity_count(),
            error: None,
        })
    }

    pub fn import_gltf(&mut self, gltf_path: impl AsRef<Path>) -> Result<crate::export::ImportResult, String> {
        let path = gltf_path.as_ref();
        let mut imported = crate::export::read_gltf(path)?;
        imported.revision = self.document.revision.saturating_add(1);
        let v = imported.validate();
        if !v.ok {
            return Err(format!(
                "import_invalid:{}",
                v.diagnostics
                    .first()
                    .map(|d| d.code.as_str())
                    .unwrap_or("unknown")
            ));
        }
        self.undo.push(self.document.clone());
        self.cap_undo();
        self.redo.clear();
        self.document = imported;
        self.push_journal(self.document.revision, "import", "import");
        self.document
            .save(&self.path)
            .map_err(|e| format!("import_save:{e}"))?;
        Ok(crate::export::ImportResult {
            path: path.display().to_string(),
            entity_count: self.document.entity_count(),
            graph_count: self.document.graphs.len(),
            revision: self.document.revision,
        })
    }

    pub fn project_list(&self) -> ProjectResult {
        ProjectResult {
            ok: true,
            action: "list".into(),
            path: self.path.display().to_string(),
            revision: self.document.revision,
            entity_count: self.document.entity_count(),
            error: None,
        }
    }

    pub fn reload_from_disk(&mut self) -> io::Result<()> {
        self.document = Document::load(&self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_box(id: &str, color: [f32; 4]) -> Entity {
        Entity {
            id: id.into(),
            kind: "mesh".into(),
            transform: Transform::default(),
            mesh: MeshRecipe {
                recipe: "box".into(),
                size: [1.0, 1.0, 1.0],
            },
            material: Material { color },
            parent: None,
                    graph_id: None,
        }
    }

    #[test]
    fn apply_create_then_patch_color_increments_revision() {
        let mut doc = Document {
            revision: 0,
            scenes: vec![Scene {
                id: "main".into(),
                entities: vec![],
            }],
            idempotency_log: vec![],
            graphs: vec![],
            light: crate::light::SceneLight::default(),
        };

        let create = ApplyRequest {
            base_revision: 0,
            idempotency_key: "create-box".into(),
            label: "create mesh".into(),
            changes: vec![ApplyChange::CreateMesh {
                entity: sample_box("box-1", [1.0, 0.0, 0.0, 1.0]),
            }],
            dry_run: false,
        };
        let r1 = doc.apply(&create);
        assert!(r1.ok);
        assert!(!r1.idempotent);
        assert_eq!(r1.revision, 1);
        assert_eq!(doc.revision, 1);
        assert_eq!(
            doc.find_entity("box-1").unwrap().material.color,
            [1.0, 0.0, 0.0, 1.0]
        );

        let patch = ApplyRequest {
            base_revision: 1,
            idempotency_key: "paint-blue".into(),
            label: "patch color".into(),
            changes: vec![ApplyChange::PatchColor {
                entity_id: "box-1".into(),
                color: [0.2, 0.4, 0.9, 1.0],
            }],
            dry_run: false,
        };
        let r2 = doc.apply(&patch);
        assert!(r2.ok);
        assert_eq!(r2.revision, 2);
        assert_eq!(
            doc.find_entity("box-1").unwrap().material.color,
            [0.2, 0.4, 0.9, 1.0]
        );
    }

    #[test]
    fn reject_stale_base_revision() {
        let mut doc = Document::bootstrap();
        assert_eq!(doc.revision, 1);

        let bad = ApplyRequest {
            base_revision: 0,
            idempotency_key: "stale".into(),
            label: "too old".into(),
            changes: vec![ApplyChange::PatchColor {
                entity_id: "box-1".into(),
                color: [0.0, 1.0, 0.0, 1.0],
            }],
            dry_run: false,
        };
        let r = doc.apply(&bad);
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("revision_conflict"));
        assert_eq!(r.code.as_deref(), Some("revision_conflict"));
        assert_eq!(r.current_revision, Some(1));
        assert_eq!(doc.revision, 1);
        assert_eq!(
            doc.find_entity("box-1").unwrap().material.color,
            [0.86, 0.34, 0.22, 1.0]
        );
    }

    #[test]
    fn idempotency_key_replay_is_noop() {
        let mut doc = Document::bootstrap();
        let req = ApplyRequest {
            base_revision: 1,
            idempotency_key: "once".into(),
            label: "paint".into(),
            changes: vec![ApplyChange::PatchColor {
                entity_id: "box-1".into(),
                color: [0.1, 0.8, 0.3, 1.0],
            }],
            dry_run: false,
        };
        let first = doc.apply(&req);
        assert!(first.ok && !first.idempotent);
        assert_eq!(doc.revision, 2);

        // Even with a mismatched baseRevision, the same key must short-circuit.
        let replay = ApplyRequest {
            base_revision: 1,
            ..req.clone()
        };
        let second = doc.apply(&replay);
        assert!(second.ok && second.idempotent);
        assert_eq!(second.code.as_deref(), Some("idempotency_reused"));
        assert_eq!(second.revision, 2);
        assert_eq!(doc.revision, 2);
    }

    #[test]
    fn round_trip_json_disk() {
        let dir = std::env::temp_dir().join(format!(
            "thinner-floor-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.json");

        let mut doc = Document::bootstrap();
        doc.save(&path).unwrap();
        let loaded = Document::load(&path).unwrap();
        assert_eq!(loaded, doc);

        let req = ApplyRequest {
            base_revision: 1,
            idempotency_key: "disk-paint".into(),
            label: "disk paint".into(),
            changes: vec![ApplyChange::PatchColor {
                entity_id: "box-1".into(),
                color: [0.05, 0.55, 0.95, 1.0],
            }],
            dry_run: false,
        };
        assert!(doc.apply(&req).ok);
        doc.save(&path).unwrap();
        let again = Document::load(&path).unwrap();
        assert_eq!(again.revision, 2);
        assert_eq!(
            again.find_entity("box-1").unwrap().material.color,
            [0.05, 0.55, 0.95, 1.0]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_does_not_commit() {
        let mut doc = Document::bootstrap();
        let req = ApplyRequest {
            base_revision: 1,
            idempotency_key: "dry-blue".into(),
            label: "dry paint".into(),
            changes: vec![ApplyChange::PatchColor {
                entity_id: "box-1".into(),
                color: [0.2, 0.45, 0.9, 1.0],
            }],
            dry_run: true,
        };
        let r = doc.apply(&req);
        assert!(r.ok);
        assert!(r.dry_run);
        assert_eq!(doc.revision, 1);
        assert_eq!(
            doc.find_entity("box-1").unwrap().material.color,
            [0.86, 0.34, 0.22, 1.0]
        );
        assert!(!doc.idempotency_log.contains(&"dry-blue".to_string()));
        let forecast = r.pixel_forecast.expect("pixelForecast on dry-run");
        assert!(forecast.pixels_will_move);
        assert!(forecast.changed_pixels > 0);
    }

    #[test]
    fn dry_run_same_color_forecasts_no_move() {
        let mut doc = Document::bootstrap();
        let terracotta = doc.find_entity("box-1").unwrap().material.color;
        let req = ApplyRequest {
            base_revision: 1,
            idempotency_key: "dry-same".into(),
            label: "same color".into(),
            changes: vec![ApplyChange::PatchColor {
                entity_id: "box-1".into(),
                color: terracotta,
            }],
            dry_run: true,
        };
        let r = doc.apply(&req);
        assert!(r.ok);
        let forecast = r.pixel_forecast.expect("forecast");
        assert!(!forecast.pixels_will_move);
    }

    #[test]
    fn dollar_alias_create_then_patch_stores_canonical_id() {
        let mut doc = Document {
            revision: 0,
            scenes: vec![Scene {
                id: "main".into(),
                entities: vec![],
            }],
            idempotency_log: vec![],
            graphs: vec![],
            light: crate::light::SceneLight::default(),
        };
        let mut hero = sample_box("$hero", [1.0, 0.0, 0.0, 1.0]);
        hero.id = "$hero".into();
        let req = ApplyRequest {
            base_revision: 0,
            idempotency_key: "alias-1".into(),
            label: "create $hero then paint".into(),
            changes: vec![
                ApplyChange::CreateMesh { entity: hero },
                ApplyChange::PatchColor {
                    entity_id: "$hero".into(),
                    color: [0.1, 0.8, 0.2, 1.0],
                },
            ],
            dry_run: false,
        };
        let r = doc.apply(&req);
        assert!(r.ok, "{r:?}");
        assert!(doc.find_entity("$hero").is_none());
        let e = doc.find_entity("hero").expect("canonical id");
        assert_eq!(e.material.color, [0.1, 0.8, 0.2, 1.0]);
    }

    #[test]
    fn undo_redo_are_new_revisions() {
        let dir = std::env::temp_dir().join(format!(
            "tf-hist-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.json");
        Document::bootstrap().save(&path).unwrap();
        let mut store = DocumentStore::open(&path).unwrap();
        assert_eq!(store.document.revision, 1);

        let paint = ApplyRequest {
            base_revision: 1,
            idempotency_key: "u1".into(),
            label: "paint".into(),
            changes: vec![ApplyChange::PatchColor {
                entity_id: "box-1".into(),
                color: [0.0, 1.0, 0.0, 1.0],
            }],
            dry_run: false,
        };
        assert!(store.apply(&paint).unwrap().ok);
        assert_eq!(store.document.revision, 2);
        assert_eq!(
            store.document.find_entity("box-1").unwrap().material.color[1],
            1.0
        );

        let u = store.undo().unwrap();
        assert!(u.ok);
        assert_eq!(u.revision, 3);
        assert_eq!(store.document.revision, 3);
        assert_eq!(
            store.document.find_entity("box-1").unwrap().material.color,
            [0.86, 0.34, 0.22, 1.0]
        );

        let r = store.redo().unwrap();
        assert!(r.ok);
        assert_eq!(r.revision, 4);
        assert_eq!(
            store.document.find_entity("box-1").unwrap().material.color[1],
            1.0
        );

        let loaded = Document::load(&path).unwrap();
        assert_eq!(loaded.revision, 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_create_writes_bootstrap_and_refuses_existing() {
        let dir = std::env::temp_dir().join(format!(
            "tf-create-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("old.json");
        Document::bootstrap().save(&existing).unwrap();
        let mut store = DocumentStore::open(&existing).unwrap();
        assert_eq!(store.project_create(&existing).unwrap().error.as_deref(), Some("project_exists"));

        let fresh = dir.join("new.json");
        let created = store.project_create(&fresh).unwrap();
        assert!(created.ok, "{created:?}");
        assert_eq!(created.revision, 1);
        assert_eq!(created.entity_count, 2);
        assert_eq!(store.document.revision, 1);
        assert_eq!(store.undo_depth(), 0);
        assert!(fresh.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn layout_linear_then_group_preserves_world() {
        let mut doc = Document::bootstrap();
        let req = ApplyRequest {
            base_revision: 1,
            idempotency_key: "lay".into(),
            label: "row".into(),
            changes: vec![ApplyChange::LayoutPattern {
                pattern: "linear".into(),
                count: 3,
                origin: [3.0, 0.5, 0.0],
                spacing: 1.5,
                columns: 0,
                seed: 0,
                id_prefix: "row".into(),
                color: [0.1, 0.8, 0.2, 1.0],
                size: None,
            }],
            dry_run: false,
        };
        assert!(doc.apply(&req).ok);
        let w0 = doc.world_translation("row-0").unwrap();
        let grp = ApplyRequest {
            base_revision: 2,
            idempotency_key: "g".into(),
            label: "group row".into(),
            changes: vec![ApplyChange::Group {
                group_id: "row-g".into(),
                member_ids: vec!["row-0".into(), "row-1".into(), "row-2".into()],
            }],
            dry_run: false,
        };
        assert!(doc.apply(&grp).ok);
        assert_eq!(
            doc.find_entity("row-0").unwrap().parent.as_deref(),
            Some("row-g")
        );
        let w1 = doc.world_translation("row-0").unwrap();
        assert!((w0[0] - w1[0]).abs() < 1e-4);
        let un = ApplyRequest {
            base_revision: 3,
            idempotency_key: "u".into(),
            label: "ungroup".into(),
            changes: vec![ApplyChange::Ungroup {
                group_id: "row-g".into(),
            }],
            dry_run: false,
        };
        assert!(doc.apply(&un).ok);
        assert!(doc.find_entity("row-g").is_none());
        assert!(doc.find_entity("row-0").unwrap().parent.is_none());
        let w2 = doc.world_translation("row-0").unwrap();
        assert!((w0[0] - w2[0]).abs() < 1e-4);
    }

    #[test]
    fn group_rotation_orbits_children() {
        let mut doc = Document::bootstrap();
        let mut a = sample_box("a", [1.0, 0.0, 0.0, 1.0]);
        a.transform.translation = [1.0, 0.5, 0.0];
        let mut b = sample_box("b", [0.0, 1.0, 0.0, 1.0]);
        b.transform.translation = [-1.0, 0.5, 0.0];
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 1,
                idempotency_key: "ab".into(),
                label: "two".into(),
                changes: vec![
                    ApplyChange::CreateMesh { entity: a },
                    ApplyChange::CreateMesh { entity: b },
                ],
                dry_run: false,
            })
            .ok);
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 2,
                idempotency_key: "g".into(),
                label: "g".into(),
                changes: vec![ApplyChange::Group {
                    group_id: "spin".into(),
                    member_ids: vec!["a".into(), "b".into()],
                }],
                dry_run: false,
            })
            .ok);
        let q = glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 3,
                idempotency_key: "r".into(),
                label: "spin 90".into(),
                changes: vec![ApplyChange::PatchRotation {
                    entity_id: "spin".into(),
                    rotation: [q.x, q.y, q.z, q.w],
                }],
                dry_run: false,
            })
            .ok);
        let wa = doc.world_translation("a").unwrap();
        // 90° Y: (1, 0.5, 0) around origin-ish centroid 0 → (0, 0.5, -1) or (0, 0.5, 1)
        assert!(wa[1] > 0.4 && wa[1] < 0.6);
        assert!(wa[0].abs() < 0.2 || wa[2].abs() > 0.7);
    }

    #[test]
    fn create_plane_and_sphere() {
        let mut doc = Document::bootstrap();
        let mut plane = sample_box("pad", [0.2, 0.5, 0.3, 1.0]);
        plane.mesh.recipe = "plane".into();
        plane.mesh.size = [3.0, 0.0, 3.0];
        plane.transform.translation = [0.0, 0.0, 3.0];
        let mut sph = sample_box("ball", [0.9, 0.2, 0.2, 1.0]);
        sph.mesh.recipe = "sphere".into();
        sph.transform.translation = [2.0, 0.8, 0.0];
        let r = doc.apply(&ApplyRequest {
            base_revision: 1,
            idempotency_key: "ps".into(),
            label: "plane sphere".into(),
            changes: vec![
                ApplyChange::CreateMesh { entity: plane },
                ApplyChange::CreateMesh { entity: sph },
            ],
            dry_run: false,
        });
        assert!(r.ok, "{r:?}");
        assert_eq!(doc.find_entity("pad").unwrap().mesh.recipe, "plane");
        assert_eq!(doc.find_entity("ball").unwrap().mesh.recipe, "sphere");
    }

    #[test]
    fn graph_patch_is_socket_level() {
        let mut doc = Document::bootstrap();
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 1,
                idempotency_key: "gc".into(),
                label: "graph".into(),
                changes: vec![ApplyChange::GraphCreate {
                    graph_id: "mat-1".into(),
                }],
                dry_run: false,
            })
            .ok);
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 2,
                idempotency_key: "gp".into(),
                label: "rough".into(),
                changes: vec![ApplyChange::GraphPatch {
                    graph_id: "mat-1".into(),
                    node_id: "principled".into(),
                    socket: "roughness".into(),
                    value: 0.2,
                }],
                dry_run: false,
            })
            .ok);
        let n = &doc.graphs[0].nodes[0];
        assert!((n.sockets["roughness"] - 0.2).abs() < 1e-6);
        assert!((n.sockets["transmission"] - 0.0).abs() < 1e-6);
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 3,
                idempotency_key: "bad".into(),
                label: "bad socket".into(),
                changes: vec![ApplyChange::GraphPatch {
                    graph_id: "mat-1".into(),
                    node_id: "principled".into(),
                    socket: "nope".into(),
                    value: 1.0,
                }],
                dry_run: false,
            })
            .error
            .unwrap()
            .contains("unknown_socket"));
    }

    #[test]
    fn graph_bind_evaluates_principled_color() {
        let mut doc = Document::bootstrap();
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 1,
                idempotency_key: "gc".into(),
                label: "graph".into(),
                changes: vec![ApplyChange::GraphCreate {
                    graph_id: "mat-1".into(),
                }],
                dry_run: false,
            })
            .ok);
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 2,
                idempotency_key: "gb".into(),
                label: "bind".into(),
                changes: vec![ApplyChange::GraphBind {
                    graph_id: "mat-1".into(),
                    entity_id: "box-1".into(),
                }],
                dry_run: false,
            })
            .ok);
        let e = doc.find_entity("box-1").unwrap();
        let s0 = doc.resolved_surface(e);
        assert!((s0.color[0] - 0.86).abs() < 1e-5);
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 3,
                idempotency_key: "gp".into(),
                label: "green".into(),
                changes: vec![ApplyChange::GraphPatch {
                    graph_id: "mat-1".into(),
                    node_id: "principled".into(),
                    socket: "base_color_g".into(),
                    value: 0.9,
                }],
                dry_run: false,
            })
            .ok);
        let e = doc.find_entity("box-1").unwrap();
        let s = doc.resolved_surface(e);
        assert!((s.color[1] - 0.9).abs() < 1e-5);
        assert!(doc.validate().ok);
    }

    #[test]
    fn validate_flags_dangling_graph() {
        let mut doc = Document::bootstrap();
        doc.find_entity_mut("box-1").unwrap().graph_id = Some("nope".into());
        let v = doc.validate();
        assert!(!v.ok);
        assert!(v.diagnostics.iter().any(|d| d.code == "missing_graph"));
    }

    #[test]
    fn patch_light_toggles_shadows() {
        let mut doc = Document::bootstrap();
        assert!(doc.light.shadows);
        assert!(doc
            .apply(&ApplyRequest {
                base_revision: 1,
                idempotency_key: "sh-off".into(),
                label: "shadows off".into(),
                changes: vec![ApplyChange::PatchLight {
                    direction: None,
                    shadows: Some(false),
                }],
                dry_run: false,
            })
            .ok);
        assert!(!doc.light.shadows);
        assert_eq!(doc.revision, 2);
    }
}
