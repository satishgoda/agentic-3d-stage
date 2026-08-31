# Agentic 3D Stage

Thin native authoring viewport: a **wgpu** window, a versioned JSON document, a localhost mailbox, and an optional **Blender Cycles** overlay (sidecar process, not linked into the Rust binary).

The crate and window are still named **thinner-floor**. This repo is the public stage.

```text
You / agent
  └─ talk.ps1 / thinner-floor.exe
        └─ mailbox :17421
              ├─ sit.json / thinner-floor.json  →  wgpu Lambert window
              └─ if overlay on
                    live.xml → cycles-stream → SHM → composite on the same window
```

Private development stays in `satishgoda/thinner-floor` and `satishgoda/cycles`. This tree is the subset you can build from scratch.

## Requirements (Windows)

| piece | toolchain |
| --- | --- |
| sit | Rust **windows-gnu** (`rust-toolchain.toml`) + MSYS `gcc` |
| Cycles worker | **MSVC** 2022 Build Tools + CMake — gcc cannot build Cycles |

```powershell
git clone --recurse-submodules https://github.com/satishgoda/agentic-3d-stage.git
cd agentic-3d-stage
```

If you already cloned without submodules: `git submodule update --init --recursive` (Cycles is large).

## Sit

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;" + $env:Path
cargo test
cargo run
```

Leave that window up. Another shell, same PATH + cwd:

```powershell
.\talk.ps1 status
.\target\debug\thinner-floor.exe query on-screen
.\talk.ps1 paint 0.85 0.15 0.55
```

Do not `cargo run -- status` while the exe is locked.

## Cycles overlay (optional)

```powershell
.\scripts\setup-cycles.ps1
.\scripts\build-cycles.ps1
$env:TF_CYCLES_ROOT = (Resolve-Path .\third_party\cycles)
# sit already running:
.\talk.ps1 cycles start -Samples 16
.\talk.ps1 cycles stop
```

Window: **Ctrl+Shift+C** start/pause, **Ctrl+Shift+X** stop. The sidecar must run with `headless=false` (our patch) so samples stream into the window instead of appearing only at the last spp.

Pin: `blender/cycles` @ `131900298` (2026-08-19). Patch: [`patches/cycles/0001-cycles-stream.patch`](patches/cycles/0001-cycles-stream.patch).

## Docs

- [docs/BUILD.md](docs/BUILD.md) — toolchains, submodule, CMake flags
- [docs/HOWTO.md](docs/HOWTO.md) — Look / Change / Cycles
- [docs/architecture.md](docs/architecture.md) — crate map

## License

MIT for this repository. Cycles in `third_party/cycles` is **Apache-2.0** (Blender Foundation). See [NOTICE](NOTICE).
