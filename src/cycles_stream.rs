//! Cycles sidecar client: spawn daemon, TCP control, shared-memory frames.

use crate::cycles_xml;
use crate::document::Document;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const CONTROL_ADDR: &str = "127.0.0.1:17422";
pub const MAGIC: u32 = 0x54464359;
pub const HEADER: usize = 256;
pub const FLAG_HAS_FRAME: u32 = 1;
pub const FLAG_RUNNING: u32 = 2;
pub const FLAG_PAUSED: u32 = 4;
pub const FLAG_DONE: u32 = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CyclesStatus {
    pub ok: bool,
    pub state: String,
    pub sample: u32,
    pub width: u32,
    pub height: u32,
    pub overlay: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub sample: u32,
    pub seq: u32,
    pub flags: u32,
    pub rgba: Vec<u8>,
}

pub struct CyclesHost {
    child: Option<Child>,
    overlay: bool,
    last_seq: u32,
    last_error: Option<String>,
    last_samples: u32,
    last_width: u32,
    last_height: u32,
    shm: Option<Shm>,
}

struct Shm {
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
    ptr: *const u8,
    len: usize,
}

unsafe impl Send for Shm {}

fn default_exe() -> PathBuf {
    PathBuf::from(cycles_xml::cycles_root()).join("install").join("cycles-stream.exe")
}

impl CyclesHost {
    fn new() -> Self {
        Self {
            child: None,
            overlay: false,
            last_seq: 0,
            last_error: None,
            last_samples: 64,
            last_width: 960,
            last_height: 640,
            shm: None,
        }
    }

    pub fn ensure_spawned(&mut self) -> Result<(), String> {
        if let Some(ch) = self.child.as_mut() {
            if ch.try_wait().ok().flatten().is_none() {
                return Ok(());
            }
        }
        let exe = std::env::var("TF_CYCLES_STREAM")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_exe());
        if !exe.exists() {
            return Err(format!("cycles-stream missing: {}", exe.display()));
        }
        let mut cmd = Command::new(&exe);
        cmd.arg("--listen")
            .arg(CONTROL_ADDR)
            .current_dir(exe.parent().unwrap_or(std::path::Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("spawn cycles-stream:{e}"))?;
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(
                &CONTROL_ADDR.parse().unwrap(),
                Duration::from_millis(200),
            )
            .is_ok()
            {
                let _ = self.open_shm();
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        Err("cycles-stream did not listen on 17422".into())
    }

    fn open_shm(&mut self) -> Result<(), String> {
        if self.shm.is_some() {
            return Ok(());
        }
        #[cfg(windows)]
        {
            let shm = unsafe { Shm::open() }?;
            self.shm = Some(shm);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            Err("cycles shm is Windows-only".into())
        }
    }

    fn rpc(&mut self, line: &str) -> Result<String, String> {
        self.ensure_spawned()?;
        let mut s = TcpStream::connect_timeout(
            &CONTROL_ADDR.parse().unwrap(),
            Duration::from_millis(800),
        )
        .map_err(|e| format!("cycles ctl:{e}"))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        s.write_all(line.as_bytes())
            .and_then(|_| s.write_all(b"\n"))
            .map_err(|e| e.to_string())?;
        let mut r = BufReader::new(s);
        let mut out = String::new();
        r.read_line(&mut out).map_err(|e| e.to_string())?;
        Ok(out.trim().to_string())
    }

    pub fn start(&mut self, doc: &Document, samples: u32, width: u32, height: u32) -> CyclesStatus {
        match self.start_inner(doc, samples, width, height) {
            Ok(s) => s,
            Err(e) => {
                self.last_error = Some(e.clone());
                CyclesStatus {
                    ok: false,
                    state: "error".into(),
                    sample: 0,
                    width: 0,
                    height: 0,
                    overlay: self.overlay,
                    error: Some(e),
                }
            }
        }
    }

    fn start_inner(
        &mut self,
        doc: &Document,
        samples: u32,
        width: u32,
        height: u32,
    ) -> Result<CyclesStatus, String> {
        self.ensure_spawned()?;
        self.last_samples = samples.max(1);
        self.last_width = width.max(8);
        self.last_height = height.max(8);
        self.last_seq = 0;
        let xml = PathBuf::from(cycles_xml::cycles_root())
            .join("install")
            .join("live.xml");
        cycles_xml::write_document_xml(doc, &xml, self.last_width, self.last_height)?;
        let abs = std::fs::canonicalize(&xml).unwrap_or(xml);
        let mut path = abs.to_string_lossy().to_string();
        if let Some(s) = path.strip_prefix(r"\\?\") {
            path = s.to_string();
        }
        let path = path.replace('\\', "/");
        let req = format!(
            "{{\"op\":\"start\",\"xml\":\"{path}\",\"samples\":{},\"width\":{},\"height\":{}}}",
            self.last_samples, self.last_width, self.last_height
        );
        let resp = self.rpc(&req)?;
        self.overlay = true;
        Ok(self.parse_status(&resp))
    }

    pub fn pause(&mut self) -> CyclesStatus {
        self.cmd("pause")
    }
    pub fn resume(&mut self) -> CyclesStatus {
        self.cmd("resume")
    }
    pub fn stop(&mut self) -> CyclesStatus {
        let st = self.cmd("stop");
        self.overlay = false;
        CyclesStatus {
            overlay: false,
            ..st
        }
    }

    fn cmd(&mut self, op: &str) -> CyclesStatus {
        match self.rpc(&format!("{{\"op\":\"{op}\"}}")) {
            Ok(r) => self.parse_status(&r),
            Err(e) => CyclesStatus {
                ok: false,
                state: "error".into(),
                sample: 0,
                width: 0,
                height: 0,
                overlay: self.overlay,
                error: Some(e),
            },
        }
    }

    fn parse_status(&self, s: &str) -> CyclesStatus {
        let v: serde_json::Value = serde_json::from_str(s).unwrap_or(serde_json::json!({}));
        CyclesStatus {
            ok: v["ok"].as_bool().unwrap_or(false),
            state: v["state"].as_str().unwrap_or("idle").into(),
            sample: v["sample"].as_u64().unwrap_or(0) as u32,
            width: v["width"].as_u64().unwrap_or(0) as u32,
            height: v["height"].as_u64().unwrap_or(0) as u32,
            overlay: self.overlay,
            error: v["error"].as_str().map(|e| e.to_string()),
        }
    }

    pub fn snapshot(&mut self) -> CyclesStatus {
        match self.rpc("{\"op\":\"status\"}") {
            Ok(r) => self.parse_status(&r),
            Err(_) => CyclesStatus {
                ok: self.child.is_some(),
                state: if self.child.is_some() {
                    "idle"
                } else {
                    "down"
                }
                .into(),
                sample: 0,
                width: 0,
                height: 0,
                overlay: self.overlay,
                error: None,
            },
        }
    }

    pub fn overlay_on(&self) -> bool {
        self.overlay
    }

    /// Overlay is showing a frozen sidecar session. Re-export XML and `start` again.
    pub fn refresh_if_overlay(&mut self, doc: &Document) {
        if !self.overlay {
            return;
        }
        let samples = self.last_samples;
        let width = self.last_width;
        let height = self.last_height;
        let st = self.start(doc, samples, width, height);
        if !st.ok {
            self.last_error = st.error;
        }
    }

    pub fn read_frame(&mut self) -> Option<Frame> {
        let shm = self.shm.as_ref()?;
        unsafe { shm.read(self.last_seq) }.map(|f| {
            self.last_seq = f.seq;
            f
        })
    }

    pub fn kill_child(&mut self) {
        let _ = self.rpc("{\"op\":\"quit\"}");
        if let Some(mut ch) = self.child.take() {
            let _ = ch.kill();
            let _ = ch.wait();
        }
        self.overlay = false;
    }
}

impl Drop for CyclesHost {
    fn drop(&mut self) {
        self.kill_child();
    }
}

#[cfg(windows)]
impl Shm {
    unsafe fn open() -> Result<Self, String> {
        let name: Vec<u16> = "Local\\ThinnerFloorCyclesFb"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
        if handle.is_null() {
            return Err("OpenFileMappingW cycles fb".into());
        }
        let len = HEADER + 1920 * 1080 * 4;
        let ptr = MapViewOfFile(handle, FILE_MAP_READ, 0, 0, len) as *const u8;
        if ptr.is_null() {
            CloseHandle(handle);
            return Err("MapViewOfFile cycles fb".into());
        }
        Ok(Self { handle, ptr, len })
    }

    unsafe fn read(&self, last_seq: u32) -> Option<Frame> {
        let magic = std::ptr::read_unaligned(self.ptr.add(0) as *const u32);
        if magic != MAGIC {
            return None;
        }
        let width = std::ptr::read_unaligned(self.ptr.add(4) as *const u32);
        let height = std::ptr::read_unaligned(self.ptr.add(8) as *const u32);
        let sample = std::ptr::read_unaligned(self.ptr.add(12) as *const u32);
        let seq = std::ptr::read_unaligned(self.ptr.add(16) as *const u32);
        let flags = std::ptr::read_unaligned(self.ptr.add(20) as *const u32);
        let stride = std::ptr::read_unaligned(self.ptr.add(24) as *const u32);
        if seq == last_seq || width == 0 || height == 0 || (flags & FLAG_HAS_FRAME) == 0 {
            return None;
        }
        if stride == 0 || height as usize * stride as usize + HEADER > self.len {
            return None;
        }
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let src = self.ptr.add(HEADER);
        for y in 0..height as usize {
            let row = src.add(y * stride as usize);
            let dst = y * width as usize * 4;
            std::ptr::copy_nonoverlapping(row, rgba.as_mut_ptr().add(dst), width as usize * 4);
        }
        let seq2 = std::ptr::read_unaligned(self.ptr.add(16) as *const u32);
        if seq2 != seq {
            return None;
        }
        Some(Frame {
            width,
            height,
            sample,
            seq,
            flags,
            rgba,
        })
    }
}

#[cfg(windows)]
impl Drop for Shm {
    fn drop(&mut self) {
        unsafe {
            if !self.ptr.is_null() {
                UnmapViewOfFile(self.ptr as *const _);
            }
            if !self.handle.is_null() {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
const FILE_MAP_READ: u32 = 0x0004;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> *mut std::ffi::c_void;
    fn MapViewOfFile(
        h: *mut std::ffi::c_void,
        access: u32,
        hi: u32,
        lo: u32,
        size: usize,
    ) -> *mut std::ffi::c_void;
    fn UnmapViewOfFile(p: *const std::ffi::c_void) -> i32;
    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
}

static HOST: OnceLock<Mutex<CyclesHost>> = OnceLock::new();

pub fn host() -> &'static Mutex<CyclesHost> {
    HOST.get_or_init(|| Mutex::new(CyclesHost::new()))
}

pub fn exe_available() -> bool {
    default_exe().exists()
        || std::env::var("TF_CYCLES_STREAM")
            .map(|p| PathBuf::from(p).exists())
            .unwrap_or(false)
}
