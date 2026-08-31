//! Mailbox live-feed log — compact cards for the viewport HUD.

use crate::document::{ApplyChange, ApplyRequest};
use crate::mailbox::{MailboxRequest, MailboxResponse};
use crate::query::{ColorQuery, QuerySpec};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const FEED_CAPACITY: usize = 256;
const ACTIVE_WINDOW: Duration = Duration::from_secs(2);
/// Max hit ids drawn on a card before "+N more".
pub const FEED_HIT_CAP: usize = 12;

#[derive(Debug, Clone)]
pub struct FeedEvent {
    pub op: String,
    pub ok: bool,
    pub elapsed_ms: f32,
    pub revision: Option<u64>,
    pub summary: String,
    /// Query hit ids (not jammed into `summary`).
    pub hits: Vec<String>,
    pub at: Instant,
}

#[derive(Debug, Default)]
pub struct MailboxFeed {
    events: VecDeque<FeedEvent>,
    /// Monotonic stamp so the HUD can skip redraws when quiet.
    pub seq: u64,
}

impl MailboxFeed {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Newest-first iterator for the drawer (uncapped; HUD takes what fits).
    pub fn newest_first(&self) -> impl Iterator<Item = &FeedEvent> {
        self.events.iter().rev()
    }

    pub fn is_active(&self, now: Instant) -> bool {
        self.events
            .back()
            .map(|e| now.saturating_duration_since(e.at) <= ACTIVE_WINDOW)
            .unwrap_or(false)
    }

    pub fn push(&mut self, event: FeedEvent) {
        // Future no-op noise (e.g. ping) stays out of the drawer.
        if event.op == "ping" {
            return;
        }
        if self.events.len() >= FEED_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(event);
        self.seq = self.seq.wrapping_add(1);
    }

    pub fn record_exchange(
        &mut self,
        request: &MailboxRequest,
        response: &MailboxResponse,
        elapsed: Duration,
    ) {
        let (op, summary, revision, hits) = summarize(request, response);
        self.push(FeedEvent {
            op,
            ok: response.ok,
            elapsed_ms: elapsed.as_secs_f32() * 1000.0,
            revision,
            summary,
            hits,
            at: Instant::now(),
        });
    }
}

fn summarize(
    request: &MailboxRequest,
    response: &MailboxResponse,
) -> (String, String, Option<u64>, Vec<String>) {
    match request {
        MailboxRequest::Status => {
            let rev = response.status.as_ref().map(|s| s.revision);
            let summary = match &response.status {
                Some(s) => format!("rev={} entities={}", s.revision, s.entity_count),
                None => response
                    .error
                    .clone()
                    .unwrap_or_else(|| "status".into()),
            };
            ("status".into(), summary, rev, Vec::new())
        }
        MailboxRequest::Inspect { .. } => {
            let (rev, n) = match &response.document {
                Some(doc) => (Some(doc.revision), doc.entity_count()),
                None => (None, 0),
            };
            let summary = if response.ok {
                format!("rev={} entities={}", rev.unwrap_or(0), n)
            } else {
                response
                    .error
                    .clone()
                    .unwrap_or_else(|| "inspect failed".into())
            };
            ("inspect".into(), summary, rev, Vec::new())
        }
        MailboxRequest::Query { query } => {
            let hits = response
                .query
                .as_ref()
                .map(|q| q.hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default();
            let summary = query_headline(query);
            let summary = if response.ok {
                summary
            } else {
                response
                    .error
                    .clone()
                    .or_else(|| response.query.as_ref().and_then(|q| q.error.clone()))
                    .unwrap_or(summary)
            };
            ("query".into(), summary, None, hits)
        }
        MailboxRequest::Apply { request } => {
            let rev = response.apply.as_ref().map(|a| a.revision);
            let summary = summarize_apply(request, response);
            ("apply".into(), summary, rev, Vec::new())
        }
        MailboxRequest::History { action } => {
            let rev = response.history.as_ref().map(|h| h.revision);
            let summary = match &response.history {
                Some(h) if h.ok => format!(
                    "{:?} rev={} undo={} redo={}",
                    action, h.revision, h.undo_depth, h.redo_depth
                ),
                _ => response
                    .error
                    .clone()
                    .or_else(|| response.history.as_ref().and_then(|h| h.error.clone()))
                    .unwrap_or_else(|| "history".into()),
            };
            ("history".into(), summary, rev, Vec::new())
        }
        MailboxRequest::Project { action, .. } => {
            let rev = response.project.as_ref().map(|p| p.revision);
            let summary = match &response.project {
                Some(p) if p.ok => format!("{:?} {}", action, p.path),
                _ => response
                    .error
                    .clone()
                    .unwrap_or_else(|| "project".into()),
            };
            ("project".into(), summary, rev, Vec::new())
        }
        MailboxRequest::Render { .. } => {
            let rev = response.render.as_ref().map(|r| r.revision);
            let summary = match &response.render {
                Some(r) if r.ok => format!("{} {}x{} cam={}", r.path, r.width, r.height, r.camera),
                _ => response.error.clone().unwrap_or_else(|| "render".into()),
            };
            ("render".into(), summary, rev, Vec::new())
        }
        MailboxRequest::Camera { mode, frame, .. } => {
            let summary = if frame.as_ref().map(|f| !f.is_empty()).unwrap_or(false) {
                "frame".into()
            } else {
                mode.clone().unwrap_or_else(|| "camera".into())
            };
            ("camera".into(), summary, None, Vec::new())
        }
        MailboxRequest::Catalog => ("catalog".into(), "live_ops".into(), None, Vec::new()),
        MailboxRequest::Play { action, .. } => ("play".into(), action.clone(), None, Vec::new()),
        MailboxRequest::Rtx { .. } => ("rtx".into(), "lifecycle".into(), None, Vec::new()),
        MailboxRequest::Export { path } => (
            "export".into(),
            path.clone()
                .unwrap_or_else(|| crate::export::DEFAULT_PATH.into()),
            None,
            Vec::new(),
        ),
        MailboxRequest::Validate => {
            let n = response
                .validate
                .as_ref()
                .map(|v| v.diagnostics.len())
                .unwrap_or(0);
            (
                "validate".into(),
                format!("ok={} diags={n}", response.ok),
                None,
                Vec::new(),
            )
        }
        MailboxRequest::Import { path } => (
            "import".into(),
            path.clone()
                .unwrap_or_else(|| crate::export::DEFAULT_PATH.into()),
            response.import.as_ref().map(|i| i.revision),
            Vec::new(),
        ),
        MailboxRequest::Cycles { action, .. } => {
            ("cycles".into(), action.clone(), None, Vec::new())
        }
    }
}

fn query_headline(query: &QuerySpec) -> String {
    match query {
        QuerySpec::LeftOf { entity_id, .. } => format!("left_of {entity_id}"),
        QuerySpec::OnScreen { .. } => "on_screen".into(),
        QuerySpec::ColorOf { color, .. } => {
            let name = match color {
                ColorQuery::Name(n) => n.clone(),
                ColorQuery::Rgba(c) => {
                    format!("rgba({:.2},{:.2},{:.2})", c[0], c[1], c[2])
                }
            };
            format!("color_of {name}")
        }
        QuerySpec::AssemblyOf { entity_id } => format!("assembly_of {entity_id}"),
        QuerySpec::Pixel { x, y, .. } => format!("pixel {x},{y}"),
        QuerySpec::Elements { .. } => "elements".into(),
    }
}

fn summarize_apply(request: &ApplyRequest, response: &MailboxResponse) -> String {
    if !response.ok {
        return response
            .error
            .clone()
            .or_else(|| response.apply.as_ref().and_then(|a| a.error.clone()))
            .unwrap_or_else(|| "apply failed".into());
    }
    let mut parts = Vec::new();
    if !request.label.is_empty() {
        parts.push(request.label.clone());
    }
    if response.apply.as_ref().map(|a| a.idempotent).unwrap_or(false) {
        parts.push("idempotent".into());
    }
    for change in request.changes.iter().take(3) {
        parts.push(match change {
            ApplyChange::CreateMesh { entity } => format!("create {}", entity.id),
            ApplyChange::PatchColor { entity_id, color } => {
                format!(
                    "color {entity_id} [{:.2},{:.2},{:.2}]",
                    color[0], color[1], color[2]
                )
            }
            ApplyChange::PatchTranslation {
                entity_id,
                translation,
            } => format!(
                "move {entity_id} [{:.1},{:.1},{:.1}]",
                translation[0], translation[1], translation[2]
            ),
            ApplyChange::LayoutPattern {
                pattern,
                count,
                id_prefix,
                ..
            } => format!("layout {pattern} {count} {id_prefix}-*"),
            ApplyChange::PatchRotation { entity_id, .. } => format!("rotate {entity_id}"),
            ApplyChange::Group { group_id, .. } => format!("group {group_id}"),
            ApplyChange::Ungroup { group_id } => format!("ungroup {group_id}"),
            ApplyChange::GraphCreate { graph_id } => format!("graph {graph_id}"),
            ApplyChange::GraphPatch {
                graph_id,
                socket,
                value,
                ..
            } => format!("socket {graph_id}.{socket}={value}"),
            ApplyChange::GraphBind {
                graph_id,
                entity_id,
            } => format!("bind {graph_id} -> {entity_id}"),
            ApplyChange::PatchLight { direction, shadows } => {
                let d = direction
                    .map(|v| format!("dir[{:.2},{:.2},{:.2}]", v[0], v[1], v[2]))
                    .unwrap_or_default();
                let s = shadows
                    .map(|b| if b { "shadows on" } else { "shadows off" })
                    .unwrap_or("");
                format!("light {d} {s}")
            }
        });
    }
    if request.changes.len() > 3 {
        parts.push(format!("+{} more", request.changes.len() - 3));
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{ApplyResult, Material, MeshRecipe, Transform};
    use crate::query::{QueryHit, QueryResult};

    fn event(op: &str, rev: Option<u64>) -> FeedEvent {
        FeedEvent {
            op: op.into(),
            ok: true,
            elapsed_ms: 1.0,
            revision: rev,
            summary: "n".into(),
            hits: Vec::new(),
            at: Instant::now(),
        }
    }

    #[test]
    fn ping_is_excluded_from_feed() {
        let mut feed = MailboxFeed::new();
        feed.push(FeedEvent {
            op: "ping".into(),
            ok: true,
            elapsed_ms: 0.1,
            revision: None,
            summary: "pong".into(),
            hits: Vec::new(),
            at: Instant::now(),
        });
        assert!(feed.is_empty());
        assert_eq!(feed.seq, 0);
    }

    #[test]
    fn ring_keeps_last_capacity() {
        let mut feed = MailboxFeed::new();
        for i in 0..(FEED_CAPACITY + 10) {
            feed.push(event("status", Some(i as u64)));
        }
        assert_eq!(feed.len(), FEED_CAPACITY);
        assert_eq!(feed.events.front().unwrap().revision, Some(10));
        assert_eq!(
            feed.events.back().unwrap().revision,
            Some((FEED_CAPACITY + 9) as u64)
        );
        assert_eq!(feed.newest_first().count(), FEED_CAPACITY);
        assert_eq!(
            feed.newest_first().next().unwrap().revision,
            Some((FEED_CAPACITY + 9) as u64)
        );
    }

    #[test]
    fn summarize_query_left_of_is_compact() {
        let req = MailboxRequest::Query {
            query: QuerySpec::LeftOf {
                entity_id: "box-1".into(),
                min_extent: 0.35,
            },
        };
        let resp = MailboxResponse {
            ok: true,
            error: None,
            status: None,
            document: None,
            inspect_summary: None,
            apply: None,
            query: Some(QueryResult {
                ok: true,
                query: "left_of".into(),
                hits: vec![QueryHit::new("box-2", "camera_left")],
                error: None,
            }),
            history: None,
            project: None,
            render: None,
            evidence: None,
            catalog: None,
            play: None,
            rtx: None,
            export: None,
            validate: None,
            import: None,
            cycles: None,
        };
        let (op, summary, _, hits) = summarize(&req, &resp);
        assert_eq!(op, "query");
        assert_eq!(summary, "left_of box-1");
        assert_eq!(hits, vec!["box-2".to_string()]);
        assert!(!summary.contains('{'));
        assert!(!summary.contains(','));
    }

    #[test]
    fn summarize_apply_mentions_label_not_json() {
        let req = MailboxRequest::Apply {
            request: ApplyRequest {
                base_revision: 6,
                idempotency_key: "k".into(),
                label: "nudge left".into(),
                changes: vec![ApplyChange::PatchTranslation {
                    entity_id: "box-2".into(),
                    translation: [-2.0, 0.5, 1.0],
                }],
                dry_run: false,
            },
        };
        let resp = MailboxResponse {
            ok: true,
            error: None,
            status: None,
            document: None,
            inspect_summary: None,
            apply: Some(ApplyResult {
                ok: true,
                revision: 7,
                label: "nudge left".into(),
                idempotent: false,
                error: None,
                code: None,
                current_revision: None,
                dry_run: false,
                pixel_forecast: None,
            }),
            query: None,
            history: None,
            project: None,
            render: None,
            evidence: None,
            catalog: None,
            play: None,
            rtx: None,
            export: None,
            validate: None,
            import: None,
            cycles: None,
        };
        let (op, summary, rev, hits) = summarize(&req, &resp);
        assert_eq!(op, "apply");
        assert_eq!(rev, Some(7));
        assert!(hits.is_empty());
        assert!(summary.contains("nudge left"));
        assert!(summary.contains("box-2"));
        assert!(!summary.contains("idempotency"));
        assert!(!summary.contains('{'));
    }

    #[test]
    fn summarize_inspect_does_not_dump_document() {
        use crate::document::{Document, Entity, Scene};
        let doc = Document {
            revision: 6,
            scenes: vec![Scene {
                id: "main".into(),
                entities: vec![Entity {
                    id: "box-1".into(),
                    kind: "mesh".into(),
                    transform: Transform::default(),
                    mesh: MeshRecipe {
                        recipe: "box".into(),
                        size: [1.0, 1.0, 1.0],
                    },
                    material: Material {
                        color: [1.0, 0.0, 0.0, 1.0],
                    },
                    parent: None,
                    graph_id: None,
                }],
            }],
            idempotency_log: vec![],
            graphs: vec![],
            light: crate::light::SceneLight::default(),
        };
        let req = MailboxRequest::Inspect { slice: None };
        let resp = MailboxResponse {
            ok: true,
            error: None,
            status: None,
            document: Some(doc),
            inspect_summary: None,
            apply: None,
            query: None,
            history: None,
            project: None,
            render: None,
            evidence: None,
            catalog: None,
            play: None,
            rtx: None,
            export: None,
            validate: None,
            import: None,
            cycles: None,
        };
        let (_, summary, _, hits) = summarize(&req, &resp);
        assert_eq!(summary, "rev=6 entities=1");
        assert!(hits.is_empty());
        assert!(!summary.contains("box-1"));
    }

    #[test]
    fn summarize_query_assembly_of_lists_ids_not_comma_jam() {
        let req = MailboxRequest::Query {
            query: QuerySpec::AssemblyOf {
                entity_id: "box-1".into(),
            },
        };
        let resp = MailboxResponse {
            ok: true,
            error: None,
            status: None,
            document: None,
            inspect_summary: None,
            apply: None,
            query: Some(QueryResult {
                ok: true,
                query: "assembly_of".into(),
                hits: vec![
                    QueryHit::new("box-1", "name_prefix:box-1"),
                    QueryHit::new("box-1-hat-brim", "name_prefix:box-1"),
                    QueryHit::new("box-1-eye-l", "name_prefix:box-1"),
                    QueryHit::new("box-1-eye-r", "name_prefix:box-1"),
                ],
                error: None,
            }),
            history: None,
            project: None,
            render: None,
            evidence: None,
            catalog: None,
            play: None,
            rtx: None,
            export: None,
            validate: None,
            import: None,
            cycles: None,
        };
        let (op, summary, _, hits) = summarize(&req, &resp);
        assert_eq!(op, "query");
        assert_eq!(summary, "assembly_of box-1");
        assert!(!summary.contains(','));
        assert!(!summary.contains("box-1-hat-brim"));
        assert_eq!(
            hits,
            vec![
                "box-1".to_string(),
                "box-1-hat-brim".to_string(),
                "box-1-eye-l".to_string(),
                "box-1-eye-r".to_string(),
            ]
        );
        let jammed = format!("assembly_of box-1 -> {}", hits.join(","));
        assert_ne!(summary, jammed);
    }
}
