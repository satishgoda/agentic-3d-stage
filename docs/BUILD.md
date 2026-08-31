# Build

Two toolchains. Do not mix them on one PATH for both jobs.

## Sit (Rust, windows-gnu)

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;" + $env:Path
rustc --version   # expect windows-gnu (this dir pins rust-toolchain.toml)
cargo test
cargo run
```

`cargo run` with no args is the window. While it is up, talk with `.\target\debug\thinner-floor.exe` or `.\talk.ps1`, not a second `cargo run` that relinks the locked exe.

## Cycles worker (MSVC)

gcc cannot build Cycles. Use Visual Studio 2022 Build Tools + CMake.

```powershell
.\scripts\setup-cycles.ps1    # submodule blender/cycles @ 131900298 + apply patch
.\scripts\build-cycles.ps1    # cmake VS 2022, CPU only, copy install\cycles-stream.exe
```

`setup-cycles.ps1` clones [blender/cycles](https://github.com/blender/cycles) into `third_party/cycles` (pin `1319002982e09970cb50f727e3f299cea78de229`) and applies [`patches/cycles/0001-cycles-stream.patch`](../patches/cycles/0001-cycles-stream.patch).

CMake flags (CPU sidecar, same as our private tree):

```text
-DWITH_CYCLES_DEVICE_CUDA=OFF
-DWITH_CYCLES_DEVICE_OPTIX=OFF
-DWITH_CYCLES_DEVICE_HIP=OFF
```

Unix:

```bash
./scripts/setup-cycles.sh
# then cmake --build as in BUILDING.md inside third_party/cycles, target cycles-stream
```

Point the sit at the tree:

```powershell
$env:TF_CYCLES_ROOT = (Resolve-Path .\third_party\cycles)
```

If `TF_CYCLES_ROOT` is unset, the sit looks for `./third_party/cycles` from the current working directory.

Overlay: sit up, then `.\talk.ps1 cycles start -Samples 16`.
