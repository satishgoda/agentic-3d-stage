//! Local mailbox: JSON lines over TCP and a local socket (Windows named pipe / Unix UDS).
//! Token-auth on every line (B1).

use crate::capabilities;
use crate::document::{
    ApplyRequest, ApplyResult, Document, DocumentStore, HistoryResult, InspectSummary,
    ProjectResult, DEFAULT_TOKEN_PATH,
};
use crate::feed::{FeedEvent, MailboxFeed};
use crate::query::{run_query, QueryResult, QuerySpec};
use serde::{Deserialize, Serialize};
use interprocess::local_socket::{
    prelude::*, GenericNamespaced, ListenerOptions, Stream as LocalStream, ToNsName,
};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Namespaced local socket (Windows named pipe / Unix abstract or ns name).
pub const DEFAULT_LOCAL_NAME: &str = "thinner-floor";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MailboxRequest {
    Status,
    Inspect {
        #[serde(default)]
        slice: Option<String>,
    },
    Apply {
        #[serde(flatten)]
        request: ApplyRequest,
    },
    Query {
        query: QuerySpec,
    },
    History {
        action: HistoryAction,
    },
    Project {
        action: ProjectAction,
        #[serde(default)]
        path: Option<String>,
    },
    Render {
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
    Camera {
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        review_eye: Option<[f32; 3]>,
        #[serde(default)]
        review_target: Option<[f32; 3]>,
        #[serde(default)]
        frame: Option<Vec<String>>,
    },
    Catalog,
    Play {
        action: String,
        #[serde(default)]
        time: Option<f32>,
    },
    Rtx {
        #[serde(default)]
        requested: Option<bool>,
    },
    Export {
        #[serde(default)]
        path: Option<String>,
    },
    Validate,
    Import {
        #[serde(default)]
        path: Option<String>,
    },
    Cycles {
        action: String,
        #[serde(default)]
        samples: Option<u32>,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryAction {
    List,
    Undo,
    Redo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAction {
    List,
    Save,
    Open,
    Create,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPayload {
    pub revision: u64,
    pub entity_count: usize,
    pub scene_count: usize,
    pub document_path: String,
    pub mailbox: String,
    pub local_mailbox: String,
    pub capabilities: capabilities::Capabilities,
    pub auth: String,
    pub undo_depth: usize,
    pub redo_depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_evidence: Option<crate::beauty::BeautyDigest>,
    pub viewport: crate::camera::ViewportCameras,
    pub light: crate::light::SceneLight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<Document>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspect_summary: Option<InspectSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply: Option<ApplyResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<QueryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<crate::beauty::RenderResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<crate::beauty::BeautyDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play: Option<crate::play::PlayState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtx: Option<crate::rtx::RtxState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<crate::export::ExportResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validate: Option<crate::document::ValidateResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import: Option<crate::export::ImportResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycles: Option<crate::cycles_stream::CyclesStatus>,
}

impl MailboxResponse {
    fn bad(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            status: None,
            document: None,
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
        }
    }

    fn empty_ok() -> Self {
        Self {
            ok: true,
            error: None,
            status: None,
            document: None,
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
        }
    }
}

pub fn generate_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mix = nanos ^ (pid << 48) ^ nanos.rotate_left(23);
    format!("{mix:032x}")
}

pub fn write_token_file(path: impl AsRef<Path>, token: &str) -> std::io::Result<()> {
    fs::write(path, token)
}

pub fn read_token_file(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn load_client_token() -> String {
    std::env::var("THINNER_FLOOR_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| read_token_file(DEFAULT_TOKEN_PATH))
        .unwrap_or_default()
}

pub struct MailboxServer {
    pub addr: String,
    pub local: String,
}

impl MailboxServer {
    /// TCP + local socket (named pipe on Windows). Tests may call `spawn_tcp` only.
    pub fn spawn(
        addr: impl Into<String>,
        store: Arc<Mutex<DocumentStore>>,
        feed: Arc<Mutex<MailboxFeed>>,
        token: String,
    ) -> std::io::Result<Self> {
        let tcp = Self::spawn_tcp(addr, Arc::clone(&store), Arc::clone(&feed), token.clone())?;
        let local = DEFAULT_LOCAL_NAME.to_string();
        if let Err(e) = Self::spawn_local(
            &local,
            store,
            feed,
            token,
            tcp.addr.clone(),
        ) {
            eprintln!("mailbox local socket ({local}): {e}");
        }
        Ok(Self {
            addr: tcp.addr,
            local,
        })
    }

    pub fn spawn_tcp(
        addr: impl Into<String>,
        store: Arc<Mutex<DocumentStore>>,
        feed: Arc<Mutex<MailboxFeed>>,
        token: String,
    ) -> std::io::Result<Self> {
        let addr = addr.into();
        let listener = TcpListener::bind(&addr)?;
        let bound = listener.local_addr()?.to_string();
        let store_for_thread = Arc::clone(&store);
        let feed_for_thread = Arc::clone(&feed);
        let bound_for_thread = bound.clone();

        thread::Builder::new()
            .name("thinner-floor-mailbox-tcp".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let _ = stream.set_nodelay(true);
                            let store = Arc::clone(&store_for_thread);
                            let feed = Arc::clone(&feed_for_thread);
                            let mailbox = bound_for_thread.clone();
                            let token = token.clone();
                            let _ = thread::spawn(move || {
                                if let Err(e) = handle_client(stream, store, feed, &mailbox, &token)
                                {
                                    eprintln!("mailbox client error: {e}");
                                }
                            });
                        }
                        Err(e) => eprintln!("mailbox accept error: {e}"),
                    }
                }
            })?;

        Ok(Self {
            addr: bound,
            local: String::new(),
        })
    }

    fn spawn_local(
        name: &str,
        store: Arc<Mutex<DocumentStore>>,
        feed: Arc<Mutex<MailboxFeed>>,
        token: String,
        tcp_addr: String,
    ) -> std::io::Result<()> {
        let ns = name
            .to_ns_name::<GenericNamespaced>()
            .map_err(io_other)?;
        let listener = ListenerOptions::new().name(ns).create_sync()?;
        let store_for_thread = store;
        let feed_for_thread = feed;
        let name_owned = name.to_string();
        thread::Builder::new()
            .name("thinner-floor-mailbox-local".into())
            .spawn(move || {
                loop {
                    match listener.accept() {
                        Ok(stream) => {
                            let store = Arc::clone(&store_for_thread);
                            let feed = Arc::clone(&feed_for_thread);
                            let mailbox = tcp_addr.clone();
                            let token = token.clone();
                            let _ = thread::spawn(move || {
                                if let Err(e) =
                                    handle_client(stream, store, feed, &mailbox, &token)
                                {
                                    eprintln!("mailbox local client error: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            eprintln!("mailbox local accept error: {e}");
                            if name_owned.is_empty() {
                                break;
                            }
                        }
                    }
                }
            })?;
        Ok(())
    }
}

fn io_other(err: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string())
}

fn read_line_stream(stream: &mut impl Read) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = stream.read(&mut b)?;
        if n == 0 {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
            };
        }
        buf.push(b[0]);
        if b[0] == b'\n' {
            break;
        }
        if buf.len() > 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "mailbox line over 1 MiB",
            ));
        }
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn handle_client<S: Read + Write>(
    mut stream: S,
    store: Arc<Mutex<DocumentStore>>,
    feed: Arc<Mutex<MailboxFeed>>,
    mailbox: &str,
    expected_token: &str,
) -> std::io::Result<()> {
    loop {
        let Some(line) = read_line_stream(&mut stream)? else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let started = Instant::now();
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => {
                let presented = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
                if presented != expected_token {
                    MailboxResponse::bad("unauthorized")
                } else {
                    match serde_json::from_value::<MailboxRequest>(v) {
                        Ok(req) => {
                            let response = dispatch(req.clone(), &store, mailbox);
                            if let Ok(mut feed) = feed.lock() {
                                feed.record_exchange(&req, &response, started.elapsed());
                            }
                            response
                        }
                        Err(e) => MailboxResponse::bad(format!("bad_request:{e}")),
                    }
                }
            }
            Err(e) => {
                let response = MailboxResponse::bad(format!("bad_request:{e}"));
                if let Ok(mut feed) = feed.lock() {
                    feed.push(FeedEvent {
                        op: "error".into(),
                        ok: false,
                        elapsed_ms: started.elapsed().as_secs_f32() * 1000.0,
                        revision: None,
                        summary: "bad_request".into(),
                        hits: Vec::new(),
                        at: Instant::now(),
                    });
                }
                response
            }
        };

        let mut out = serde_json::to_string(&response)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        out.push('\n');
        stream.write_all(out.as_bytes())?;
        stream.flush()?;
    }
    Ok(())
}

fn refresh_cycles_overlay(doc: &crate::document::Document) {
    if let Ok(mut host) = crate::cycles_stream::host().lock() {
        host.refresh_if_overlay(doc);
    }
}

fn dispatch(
    req: MailboxRequest,
    store: &Arc<Mutex<DocumentStore>>,
    mailbox: &str,
) -> MailboxResponse {
    let mut guard = match store.lock() {
        Ok(g) => g,
        Err(_) => return MailboxResponse::bad("store_poisoned"),
    };

    match req {
        MailboxRequest::Status => {
            let mut r = MailboxResponse::empty_ok();
            r.status = Some(StatusPayload {
                revision: guard.document.revision,
                entity_count: guard.document.entity_count(),
                scene_count: guard.document.scenes.len(),
                document_path: guard.path.display().to_string(),
                mailbox: mailbox.to_string(),
                local_mailbox: DEFAULT_LOCAL_NAME.into(),
                capabilities: capabilities::live(),
                auth: "token".into(),
                undo_depth: guard.undo_depth(),
                redo_depth: guard.redo_depth(),
                latest_evidence: crate::beauty::last_digest(),
                viewport: *crate::camera::rig(),
                light: guard.document.light.clone(),
            });
            r
        }
        MailboxRequest::Inspect { slice } => {
            let mut r = MailboxResponse::empty_ok();
            let kind = slice.as_deref().unwrap_or("full");
            if kind.eq_ignore_ascii_case("summary") {
                r.inspect_summary = Some(guard.document.inspect_summary());
            } else if kind.eq_ignore_ascii_case("catalog") {
                r.catalog = Some(crate::catalog::live());
            } else if kind.eq_ignore_ascii_case("evidence") {
                drop(guard);
                let mut s = MailboxResponse::empty_ok();
                match crate::beauty::last_digest() {
                    Some(d) => s.evidence = Some(d),
                    None => return MailboxResponse::bad("no_evidence"),
                }
                return s;
            } else {
                r.document = Some(guard.document.clone());
            }
            r
        }
        MailboxRequest::Apply { request } => match guard.apply(&request) {
            Ok(result) => {
                let mut r = MailboxResponse::empty_ok();
                r.ok = result.ok;
                r.error = result.error.clone();
                let refresh = result.ok && !result.idempotent && !result.dry_run;
                r.apply = Some(result);
                if refresh {
                    let doc = guard.document.clone();
                    drop(guard);
                    refresh_cycles_overlay(&doc);
                }
                r
            }
            Err(e) => MailboxResponse::bad(format!("io:{e}")),
        },
        MailboxRequest::Query { query } => {
            let result = run_query(&guard.document, &query);
            let mut r = MailboxResponse::empty_ok();
            r.ok = result.ok;
            r.error = result.error.clone();
            r.query = Some(result);
            r
        }
        MailboxRequest::History { action } => {
            let refresh = !matches!(action, HistoryAction::List);
            let result = match action {
                HistoryAction::List => guard.history_list(),
                HistoryAction::Undo => match guard.undo() {
                    Ok(h) => h,
                    Err(e) => return MailboxResponse::bad(format!("io:{e}")),
                },
                HistoryAction::Redo => match guard.redo() {
                    Ok(h) => h,
                    Err(e) => return MailboxResponse::bad(format!("io:{e}")),
                },
            };
            let mut r = MailboxResponse::empty_ok();
            r.ok = result.ok;
            r.error = result.error.clone();
            r.history = Some(result);
            if r.ok && refresh {
                let doc = guard.document.clone();
                drop(guard);
                refresh_cycles_overlay(&doc);
            }
            r
        }
        MailboxRequest::Project { action, path } => {
            let refresh = matches!(action, ProjectAction::Open | ProjectAction::Create);
            let result = match action {
                ProjectAction::List => guard.project_list(),
                ProjectAction::Save => match guard.project_save() {
                    Ok(p) => p,
                    Err(e) => return MailboxResponse::bad(format!("io:{e}")),
                },
                ProjectAction::Open => {
                    let Some(path) = path else {
                        return MailboxResponse::bad("project_open_needs_path");
                    };
                    match guard.project_open(path) {
                        Ok(p) => p,
                        Err(e) => return MailboxResponse::bad(format!("io:{e}")),
                    }
                }
                ProjectAction::Create => {
                    let Some(path) = path else {
                        return MailboxResponse::bad("project_create_needs_path");
                    };
                    match guard.project_create(path) {
                        Ok(p) => p,
                        Err(e) => return MailboxResponse::bad(format!("io:{e}")),
                    }
                }
            };
            let mut r = MailboxResponse::empty_ok();
            r.ok = result.ok;
            r.error = result.error.clone();
            r.project = Some(result);
            if r.ok && refresh {
                let doc = guard.document.clone();
                drop(guard);
                refresh_cycles_overlay(&doc);
            }
            r
        }
        MailboxRequest::Render { width, height } => {
            let w = width.unwrap_or(crate::beauty::DEFAULT_WIDTH);
            let h = height.unwrap_or(crate::beauty::DEFAULT_HEIGHT);
            let doc = guard.document.clone();
            drop(guard);
            match crate::beauty::render_to_default_png(&doc, w, h) {
                Ok(result) => {
                    let mut r = MailboxResponse::empty_ok();
                    r.render = Some(result);
                    r
                }
                Err(e) => MailboxResponse::bad(e),
            }
        }
        MailboxRequest::Catalog => {
            let mut r = MailboxResponse::empty_ok();
            r.catalog = Some(crate::catalog::live());
            r
        }
        MailboxRequest::Camera {
            mode,
            review_eye,
            review_target,
            frame,
        } => {
            if let Some(ids) = frame.as_ref() {
                if !ids.is_empty() {
                    let mut pts = Vec::new();
                    for id in ids {
                        if let Some(t) = guard.document.world_translation(id) {
                            pts.push(glam::Vec3::from_array(t));
                        }
                    }
                    if !pts.is_empty() {
                        let n = pts.len() as f32;
                        let c = pts.iter().copied().fold(glam::Vec3::ZERO, |a, b| a + b) / n;
                        let radius = pts
                            .iter()
                            .map(|p| (*p - c).length())
                            .fold(0.0f32, f32::max)
                            + 0.5;
                        crate::camera::frame_authored(c, radius);
                    }
                }
            }
            drop(guard);
            {
                let mut rig = crate::camera::rig();
                if let Some(m) = mode.as_deref() {
                    match m {
                        "authored" => rig.mode = crate::camera::ViewMode::Authored,
                        "review" => rig.mode = crate::camera::ViewMode::Review,
                        other => {
                            return MailboxResponse::bad(format!("unknown_view_mode:{other}"));
                        }
                    }
                }
                if let Some(eye) = review_eye {
                    rig.review.eye = eye;
                }
                if let Some(target) = review_target {
                    rig.review.target = target;
                }
            }
            dispatch(MailboxRequest::Status, store, mailbox)
        }
        MailboxRequest::Play { action, time } => match crate::play::apply(&action, time) {
            Ok(p) => {
                let mut r = MailboxResponse::empty_ok();
                r.play = Some(p);
                r
            }
            Err(e) => MailboxResponse::bad(e),
        },
        MailboxRequest::Rtx { requested } => {
            if let Some(req) = requested {
                crate::rtx::set_requested(req);
            }
            let mut r = MailboxResponse::empty_ok();
            r.rtx = Some(crate::rtx::snapshot());
            r
        }
        MailboxRequest::Export { path } => {
            let path = path.unwrap_or_else(|| crate::export::DEFAULT_PATH.to_string());
            let doc = guard.document.clone();
            drop(guard);
            match crate::export::write_gltf(&doc, &path) {
                Ok(result) => {
                    let mut r = MailboxResponse::empty_ok();
                    r.export = Some(result);
                    r
                }
                Err(e) => MailboxResponse::bad(e),
            }
        }
        MailboxRequest::Validate => {
            let v = guard.document.validate();
            let mut r = MailboxResponse::empty_ok();
            r.ok = v.ok;
            if !v.ok {
                r.error = Some("validate_failed".into());
            }
            r.validate = Some(v);
            r
        }
        MailboxRequest::Import { path } => {
            let path = path.unwrap_or_else(|| crate::export::DEFAULT_PATH.to_string());
            match guard.import_gltf(&path) {
                Ok(result) => {
                    let mut r = MailboxResponse::empty_ok();
                    r.import = Some(result);
                    let doc = guard.document.clone();
                    drop(guard);
                    refresh_cycles_overlay(&doc);
                    r
                }
                Err(e) => MailboxResponse::bad(e),
            }
        }
        MailboxRequest::Cycles {
            action,
            samples,
            width,
            height,
        } => {
            let doc = guard.document.clone();
            drop(guard);
            let mut host = crate::cycles_stream::host().lock().unwrap();
            let st = match action.as_str() {
                "start" => host.start(&doc, samples.unwrap_or(64), width.unwrap_or(960), height.unwrap_or(640)),
                "pause" => host.pause(),
                "resume" => host.resume(),
                "stop" => host.stop(),
                "status" => host.snapshot(),
                other => crate::cycles_stream::CyclesStatus {
                    ok: false,
                    state: "error".into(),
                    sample: 0,
                    width: 0,
                    height: 0,
                    overlay: host.overlay_on(),
                    error: Some(format!("unknown_cycles_action:{other}")),
                },
            };
            let mut r = MailboxResponse::empty_ok();
            r.ok = st.ok;
            if !st.ok {
                r.error = st.error.clone();
            }
            r.cycles = Some(st);
            r
        }
    }
}

pub struct MailboxClient {
    addr: String,
    token: String,
}

fn is_tcp_addr(addr: &str) -> bool {
    if addr.starts_with("pipe:") || addr.starts_with(r"\\.\pipe\") {
        return false;
    }
    addr.contains(':') && addr.chars().any(|c| c.is_ascii_digit())
}

enum MailboxIo {
    Tcp(TcpStream),
    Local(LocalStream),
}

impl Read for MailboxIo {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(buf),
            Self::Local(s) => s.read(buf),
        }
    }
}

impl Write for MailboxIo {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(buf),
            Self::Local(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            Self::Local(s) => s.flush(),
        }
    }
}

fn open_stream(addr: &str) -> std::io::Result<MailboxIo> {
    if is_tcp_addr(addr) {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(MailboxIo::Tcp(stream))
    } else {
        let name = addr.strip_prefix("pipe:").unwrap_or(addr);
        let ns = name.to_ns_name::<GenericNamespaced>().map_err(io_other)?;
        Ok(MailboxIo::Local(LocalStream::connect(ns)?))
    }
}

impl MailboxClient {
    pub fn connect(addr: impl AsRef<str>) -> std::io::Result<Self> {
        Self::connect_with_token(addr, load_client_token())
    }

    pub fn connect_with_token(
        addr: impl AsRef<str>,
        token: impl Into<String>,
    ) -> std::io::Result<Self> {
        let display = addr.as_ref().to_string();
        drop(open_stream(&display)?);
        Ok(Self {
            addr: display,
            token: token.into(),
        })
    }

    /// Prefer the local socket (named pipe / UDS), then TCP.
    pub fn connect_sit(tcp_addr: &str) -> std::io::Result<Self> {
        match Self::connect(DEFAULT_LOCAL_NAME) {
            Ok(c) => Ok(c),
            Err(_) => Self::connect(tcp_addr),
        }
    }

    pub fn request(&self, req: &MailboxRequest) -> std::io::Result<MailboxResponse> {
        let mut stream = open_stream(&self.addr)?;
        let mut v = serde_json::to_value(req)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(obj) = v.as_object_mut() {
            if !self.token.is_empty() {
                obj.insert("token".into(), Value::String(self.token.clone()));
            }
        }
        let mut line = serde_json::to_string(&v)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        stream.flush()?;
        let response_line = read_line_stream(&mut stream)?.unwrap_or_default();
        serde_json::from_str(response_line.trim()).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad mailbox response: {e}; body={response_line:?}"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{
        ApplyChange, ApplyRequest, DocumentStore, Entity, Material, MeshRecipe, Transform,
    };
    use crate::query::{QuerySpec, DEFAULT_MIN_EXTENT};
    use std::sync::{Arc, Mutex};

    fn mesh(id: &str, t: [f32; 3], color: [f32; 4]) -> Entity {
        Entity {
            id: id.into(),
            kind: "mesh".into(),
            transform: Transform {
                translation: t,
                ..Default::default()
            },
            mesh: MeshRecipe {
                recipe: "box".into(),
                size: [1.0, 1.0, 1.0],
            },
            material: Material { color },
            parent: None,
                    graph_id: None,
        }
    }

    fn temp_store() -> (std::path::PathBuf, Arc<Mutex<DocumentStore>>, String) {
        let dir = std::env::temp_dir().join(format!(
            "tf-b-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scene.json");
        let store = Arc::new(Mutex::new(DocumentStore::open(&path).unwrap()));
        let token = generate_token();
        (path, store, token)
    }

    #[test]
    fn b_acceptance_two_boxes_query_undo_save_reopen() {
        let (path, store, token) = temp_store();
        let feed = Arc::new(Mutex::new(MailboxFeed::new()));
        let server =
            MailboxServer::spawn_tcp("127.0.0.1:0", Arc::clone(&store), feed, token.clone())
                .unwrap();
        let client = MailboxClient::connect_with_token(&server.addr, token.clone()).unwrap();

        let yellow = [0.88, 0.68, 0.14, 1.0];
        let red = [0.78, 0.18, 0.14, 1.0];
        let mut left = mesh("$left", [-2.0, 0.5, 0.0], yellow);
        left.id = "$left".into();
        let mut right = mesh("$right", [2.0, 0.5, 0.0], red);
        right.id = "$right".into();

        let created = client
            .request(&MailboxRequest::Apply {
                request: ApplyRequest {
                    base_revision: 1,
                    idempotency_key: "b-two-boxes".into(),
                    label: "create left and right".into(),
                    changes: vec![
                        ApplyChange::CreateMesh { entity: left },
                        ApplyChange::CreateMesh { entity: right },
                    ],
                    dry_run: false,
                },
            })
            .unwrap();
        assert!(created.ok, "{created:?}");
        assert_eq!(created.apply.as_ref().unwrap().revision, 2);

        let painted = client
            .request(&MailboxRequest::Apply {
                request: ApplyRequest {
                    base_revision: 2,
                    idempotency_key: "b-color".into(),
                    label: "color both blue".into(),
                    changes: vec![
                        ApplyChange::PatchColor {
                            entity_id: "$left".into(),
                            color: [0.2, 0.45, 0.9, 1.0],
                        },
                        ApplyChange::PatchColor {
                            entity_id: "$right".into(),
                            color: [0.2, 0.45, 0.9, 1.0],
                        },
                    ],
                    dry_run: false,
                },
            })
            .unwrap();
        assert!(painted.ok, "{painted:?}");
        assert_eq!(painted.apply.as_ref().unwrap().revision, 3);

        let left_of = client
            .request(&MailboxRequest::Query {
                query: QuerySpec::LeftOf {
                    entity_id: "right".into(),
                    min_extent: DEFAULT_MIN_EXTENT,
                },
            })
            .unwrap();
        assert!(left_of.ok, "{left_of:?}");
        let ids: Vec<_> = left_of
            .query
            .as_ref()
            .unwrap()
            .hits
            .iter()
            .map(|h| h.id.as_str())
            .collect();
        assert!(ids.contains(&"left"), "{ids:?}");

        let on_screen = client
            .request(&MailboxRequest::Query {
                query: QuerySpec::OnScreen {
                    min_extent: DEFAULT_MIN_EXTENT,
                    aspect: crate::camera::DEFAULT_ASPECT,
                },
            })
            .unwrap();
        assert!(on_screen.ok, "{on_screen:?}");
        let screen: Vec<_> = on_screen
            .query
            .as_ref()
            .unwrap()
            .hits
            .iter()
            .map(|h| h.id.as_str())
            .collect();
        assert!(screen.contains(&"left") && screen.contains(&"right"), "{screen:?}");

        let undid = client
            .request(&MailboxRequest::History {
                action: HistoryAction::Undo,
            })
            .unwrap();
        assert!(undid.ok, "{undid:?}");
        assert_eq!(undid.history.as_ref().unwrap().revision, 4);

        let saved = client
            .request(&MailboxRequest::Project {
                action: ProjectAction::Save,
                path: None,
            })
            .unwrap();
        assert!(saved.ok, "{saved:?}");
        assert_eq!(saved.project.as_ref().unwrap().revision, 4);

        let opened = client
            .request(&MailboxRequest::Project {
                action: ProjectAction::Open,
                path: Some(path.display().to_string()),
            })
            .unwrap();
        assert!(opened.ok, "{opened:?}");
        assert_eq!(opened.project.as_ref().unwrap().revision, 4);

        let st = client.request(&MailboxRequest::Status).unwrap();
        assert_eq!(st.status.as_ref().unwrap().revision, 4);
        let doc = client
            .request(&MailboxRequest::Inspect { slice: None })
            .unwrap()
            .document
            .unwrap();
        assert_eq!(doc.find_entity("left").unwrap().material.color, yellow);
        assert_eq!(doc.find_entity("right").unwrap().material.color, red);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn local_socket_status_roundtrip() {
        let (_path, store, token) = temp_store();
        let feed = Arc::new(Mutex::new(MailboxFeed::new()));
        let name = format!("tf-b1-{}", std::process::id());
        MailboxServer::spawn_local(
            &name,
            store,
            feed,
            token.clone(),
            "local-test".into(),
        )
        .unwrap();
        // Accept loop needs a tick on a slow box.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let client = MailboxClient::connect_with_token(&name, token).unwrap();
        let st = client.request(&MailboxRequest::Status).unwrap();
        assert!(st.ok, "{st:?}");
        assert_eq!(st.status.as_ref().unwrap().auth, "token");
        assert_eq!(st.status.as_ref().unwrap().local_mailbox, DEFAULT_LOCAL_NAME);
    }

    #[test]
    fn export_writes_gltf_via_mailbox() {
        let (path, store, token) = temp_store();
        let feed = Arc::new(Mutex::new(MailboxFeed::new()));
        let server =
            MailboxServer::spawn_tcp("127.0.0.1:0", Arc::clone(&store), feed, token.clone())
                .unwrap();
        let client = MailboxClient::connect_with_token(&server.addr, token).unwrap();
        let gltf = path.parent().unwrap().join("out.gltf");
        let resp = client
            .request(&MailboxRequest::Export {
                path: Some(gltf.display().to_string()),
            })
            .unwrap();
        assert!(resp.ok, "{resp:?}");
        let ex = resp.export.as_ref().unwrap();
        assert_eq!(ex.mesh_count, 2);
        assert!(ex.byte_length > 0);
        let text = fs::read_to_string(&gltf).unwrap();
        assert!(text.contains("\"asset\""));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
