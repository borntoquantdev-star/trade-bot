# สร้างชุดบอร์ดพกพาไว้แจกนักเรียน — ไม่ต้องลง Rust ไม่ต้องลง trunk
#
#   .\pack.ps1
#
# ได้ไฟล์ pack\board-pack.zip ข้างในมี
#   board-server.exe   เซิร์ฟเวอร์ของบอร์ด (build แบบ release) อ่านอย่างเดียว
#   TradeBot.exe       ตัวเปิดแบบดับเบิลคลิก: เซิร์ฟเวอร์เดียวกัน + ปุ่มสั่งบอท (เฉพาะ 127.0.0.1)
#   uv.exe             ตัวติดตั้ง Python ที่ TradeBot.exe ใช้สร้าง .venv ครั้งแรก (ปักเวอร์ชันไว้ ตรวจ SHA-256)
#   dist\              หน้าเว็บที่ build แล้ว
#   start.ps1          เปิดบอร์ดคำสั่งเดียว (โหมดอ่านอย่างเดียว)
#   README.txt        วิธีใช้สามบรรทัด
#
# นักเรียนแตก zip ไว้ "ในโฟลเดอร์บอท" (โฟลเดอร์ที่มี tradebot\ config\ logs\)
# ให้เป็น <โฟลเดอร์บอท>\board-pack\ แล้วรัน start.ps1 ข้างใน
# ตำแหน่งนี้สำคัญ เพราะ start.ps1 หาไฟล์ของบอทที่ .. เหมือน board\start.ps1 ทุกอย่าง
# (TradeBot.exe ฉลาดพอจะขึ้นไปหาโฟลเดอร์บอทที่อยู่ข้างบนเอง แต่ชุดนักเรียนวางไว้ที่ราก
#  คือ trade-bot\TradeBot.exe - ดู tools\student-kit.ps1)

# ไม่ใช้ $ErrorActionPreference = "Stop" ทั้งไฟล์: cargo กับ trunk เขียนความคืบหน้า
# ลง stderr และ PowerShell 5.1 จะถือว่านั่นคือ error ทันทีที่ output ถูก redirect
# จึงเช็ค $LASTEXITCODE ของคำสั่งภายนอกเอง และใส่ -ErrorAction Stop ให้ cmdlet ที่สำคัญ
Set-Location $PSScriptRoot

$out = Join-Path $PSScriptRoot "pack\board-pack"
$zip = Join-Path $PSScriptRoot "pack\board-pack.zip"

Write-Host "1/4 build เซิร์ฟเวอร์ + TradeBot.exe (release)..." -ForegroundColor Cyan
# -p board-server builds both programs of the crate: board-server.exe (the
# read-only server) and TradeBot.exe (the double-click launcher).
cargo build --release -p board-server
if ($LASTEXITCODE -ne 0) { throw "cargo build ไม่ผ่าน" }

Write-Host "2/4 build หน้าเว็บ (release)..." -ForegroundColor Cyan
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

Write-Host "3/4 uv (ตัวติดตั้งที่ TradeBot.exe ใช้สร้าง .venv ครั้งแรก)..." -ForegroundColor Cyan
# Pinned, not "latest": the kit a student gets must be the kit that was tested.
# 0.11.33 is the last 0.11 release (28 Jul 2026). The SHA-256 is the one GitHub
# publishes beside the release asset (uv-x86_64-pc-windows-msvc.zip.sha256),
# so a corrupted or swapped download stops the build instead of reaching a
# classroom. To move to another version, change both lines together.
$uvVersion = "0.11.33"
$uvSha256 = "c253ce868ad48d29327b661452ce184c9e333e6d6f5bc8d6fcfbf4dd52b83442"
$uvUrl = "https://github.com/astral-sh/uv/releases/download/$uvVersion/uv-x86_64-pc-windows-msvc.zip"
$cache = Join-Path $PSScriptRoot "pack\cache"
$uvZip = Join-Path $cache "uv-$uvVersion-x86_64-pc-windows-msvc.zip"
New-Item -ItemType Directory -Force $cache -ErrorAction Stop | Out-Null
if (-not (Test-Path $uvZip)) {
    # Windows PowerShell 5.1 does not offer TLS 1.2 by default, which GitHub requires.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    # 5.1 draws its progress bar so slowly that it multiplies the download time.
    $progress = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $uvUrl -OutFile $uvZip -ErrorAction Stop
    } catch {
        $ProgressPreference = $progress
        throw "ดาวน์โหลด uv ไม่ได้ ($uvUrl): $($_.Exception.Message)"
    }
    $ProgressPreference = $progress
}
$gotSha = (Get-FileHash -Algorithm SHA256 $uvZip -ErrorAction Stop).Hash.ToLower()
if ($gotSha -ne $uvSha256) {
    Remove-Item -Force $uvZip -ErrorAction SilentlyContinue
    throw "SHA-256 ของ uv ไม่ตรง (ได้ $gotSha ต้องเป็น $uvSha256) ลบไฟล์ที่โหลดแล้ว ลองใหม่"
}
$uvDir = Join-Path $cache "uv-$uvVersion"
if (Test-Path $uvDir) { Remove-Item -Recurse -Force $uvDir -ErrorAction Stop }
Expand-Archive -Path $uvZip -DestinationPath $uvDir -ErrorAction Stop
if (-not (Test-Path (Join-Path $uvDir "uv.exe"))) { throw "ใน zip ของ uv ไม่มี uv.exe" }

Write-Host "4/4 รวมเป็นชุด..." -ForegroundColor Cyan
if (Test-Path $out) { Remove-Item -Recurse -Force $out -ErrorAction Stop }
New-Item -ItemType Directory -Force $out -ErrorAction Stop | Out-Null
Copy-Item (Join-Path $PSScriptRoot "target\release\board-server.exe") $out -ErrorAction Stop
Copy-Item (Join-Path $PSScriptRoot "target\release\TradeBot.exe") $out -ErrorAction Stop
Copy-Item (Join-Path $uvDir "uv.exe") $out -ErrorAction Stop
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

ง่ายที่สุด: ดับเบิลคลิก TradeBot.exe
    ตัวนี้เปิดบอร์ดพร้อมปุ่มสั่งบอท (เริ่ม / หยุด / ตรวจกลยุทธ์ / ใส่กุญแจ) และติดตั้งตัวบอทให้เองครั้งแรก
    ต้องวางไว้ในโฟลเดอร์บอท (ที่มี tradebot\ config\) แล้วดับเบิลคลิก
    (ชุดนักเรียนวางไว้ให้แล้วที่ trade-bot\TradeBot.exe)

แบบอ่านอย่างเดียว (ไม่มีปุ่มสั่งบอท):
1. แตกไฟล์นี้ไว้ในโฟลเดอร์บอท (ที่มี tradebot\ config\ logs\) ให้เป็น <โฟลเดอร์บอท>\board-pack
2. คลิกขวา start.ps1 -> Run with PowerShell   (หรือพิมพ์ .\start.ps1 ใน PowerShell)
3. เปิด http://127.0.0.1:4340   ลิงก์ลัด: /#bot  /#order  /#history

ถ้า Windows ไม่ยอมรันสคริปต์: เปิด PowerShell แล้วพิมพ์
    powershell -ExecutionPolicy Bypass -File .\start.ps1

board-server.exe / start.ps1 อ่านไฟล์ที่บอทเขียนอย่างเดียว ส่งออเดอร์ไม่ได้โดยโครงสร้าง
ไม่มีกุญแจก็เปิดได้ ข้อมูลตลาดเป็นของสาธารณะ
ปุ่มสั่งบอทมีเฉพาะใน TradeBot.exe และเฉพาะที่ 127.0.0.1 (เครื่องตัวเองเท่านั้น)
'@
[System.IO.File]::WriteAllText((Join-Path $out "README.txt"), $readme, $utf8bom)

if (Test-Path $zip) { Remove-Item -Force $zip -ErrorAction Stop }
Compress-Archive -Path (Join-Path $out "*") -DestinationPath $zip -ErrorAction Stop
$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "เสร็จ: $zip ($size MB)" -ForegroundColor Green
