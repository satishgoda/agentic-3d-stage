# How to use the sit

One Rust binary: wgpu window, versioned JSON, localhost mailbox. Optional Cycles overlay is a **sidecar**, not linked in.

```text
thinner-floor.exe
├─ no args     → window + mailbox (authoring sit)
├─ status|…    → client (reads thinner-floor.token)
└─ --mcp       → stdio MCP adapter
on disk        → ./thinner-floor.json
               → ./thinner-floor.token  (gitignored)
mailbox        → 127.0.0.1:17421  JSON lines + token
```

## PATH

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;" + $env:Path
```

## Open the sit (leave running)

```powershell
cargo run
```

Window title **Thinner Floor**. First picture: terracotta box on a grey ground. MAILBOX LIVE FEED is a **title bar** (click **+** to expand; **Ctrl+Shift+M** hides).

## Look / Change (second shell)

```powershell
.\talk.ps1 status
.\talk.ps1 inspect
.\target\debug\thinner-floor.exe query on-screen
.\talk.ps1 paint 0.2 0.8 0.35
```

Apply needs the live revision from `status` if you send raw JSON (`--base-revision`). `talk.ps1 paint` does that for you.

PowerShell often eats `--change` JSON. Prefer `talk.ps1` or a here-string. See the private how-to in `thinner-floor` for the full apply grammar.

## Cycles overlay

After [BUILD.md](BUILD.md) has produced `third_party/cycles/install/cycles-stream.exe`:

```powershell
$env:TF_CYCLES_ROOT = (Resolve-Path .\third_party\cycles)
.\talk.ps1 cycles start -Samples 16
.\talk.ps1 cycles status
.\talk.ps1 cycles stop
```

Window: **Ctrl+Shift+C** / **Ctrl+Shift+X**. First sample can take a few seconds (wait banner, not a black frame). Later samples should stream into the overlay.

## Kill

Close the window, or stop the process that is listening on `:17421`.
