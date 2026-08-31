# talk.ps1 — look / change the live Thinner Floor window.
# Run from the crate root (this repo).
#   .\talk.ps1 status
#   .\talk.ps1 inspect
#   .\talk.ps1 paint 0.2 0.8 0.35
#   .\talk.ps1 add sphere
#   .\talk.ps1 add box -Id box-2 -X 1.2 -Y 0.5 -Z 0
#   .\talk.ps1 move box-2 -X 0.4 -Y 0.5 -Z 0
#   .\talk.ps1 undo
#   .\talk.ps1 cycles start|pause|resume|stop|status

param(
  [Parameter(Position = 0)]
  [ValidateSet("status", "inspect", "paint", "cycles", "add", "create", "move", "undo", "redo")]
  [string]$Command = "status",
  [Parameter(Position = 1)]$Arg1 = $null,
  [Parameter(Position = 2)][double]$G = 0.45,
  [Parameter(Position = 3)][double]$B = 0.9,
  [int]$Samples = 64,
  [string]$Id = "",
  [string]$Recipe = "",
  [double]$X = [double]::NaN,
  [double]$Y = [double]::NaN,
  [double]$Z = [double]::NaN,
  [double]$Sx = [double]::NaN,
  [double]$Sy = [double]::NaN,
  [double]$Sz = [double]::NaN,
  [double]$R = [double]::NaN
)

$ErrorActionPreference = "Stop"
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\msys64\ucrt64\bin;" + $env:Path
$mailbox = "127.0.0.1"
$port = 17421

function Send-Line([string]$line) {
  $tokenFile = Join-Path (Get-Location) "thinner-floor.token"
  if (Test-Path $tokenFile) {
    $tok = (Get-Content -Path $tokenFile -Raw).Trim()
    if ($line.StartsWith("{") -and $tok.Length -gt 0) {
      $line = $line.Insert(1, ('"token":"{0}",' -f $tok))
    }
  }
  $c = New-Object System.Net.Sockets.TcpClient($mailbox, $port)
  try {
    $s = $c.GetStream()
    $w = New-Object System.IO.StreamWriter($s)
    $w.NewLine = "`n"
    $w.AutoFlush = $true
    $w.WriteLine($line)
    $r = New-Object System.IO.StreamReader($s)
    $r.ReadLine()
  } finally {
    $c.Close()
  }
}

function Get-Rev {
  $st = Send-Line '{"op":"status"}' | ConvertFrom-Json
  if (-not $st.ok) { throw "status failed: $st" }
  return [int]$st.status.revision
}

function Next-Id([string]$prefix) {
  $insp = Send-Line '{"op":"inspect","slice":"summary"}' | ConvertFrom-Json
  $ids = @()
  if ($insp.inspectSummary -and $insp.inspectSummary.entities) {
    $ids = @($insp.inspectSummary.entities | ForEach-Object { $_.id })
  }
  $n = 1
  while ($ids -contains ("{0}-{1}" -f $prefix, $n)) { $n++ }
  return "{0}-{1}" -f $prefix, $n
}

function NumOr([double]$v, [double]$d) {
  if ([double]::IsNaN($v)) { return $d }
  return $v
}

switch ($Command) {
  "status" {
    Send-Line '{"op":"status"}'
  }
  "inspect" {
    Send-Line '{"op":"inspect","slice":"summary"}'
  }
  "paint" {
    $Rv = if ($null -ne $Arg1) { [double]$Arg1 } else { 0.2 }
    $rev = Get-Rev
    $target = if ($Id) { $Id } else { "box-1" }
    $key = "paint-{0}-{1}" -f $rev, [guid]::NewGuid().ToString("N").Substring(0, 8)
    $line = '{{"op":"apply","baseRevision":{0},"idempotencyKey":"{1}","label":"paint {5}","changes":[{{"op":"patch_color","entityId":"{5}","color":[{2},{3},{4},1.0]}}]}}' -f $rev, $key, $Rv, $G, $B, $target
    Send-Line $line
  }
  { $_ -in @("add", "create") } {
    $recipe = if ($Recipe) { $Recipe } elseif ($Arg1) { [string]$Arg1 } else { "box" }
    if ($recipe -notin @("box", "sphere", "plane")) { throw "recipe must be box|sphere|plane (got $recipe)" }
    $prefix = switch ($recipe) { "sphere" { "ball" } "plane" { "pad" } default { "box" } }
    $eid = if ($Id) { $Id } else { Next-Id $prefix }
    $px = NumOr $X 1.2
    $py = if ($recipe -eq "plane") { NumOr $Y 0.0 } else { NumOr $Y 0.5 }
    $pz = NumOr $Z 0.0
    if ($recipe -eq "plane") {
      $wx = NumOr $Sx 1.6; $wy = NumOr $Sy 0.05; $wz = NumOr $Sz 1.6
    } else {
      $wx = NumOr $Sx 0.8; $wy = NumOr $Sy 0.8; $wz = NumOr $Sz 0.8
    }
    $cr = if ([double]::IsNaN($R)) { 0.36 } else { $R }
    $cg = $G; $cb = $B
    $rev = Get-Rev
    $key = "add-{0}-{1}" -f $eid, [guid]::NewGuid().ToString("N").Substring(0, 8)
    $ent = '{{"id":"{0}","kind":"mesh","transform":{{"translation":[{1},{2},{3}],"rotation":[0,0,0,1],"scale":[1,1,1]}},"mesh":{{"recipe":"{4}","size":[{5},{6},{7}]}},"material":{{"color":[{8},{9},{10},1]}}}}' -f $eid, $px, $py, $pz, $recipe, $wx, $wy, $wz, $cr, $cg, $cb
    $line = '{{"op":"apply","baseRevision":{0},"idempotencyKey":"{1}","label":"create {2}","changes":[{{"op":"create_mesh","entity":{3}}}]}}' -f $rev, $key, $eid, $ent
    Send-Line $line
  }
  "move" {
    $eid = if ($Id) { $Id } elseif ($Arg1) { [string]$Arg1 } else { throw "move needs -Id or the entity id" }
    $px = NumOr $X 0.0
    $py = NumOr $Y 0.5
    $pz = NumOr $Z 0.0
    $rev = Get-Rev
    $key = "move-{0}-{1}" -f $eid, [guid]::NewGuid().ToString("N").Substring(0, 8)
    $line = '{{"op":"apply","baseRevision":{0},"idempotencyKey":"{1}","label":"move {2}","changes":[{{"op":"patch_translation","entityId":"{2}","translation":[{3},{4},{5}]}}]}}' -f $rev, $key, $eid, $px, $py, $pz
    Send-Line $line
  }
  "undo" {
    Send-Line '{"op":"history","action":"undo"}'
  }
  "redo" {
    Send-Line '{"op":"history","action":"redo"}'
  }
  "cycles" {
    $action = if ($Arg1) { [string]$Arg1 } else { "status" }
    $line = '{{"op":"cycles","action":"{0}","samples":{1}}}' -f $action, $Samples
    Send-Line $line
  }
}
