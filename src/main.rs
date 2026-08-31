//! thinner-floor — thin native authoring viewport for Satish Goda.
//!
//! One binary:
//! - `cargo run` opens the wgpu window + local mailbox
//! - `cargo run -- status|inspect|query|apply ...` talks to that mailbox
//! - `cargo run -- --mcp` stdio MCP adapter (sit must already be up)

mod render;

use std::env;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use thinner_floor::document::{
    ApplyChange, ApplyRequest, DocumentStore, DEFAULT_DOCUMENT_PATH, DEFAULT_MAILBOX_ADDR,
    DEFAULT_TOKEN_PATH,
};
use thinner_floor::mailbox::{
    generate_token, write_token_file, HistoryAction, MailboxClient, MailboxRequest, MailboxServer,
    ProjectAction,
};
use thinner_floor::query::{ColorQuery, QuerySpec, DEFAULT_MIN_EXTENT};
use thinner_floor::camera::DEFAULT_ASPECT;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("thinner-floor error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut document_path = DEFAULT_DOCUMENT_PATH.to_string();
    let mut mailbox_addr = DEFAULT_MAILBOX_ADDR.to_string();
    let mut mcp = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--document" => {
                document_path = args
                    .get(i + 1)
                    .ok_or("--document needs a path")?
                    .clone();
                args.drain(i..=i + 1);
            }
            "--mailbox" => {
                mailbox_addr = args
                    .get(i + 1)
                    .ok_or("--mailbox needs host:port")?
                    .clone();
                args.drain(i..=i + 1);
            }
            "--mcp" => {
                mcp = true;
                args.remove(i);
            }
            _ => i += 1,
        }
    }

    if mcp || args.first().map(|s| s.as_str()) == Some("mcp") {
        return thinner_floor::mcp::serve_stdio(&mailbox_addr).map_err(|e| e.into());
    }

    if args.is_empty() {
        return run_viewport(document_path, mailbox_addr);
    }

    match args[0].as_str() {
        "status" => client_status(&mailbox_addr),
        "inspect" => client_inspect(&mailbox_addr, &args[1..]),
        "query" => client_query(&mailbox_addr, &args[1..]),
        "apply" => client_apply(&mailbox_addr, &args[1..]),
        "history" => client_history(&mailbox_addr, &args[1..]),
        "project" => client_project(&mailbox_addr, &args[1..]),
        "render" => client_render(&mailbox_addr, &args[1..]),
        "camera" => client_camera(&mailbox_addr, &args[1..]),
        "play" => {
            let action = args.get(1).cloned().unwrap_or_else(|| "stop".into());
            let time = flag_value(&args[1..], "--time").and_then(|s| s.parse().ok());
            let client = MailboxClient::connect(&mailbox_addr)?;
            let resp = client.request(&MailboxRequest::Play { action, time })?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        "rtx" => {
            let requested = flag_value(&args[1..], "--requested").and_then(|s| match s.as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            });
            let client = MailboxClient::connect(&mailbox_addr)?;
            let resp = client.request(&MailboxRequest::Rtx { requested })?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        "catalog" => {
            let client = MailboxClient::connect(&mailbox_addr)?;
            let resp = client.request(&MailboxRequest::Catalog)?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            Ok(())
        }
        "export" => {
            let path = flag_value(&args[1..], "--path").or_else(|| args.get(1).cloned());
            let client = MailboxClient::connect(&mailbox_addr)?;
            let resp = client.request(&MailboxRequest::Export { path })?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            if !resp.ok {
                return Err(resp.error.unwrap_or_else(|| "export failed".into()).into());
            }
            Ok(())
        }
        "import" => {
            let path = flag_value(&args[1..], "--path").or_else(|| args.get(1).cloned());
            let client = MailboxClient::connect(&mailbox_addr)?;
            let resp = client.request(&MailboxRequest::Import { path })?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            if !resp.ok {
                return Err(resp.error.unwrap_or_else(|| "import failed".into()).into());
            }
            Ok(())
        }
        "light" => {
            let dir = flag_value(&args[1..], "--dir").and_then(|s| parse_vec3(&s));
            let shadows = flag_value(&args[1..], "--shadows").and_then(|s| match s.as_str() {
                "true" | "1" | "on" => Some(true),
                "false" | "0" | "off" => Some(false),
                _ => None,
            });
            let client = MailboxClient::connect(&mailbox_addr)?;
            let st = client.request(&MailboxRequest::Status)?;
            let rev = st.status.as_ref().map(|s| s.revision).unwrap_or(0);
            let key = format!("light-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
            let resp = client.request(&MailboxRequest::Apply {
                request: ApplyRequest {
                    base_revision: rev,
                    idempotency_key: key,
                    label: "patch light".into(),
                    changes: vec![ApplyChange::PatchLight {
                        direction: dir,
                        shadows,
                    }],
                    dry_run: false,
                },
            })?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            if !resp.ok {
                return Err(resp.error.unwrap_or_else(|| "light failed".into()).into());
            }
            Ok(())
        }
        "cycles" => {
            let action = args.get(1).cloned().unwrap_or_else(|| "status".into());
            let samples = flag_value(&args[1..], "--samples").and_then(|s| s.parse().ok());
            let client = MailboxClient::connect(&mailbox_addr)?;
            let resp = client.request(&MailboxRequest::Cycles {
                action,
                samples,
                width: None,
                height: None,
            })?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            if !resp.ok {
                return Err(resp.error.unwrap_or_else(|| "cycles failed".into()).into());
            }
            Ok(())
        }
        "validate" => {
            let client = MailboxClient::connect(&mailbox_addr)?;
            let resp = client.request(&MailboxRequest::Validate)?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
            if !resp.ok {
                return Err(resp.error.unwrap_or_else(|| "validate failed".into()).into());
            }
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command: {other}").into()),
    }
}

fn run_viewport(
    document_path: String,
    mailbox_addr: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(Mutex::new(DocumentStore::open(&document_path)?));
    let revision = store.lock().unwrap().document.revision;
    let entities = store.lock().unwrap().document.entity_count();

    let feed = Arc::new(Mutex::new(thinner_floor::MailboxFeed::new()));
    let token = generate_token();
    write_token_file(DEFAULT_TOKEN_PATH, &token)?;
    let server = MailboxServer::spawn(
        mailbox_addr.clone(),
        Arc::clone(&store),
        Arc::clone(&feed),
        token,
    )?;
    println!("thinner-floor");
    println!("  document : {document_path} (revision {revision}, {entities} entities)");
    println!("  mailbox  : {}", server.addr);
    println!("  local    : {}  (named pipe / UDS)", server.local);
    println!("  token    : {DEFAULT_TOKEN_PATH}  (not printed)");
    println!("  window   : Thinner Floor (wgpu)");
    println!("  live feed: title bar (click + to expand, Ctrl+Shift+M hides)");
    println!();
    println!("Look:  cargo run -- status");
    println!("       cargo run -- inspect");
    println!("       cargo run -- query left-of --entity box-1");
    println!("       cargo run -- query color-of --color yellow");
    println!("       cargo run -- query assembly-of --entity box-1");
    println!("       cargo run -- query on-screen");
    println!("Change (after looking at revision):");
    println!(
        "  cargo run -- apply --base-revision {revision} --idempotency-key paint-1 --label \"paint box blue\" \\"
    );
    println!(
        "    --change '{{\"op\":\"patch_color\",\"entityId\":\"box-1\",\"color\":[0.2,0.45,0.9,1.0]}}'"
    );

    let stop = Arc::new(AtomicBool::new(false));
    render::ViewportApp::run(store, feed, stop)?;
    Ok(())
}

fn client_status(mailbox: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = MailboxClient::connect(mailbox)?;
    let resp = client.request(&MailboxRequest::Status)?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "status failed".into()).into());
    }
    Ok(())
}

fn client_inspect(mailbox: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let slice = flag_value(args, "--slice");
    let client = MailboxClient::connect(mailbox)?;
    let resp = client.request(&MailboxRequest::Inspect { slice })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp
            .error
            .unwrap_or_else(|| "inspect failed".into())
            .into());
    }
    Ok(())
}

fn client_query(mailbox: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let spec = parse_query_args(args)?;
    let client = MailboxClient::connect(mailbox)?;
    let resp = client.request(&MailboxRequest::Query { query: spec })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp
            .error
            .or_else(|| resp.query.and_then(|q| q.error))
            .unwrap_or_else(|| "query failed".into())
            .into());
    }
    Ok(())
}

fn parse_query_args(args: &[String]) -> Result<QuerySpec, Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("query needs a kind: left-of | on-screen | color-of | assembly-of | pixel".into());
    }

    // Allow a raw JSON query blob: cargo run -- query '{"op":"left_of",...}'
    if args.len() == 1 && args[0].trim_start().starts_with('{') {
        return Ok(serde_json::from_str(&args[0])?);
    }

    let kind = args[0].as_str();
    let rest = &args[1..];
    match kind {
        "left-of" | "left_of" => {
            let entity_id = flag_value(rest, "--entity")
                .or_else(|| rest.first().cloned())
                .ok_or("left-of needs --entity ID")?;
            Ok(QuerySpec::LeftOf {
                entity_id,
                min_extent: flag_f32(rest, "--min-extent").unwrap_or(DEFAULT_MIN_EXTENT),
            })
        }
        "on-screen" | "on_screen" => Ok(QuerySpec::OnScreen {
            min_extent: flag_f32(rest, "--min-extent").unwrap_or(DEFAULT_MIN_EXTENT),
            aspect: flag_f32(rest, "--aspect").unwrap_or(DEFAULT_ASPECT),
        }),
        "color-of" | "color_of" => {
            let raw = flag_value(rest, "--color")
                .or_else(|| rest.first().cloned())
                .ok_or("color-of needs --color NAME|RGBA")?;
            let color = if raw.trim_start().starts_with('[') {
                ColorQuery::Rgba(serde_json::from_str(&raw)?)
            } else {
                ColorQuery::Name(raw)
            };
            Ok(QuerySpec::ColorOf {
                color,
                min_extent: flag_f32(rest, "--min-extent").unwrap_or(DEFAULT_MIN_EXTENT),
                tolerance: flag_f32(rest, "--tolerance").unwrap_or(0.32),
            })
        }
        "assembly-of" | "assembly_of" => {
            let entity_id = flag_value(rest, "--entity")
                .or_else(|| rest.first().cloned())
                .ok_or("assembly-of needs --entity ID")?;
            Ok(QuerySpec::AssemblyOf { entity_id })
        }
        "pixel" => {
            let x: u32 = flag_value(rest, "--x")
                .ok_or("pixel needs --x")?
                .parse()?;
            let y: u32 = flag_value(rest, "--y")
                .ok_or("pixel needs --y")?
                .parse()?;
            Ok(QuerySpec::Pixel {
                x,
                y,
                width: flag_value(rest, "--width").and_then(|s| s.parse().ok()),
                height: flag_value(rest, "--height").and_then(|s| s.parse().ok()),
            })
        }
        "elements" => {
            let parse3 = |name: &str| -> Option<[f32; 3]> {
                flag_value(rest, name).and_then(|s| {
                    let p: Vec<f32> = s
                        .split(',')
                        .filter_map(|x| x.trim().parse().ok())
                        .collect();
                    (p.len() == 3).then_some([p[0], p[1], p[2]])
                })
            };
            Ok(QuerySpec::Elements {
                bbox_min: parse3("--bbox-min"),
                bbox_max: parse3("--bbox-max"),
                y_min: flag_f32(rest, "--y-min"),
                y_max: flag_f32(rest, "--y-max"),
                not_adjacent_to: flag_value(rest, "--not-adjacent-to"),
                min_extent: flag_f32(rest, "--min-extent").unwrap_or(DEFAULT_MIN_EXTENT),
            })
        }
        other => Err(format!("unknown query kind: {other}").into()),
    }
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
}

fn flag_f32(args: &[String], name: &str) -> Option<f32> {
    flag_value(args, name)?.parse().ok()
}

fn client_apply(mailbox: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut base_revision: Option<u64> = None;
    let mut idempotency_key: Option<String> = None;
    let mut label: Option<String> = None;
    let mut changes: Vec<ApplyChange> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--base-revision" => {
                base_revision = Some(
                    args.get(i + 1)
                        .ok_or("--base-revision needs a number")?
                        .parse()?,
                );
                i += 2;
            }
            "--idempotency-key" => {
                idempotency_key = Some(
                    args.get(i + 1)
                        .ok_or("--idempotency-key needs a value")?
                        .clone(),
                );
                i += 2;
            }
            "--label" => {
                label = Some(args.get(i + 1).ok_or("--label needs a value")?.clone());
                i += 2;
            }
            "--change" => {
                let raw = args.get(i + 1).ok_or("--change needs JSON")?;
                changes.push(serde_json::from_str(raw)?);
                i += 2;
            }
            other => return Err(format!("unknown apply flag: {other}").into()),
        }
    }

    let request = ApplyRequest {
        base_revision: base_revision.ok_or("apply needs --base-revision")?,
        idempotency_key: idempotency_key.ok_or("apply needs --idempotency-key")?,
        label: label.ok_or("apply needs --label")?,
        changes,
        dry_run: false,
    };
    if request.changes.is_empty() {
        return Err("apply needs at least one --change".into());
    }

    let client = MailboxClient::connect(mailbox)?;
    let resp = client.request(&MailboxRequest::Apply { request })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp
            .error
            .or_else(|| resp.apply.and_then(|a| a.error))
            .unwrap_or_else(|| "apply failed".into())
            .into());
    }
    Ok(())
}

fn client_history(mailbox: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let action = match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "list" => HistoryAction::List,
        "undo" => HistoryAction::Undo,
        "redo" => HistoryAction::Redo,
        other => return Err(format!("history needs list|undo|redo, got {other}").into()),
    };
    let client = MailboxClient::connect(mailbox)?;
    let resp = client.request(&MailboxRequest::History { action })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "history failed".into()).into());
    }
    Ok(())
}

fn client_project(mailbox: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let action = match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "list" => ProjectAction::List,
        "save" => ProjectAction::Save,
        "open" => ProjectAction::Open,
        "create" => ProjectAction::Create,
        other => return Err(format!("project needs list|save|open|create, got {other}").into()),
    };
    let path = flag_value(args, "--path").or_else(|| {
        if matches!(action, ProjectAction::Open | ProjectAction::Create) {
            args.get(1).cloned()
        } else {
            None
        }
    });
    let client = MailboxClient::connect(mailbox)?;
    let resp = client.request(&MailboxRequest::Project { action, path })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "project failed".into()).into());
    }
    Ok(())
}

fn client_render(mailbox: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let width = flag_value(args, "--width").and_then(|s| s.parse().ok());
    let height = flag_value(args, "--height").and_then(|s| s.parse().ok());
    let client = MailboxClient::connect(mailbox)?;
    let resp = client.request(&MailboxRequest::Render { width, height })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "render failed".into()).into());
    }
    Ok(())
}

fn client_camera(mailbox: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mode = flag_value(args, "--mode");
    let review_eye = flag_value(args, "--review-eye").and_then(|s| parse_vec3(&s));
    let review_target = flag_value(args, "--review-target").and_then(|s| parse_vec3(&s));
    let client = MailboxClient::connect(mailbox)?;
    let frame = flag_value(args, "--frame").map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
    });
    let resp = client.request(&MailboxRequest::Camera {
        mode,
        review_eye,
        review_target,
        frame,
    })?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "camera failed".into()).into());
    }
    Ok(())
}

fn parse_vec3(s: &str) -> Option<[f32; 3]> {
    let t = s.trim().trim_start_matches('[').trim_end_matches(']');
    let p: Vec<f32> = t
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    if p.len() == 3 {
        Some([p[0], p[1], p[2]])
    } else {
        None
    }
}

fn print_help() {
    println!(
        "\
thinner-floor — thin native authoring viewport

Usage:
  cargo run [--document PATH] [--mailbox HOST:PORT]
  cargo run -- --mcp
  cargo run -- status
  cargo run -- inspect
  cargo run -- query left-of --entity ID
  cargo run -- query on-screen
  cargo run -- query color-of --color yellow
  cargo run -- query assembly-of --entity ID
  cargo run -- apply --base-revision N --idempotency-key KEY --label TEXT --change JSON
  cargo run -- history list|undo|redo
  cargo run -- project list|save|open|create --path FILE
  cargo run -- render [--width W] [--height H]
  cargo run -- camera [--mode authored|review] [--review-eye x,y,z] [--frame id,id]
  cargo run -- catalog
  cargo run -- export [--path sits/export.gltf]
  cargo run -- import [--path sits/export.gltf]
  cargo run -- validate
  cargo run -- cycles start|pause|resume|stop|status [--samples 64]
  cargo run -- light [--dir x,y,z] [--shadows true|false]
  cargo run -- play enter|stop|pause|seek|step [--time T]
  cargo run -- rtx --requested true|false
  cargo run -- query elements --y-min 0 --y-max 2
  cargo run -- inspect --slice summary

Mailbox ops (JSON lines on localhost TCP, default {DEFAULT_MAILBOX_ADDR}):
  {{\"op\":\"status\"}}
  {{\"op\":\"inspect\"}}
  {{\"op\":\"query\",\"query\":{{\"op\":\"left_of\",\"entityId\":\"box-1\"}}}}
  {{\"op\":\"query\",\"query\":{{\"op\":\"color_of\",\"color\":\"yellow\"}}}}
  {{\"op\":\"query\",\"query\":{{\"op\":\"assembly_of\",\"entityId\":\"box-1\"}}}}
  {{\"op\":\"query\",\"query\":{{\"op\":\"on_screen\"}}}}
  {{\"op\":\"apply\",\"baseRevision\":1,\"idempotencyKey\":\"k\",\"label\":\"paint\",\"changes\":[...]}}

Change ops:
  {{\"op\":\"create_mesh\",\"entity\":{{...}}}}
  {{\"op\":\"patch_color\",\"entityId\":\"box-1\",\"color\":[r,g,b,a]}}
  {{\"op\":\"patch_translation\",\"entityId\":\"box-2\",\"translation\":[x,y,z]}}
"
    );
}
