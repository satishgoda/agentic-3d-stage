# talk.ps1 — look / change the live Thinner Floor window.
# Run from the crate root (this repo).
#   .\talk.ps1 status
#   .\talk.ps1 inspect
#   .\talk.ps1 paint 0.2 0.8 0.35
#   .\talk.ps1 cycles start|pause|resume|stop|status

param(
  [Parameter(Position = 0)]
  [ValidateSet("status", "inspect", "paint", "cycles")]
  [string]$Command = "status",
  [Parameter(Position = 1)]$Arg1 = $null,
  [Parameter(Position = 2)][double]$G = 0.45,
  [Parameter(Position = 3)][double]$B = 0.9,
  [int]$Samples = 64
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

switch ($Command) {
  "status" {
    Send-Line '{"op":"status"}'
  }
  "inspect" {
    Send-Line '{"op":"inspect"}'
  }
  "paint" {
    $R = if ($null -ne $Arg1) { [double]$Arg1 } else { 0.2 }
    $st = Send-Line '{"op":"status"}' | ConvertFrom-Json
    if (-not $st.ok) { throw "status failed: $st" }
    $rev = $st.status.revision
    $key = "paint-{0}-{1}" -f $rev, [guid]::NewGuid().ToString("N").Substring(0, 8)
    $line = '{{"op":"apply","baseRevision":{0},"idempotencyKey":"{1}","label":"paint box-1","changes":[{{"op":"patch_color","entityId":"box-1","color":[{2},{3},{4},1.0]}}]}}' -f $rev, $key, $R, $G, $B
    Send-Line $line
  }
  "cycles" {
    $action = if ($Arg1) { [string]$Arg1 } else { "status" }
    $line = '{{"op":"cycles","action":"{0}","samples":{1}}}' -f $action, $Samples
    Send-Line $line
  }
}
