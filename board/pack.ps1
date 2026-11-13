# สร้างชุดบอร์ดพกพาไว้แจกนักเรียน — ไม่ต้องลง Rust ไม่ต้องลง trunk
#
#   .\pack.ps1
#
# ได้ไฟล์ pack\board-pack.zip ข้างในมี
#   board-server.exe   เซิร์ฟเวอร์ของบอร์ด (build แบบ release)
#   dist\              หน้าเว็บที่ build แล้ว
#   start.ps1          เปิดบอร์ดคำสั่งเดียว
#   อ่านก่อน.txt        วิธีใช้สามบรรทัด
#
# นักเรียนแตก zip ไว้ "ในโฟลเดอร์บอท" (โฟลเดอร์ที่มี tradebot\ config\ logs\)
# ให้เป็น <โฟลเดอร์บอท>\board-pack\ แล้วรัน start.ps1 ข้างใน
# ตำแหน่งนี้สำคัญ เพราะ start.ps1 หาไฟล์ของบอทที่ .. เหมือน board\start.ps1 ทุกอย่าง

# ไม่ใช้ $ErrorActionPreference = "Stop" ทั้งไฟล์: cargo กับ trunk เขียนความคืบหน้า
# ลง stderr และ PowerShell 5.1 จะถือว่านั่นคือ error ทันทีที่ output ถูก redirect
# จึงเช็ค $LASTEXITCODE ของคำสั่งภายนอกเอง และใส่ -ErrorAction Stop ให้ cmdlet ที่สำคัญ
Set-Location $PSScriptRoot

$out = Join-Path $PSScriptRoot "pack\board-pack"
$zip = Join-Path $PSScriptRoot "pack\board-pack.zip"

Write-Host "1/3 build เซิร์ฟเวอร์ (release)..." -ForegroundColor Cyan
cargo build --release -p board-server
if ($LASTEXITCODE -ne 0) { throw "cargo build ไม่ผ่าน" }

Write-Host "2/3 build หน้าเว็บ (release)..." -ForegroundColor Cyan
Push-Location (Join-Path $PSScriptRoot "app")
# trunk reads NO_COLOR as a true/false flag and refuses the "1" that most
# tools - and many terminals - set it to. Hide it for this one call.
$noColor = $env:NO_COLOR
Remove-Item Env:NO_COLOR -ErrorAction SilentlyContinue
trunk build --release
$built = $LASTEXITCODE
if ($null -ne $noColor) { $env:NO_COLOR = $noColor }
Pop-Location
if ($built -ne 0) { throw "trunk build ไม่ผ่าน" }

Write-Host "3/3 รวมเป็นชุด..." -ForegroundColor Cyan
if (Test-Path $out) { Remove-Item -Recurse -Force $out -ErrorAction Stop }
New-Item -ItemType Directory -Force $out -ErrorAction Stop | Out-Null
Copy-Item (Join-Path $PSScriptRoot "target\release\board-server.exe") $out -ErrorAction Stop
Copy-Item -Recurse (Join-Path $PSScriptRoot "app\dist") (Join-Path $out "dist") -ErrorAction Stop

# start.ps1 ของชุดพกพา: เหมือน board\start.ps1 แต่เรียก exe ตรงๆ ไม่ต้องมี cargo
$start = @'
# เปิดบอร์ด — ชุดพกพา
# วางโฟลเดอร์นี้ไว้ในโฟลเดอร์บอท (ที่มี tradebot\ config\ logs\) แล้วรันไฟล์นี้
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$bot = Split-Path $PSScriptRoot -Parent
if (-not (Test-Path (Join-Path $bot "tradebot"))) {
    Write-Host "ไม่เจอโฟลเดอร์ tradebot ที่ $bot" -ForegroundColor Red
    Write-Host "ต้องวางโฟลเดอร์ board-pack ไว้ในโฟลเดอร์บอท ให้เป็น <โฟลเดอร์บอท>\board-pack" -ForegroundColor Yellow
    exit 1
}

$env:BOARD_DIST = "dist"
$env:BOARD_LOGS = "..\logs"
$env:BOARD_CANDLES = "..\data\candles"
$env:BOARD_CODE = ".."
$env:BOARD_ENV_FILE = "..\config\.env"
if (-not $env:BOARD_CONFIG) { $env:BOARD_CONFIG = "config\config.yaml" }

$port = if ($env:PORT) { $env:PORT } else { "4340" }
Write-Host ""
Write-Host "เปิดบอร์ดที่  http://127.0.0.1:$port" -ForegroundColor Cyan
Write-Host "ลิงก์ลัด     http://127.0.0.1:$port/#order   (เล่นซ้ำ tick ที่บอทส่งออเดอร์)" -ForegroundColor Cyan
Write-Host "กด Ctrl+C เพื่อหยุด"
Write-Host ""
.\board-server.exe
'@
# Windows PowerShell 5.1 reads a .ps1 without a BOM as ANSI, and the Thai in
# it turns to rubbish - so the file is written with one, on purpose.
$utf8bom = New-Object System.Text.UTF8Encoding($true)
[System.IO.File]::WriteAllText((Join-Path $out "start.ps1"), $start, $utf8bom)

$readme = @'
ชุดบอร์ดพกพา — ไม่ต้องลง Rust ไม่ต้องลง trunk

1. แตกไฟล์นี้ไว้ในโฟลเดอร์บอท (ที่มี tradebot\ config\ logs\) ให้เป็น <โฟลเดอร์บอท>\board-pack
2. คลิกขวา start.ps1 -> Run with PowerShell   (หรือพิมพ์ .\start.ps1 ใน PowerShell)
3. เปิด http://127.0.0.1:4340   ลิงก์ลัด: /#bot  /#order  /#history

ถ้า Windows ไม่ยอมรันสคริปต์: เปิด PowerShell แล้วพิมพ์
    powershell -ExecutionPolicy Bypass -File .\start.ps1

บอร์ดอ่านไฟล์ที่บอทเขียนอย่างเดียว ส่งออเดอร์ไม่ได้โดยโครงสร้าง
ไม่มีกุญแจก็เปิดได้ ข้อมูลตลาดเป็นของสาธารณะ
'@
[System.IO.File]::WriteAllText((Join-Path $out "อ่านก่อน.txt"), $readme, $utf8bom)

if (Test-Path $zip) { Remove-Item -Force $zip -ErrorAction Stop }
Compress-Archive -Path (Join-Path $out "*") -DestinationPath $zip -ErrorAction Stop
$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "เสร็จ: $zip ($size MB)" -ForegroundColor Green
