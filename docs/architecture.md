# Architecture

One Rust binary, two hats: a wgpu window that **owns** the scene, and a CLI that **talks** to it.

```text
crate  thinner-floor
├─ binary  thinner-floor.exe     src/main.rs
│  ├─ no args     → sit (window + mailbox + token file)
│  ├─ status|inspect|query|apply|history|project|export → client
│  └─ --mcp       → stdio MCP
└─ library
   ├─ document.rs      JSON + apply + undo/redo
   ├─ mailbox.rs       TCP :17421 + named pipe + token
   ├─ cycles_xml.rs    Document → Cycles XML (scale 1 1 −1)
   ├─ cycles_stream.rs spawn cycles-stream, SHM overlay
   ├─ beauty.rs        offscreen PNG
   ├─ light.rs         directional shadow map (not RTX)
   └─ hud.rs           mailbox live feed (title bar by default)
```

```text
sit process
├─ main thread     winit ~8ms poll, wgpu Lambert, HUD, Cycles overlay
├─ mailbox         127.0.0.1:17421
└─ optional        cycles-stream.exe  :17422 + SHM Local\ThinnerFloorCyclesFb
```

Apply (non-idempotent) rewrites `./thinner-floor.json`. If Cycles overlay is on, the sit re-exports `live.xml` and restarts the sidecar session so the path trace matches the new document.

Cycles cameras look **+Z**; wgpu looks **−Z**. XML uses `scale="1 1 -1"`.

`rtx.active` is never true in this crate.
