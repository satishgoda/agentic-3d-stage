#Requires -Version 5.1
# Init blender/cycles submodule at the pinned SHA and apply cycles-stream.patch
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Pin = "1319002982e09970cb50f727e3f299cea78de229"
$Sub = Join-Path $Root "third_party\cycles"
$Patch = Join-Path $Root "patches\cycles\0001-cycles-stream.patch"
$Url = "https://github.com/blender/cycles.git"

New-Item -ItemType Directory -Force -Path (Join-Path $Root "third_party") | Out-Null

$streamCpp = Join-Path $Sub "src\app\cycles_stream.cpp"
if (-not (Test-Path (Join-Path $Sub ".git"))) {
    Write-Host "cloning blender/cycles (this is large)"
    git clone --filter=blob:none $Url $Sub
}
Set-Location $Sub
$head = (git rev-parse HEAD)
if ($head -notlike "$Pin*") {
    git fetch --filter=blob:none origin $Pin
    git checkout --detach $Pin
}
if (Test-Path $streamCpp) {
    Write-Host "cycles-stream.cpp already present (patch applied)"
} else {
    git apply --check $Patch
    git apply $Patch
    Write-Host "applied 0001-cycles-stream.patch"
}
Write-Host "cycles at $(git rev-parse --short HEAD) with cycles-stream"
Write-Host "next: .\scripts\build-cycles.ps1"
