//! Honest live capabilities — status.capabilities always wins over docs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub mcp: bool,
    pub render: bool,
    pub history: bool,
    pub jobs: bool,
    pub play: bool,
    pub rtx: bool,
    pub graphs: bool,
    pub export: bool,
    pub import: bool,
    pub lighting: bool,
    pub shadows: bool,
    pub cycles_stream: bool,
    pub behavior_runtime: bool,
    pub apply: ApplyCaps,
    pub query: QueryCaps,
    pub project: ProjectCaps,
    pub rtx_lifecycle: RtxLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RtxLifecycle {
    pub supported: bool,
    pub requested: bool,
    pub configured: bool,
    pub building: bool,
    pub active: bool,
    pub stale: bool,
    pub failed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectCaps {
    pub list: bool,
    pub save: bool,
    pub open: bool,
    pub create: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ApplyCaps {
    pub create_mesh: bool,
    pub patch_color: bool,
    pub patch_translation: bool,
    pub patch_rotation: bool,
    pub dry_run: bool,
    pub max_ops: u32,
    pub aliases: bool,
    pub max_entities: u32,
    pub layout_pattern: bool,
    pub group: bool,
    pub ungroup: bool,
    pub graph_create: bool,
    pub graph_patch: bool,
    pub graph_bind: bool,
    pub patch_light: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct QueryCaps {
    pub left_of: bool,
    pub color_of: bool,
    pub on_screen: bool,
    pub assembly_of: bool,
    pub elements: bool,
}

/// What this crate actually supports right now (Phase A + B adapter).
pub fn live() -> Capabilities {
    Capabilities {
        mcp: true,
        render: true,
        history: true,
        jobs: false,
        play: true,
        rtx: false,
        graphs: true,
        export: true,
        import: true,
        lighting: true,
        shadows: true,
        cycles_stream: crate::cycles_stream::exe_available(),
        behavior_runtime: false,
        apply: ApplyCaps {
            create_mesh: true,
            patch_color: true,
            patch_translation: true,
            patch_rotation: true,
            dry_run: true,
            max_ops: 128,
            aliases: true,
            max_entities: 20_000,
            layout_pattern: true,
            group: true,
            ungroup: true,
            graph_create: true,
            graph_patch: true,
            graph_bind: true,
            patch_light: true,
        },
        query: QueryCaps {
            left_of: true,
            color_of: true,
            on_screen: true,
            assembly_of: true,
            elements: true,
        },
        project: ProjectCaps {
            list: true,
            save: true,
            open: true,
            create: true,
        },
        rtx_lifecycle: {
            let s = crate::rtx::snapshot();
            RtxLifecycle {
                supported: s.supported,
                requested: s.requested,
                configured: s.configured,
                building: s.building,
                active: s.active,
                stale: s.stale,
                failed: s.failed,
            }
        },
    }
}

pub const MCP_TOOL_NAMES: &[&str] = &[
    "three_studio_status",
    "three_studio_project",
    "three_studio_inspect",
    "three_studio_apply",
    "three_studio_validate",
    "three_studio_play",
    "three_studio_render",
    "three_studio_history",
    "three_studio_job",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_tool_names() {
        assert_eq!(MCP_TOOL_NAMES.len(), 9);
    }

    #[test]
    fn live_flags_are_honest() {
        let c = live();
        assert!(c.mcp);
        assert!(c.render);
        assert!(!c.jobs);
        assert!(c.play);
        assert!(!c.behavior_runtime);
        assert!(!c.rtx);
        assert!(c.graphs);
        assert!(c.export);
        assert!(c.import);
        assert!(c.lighting);
        assert!(c.shadows);
        assert!(!c.rtx);
        assert!(c.apply.graph_bind);
        assert!(c.apply.patch_light);
        assert!(!c.behavior_runtime);
        assert!(c.history);
        assert!(c.apply.create_mesh);
        assert!(c.apply.aliases);
        assert!(c.project.create);
        assert!(c.apply.layout_pattern);
        assert!(c.apply.group);
        assert!(c.apply.ungroup);
        assert!(c.query.elements);
        assert!(!c.rtx_lifecycle.active);
        assert_eq!(c.apply.max_ops, 128);
    }
}
