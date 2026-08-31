#Requires -Version 5.1
# MSVC CMake build of cycles-stream (CPU). gcc cannot build Cycles.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Sub = Join-Path $Root "third_party\cycles"
if (-not (Test-Path (Join-Path $Sub "src\app\cycles_stream.cpp"))) {
    throw "run .\scripts\setup-cycles.ps1 first (missing cycles_stream.cpp)"
}
$cmake = "C:\Program Files\CMake\bin\cmake.exe"
if (-not (Test-Path $cmake)) { $cmake = (Get-Command cmake -ErrorAction Stop).Source }
$build = Join-Path $Sub "build"
$install = Join-Path $Sub "install"
& $cmake -B $build -S $Sub -G "Visual Studio 17 2022" -A x64 `
    -DWITH_CYCLES_DEVICE_CUDA=OFF -DWITH_CYCLES_DEVICE_OPTIX=OFF -DWITH_CYCLES_DEVICE_HIP=OFF
if ($LASTEXITCODE -ne 0) { throw "cmake configure failed" }
& $cmake --build $build --config Release --target cycles-stream --parallel
if ($LASTEXITCODE -ne 0) { throw "cmake build failed" }
New-Item -ItemType Directory -Force -Path $install | Out-Null
$exe = Join-Path $build "bin\Release\cycles-stream.exe"
if (-not (Test-Path $exe)) { $exe = Join-Path $build "Release\cycles-stream.exe" }
Copy-Item -Force $exe (Join-Path $install "cycles-stream.exe")
Write-Host "installed $(Join-Path $install 'cycles-stream.exe')"
Write-Host "set TF_CYCLES_ROOT=$Sub before cargo run if cwd is not the repo root"
