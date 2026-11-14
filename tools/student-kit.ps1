# ชุดนักเรียน — zip เดียว ได้ทั้งบอทและบอร์ด ไม่ต้องพึ่ง GitHub ไม่ต้องลง Rust
# (ทำโดย Claude คืน 14 พ.ย. 2026 — รอรีวิว)
#
#   .\board\pack.ps1          # ก่อน: build ชุดบอร์ด (exe + หน้าเว็บ)
#   .\tools\student-kit.ps1   # แล้ว: รวมเป็นชุดนักเรียน
#
# ได้ board\pack\student-kit.zip ข้างในเป็นโฟลเดอร์ trade-bot\ ที่มี
#   โค้ดบอททั้งหมด         จาก git (ของที่ commit แล้วเท่านั้น: ไม่มี logs\ ไม่มี .env ไม่มี .venv)
#   board-pack\            บอร์ดพกพา วางตรงตำแหน่งที่ start.ps1 ของมันหาไฟล์บอทเจอ
#   00-START-HERE.txt          สามคำสั่งแรก
#
# ทำไมต้องมี: แม่แบบบน GitHub ที่คู่มือให้ clone ยังเป็นรุ่นก่อนมีบอร์ด
# จนกว่าจะ push งานล่าสุดขึ้นไป zip นี้คือทางเดียวที่นักเรียนจะได้รุ่นเดียวกับที่ผู้สอนใช้
#
# ตรวจก่อนบีบทุกครั้ง: ถ้าเจอไฟล์ .env, โฟลเดอร์ logs หรือ config.yaml ในชุด จะหยุดทันที
param([string]$Ref = "HEAD")

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$pack = Join-Path $root "board\pack\board-pack"
if (-not (Test-Path (Join-Path $pack "board-server.exe"))) {
    throw "ยังไม่มีชุดบอร์ด: รัน .\board\pack.ps1 ก่อน"
}

$work = Join-Path $root "board\pack\student-kit"
$zip = Join-Path $root "board\pack\student-kit.zip"
$src = Join-Path $root "board\pack\student-kit-src.zip"
if (Test-Path $work) { Remove-Item -Recurse -Force $work -ErrorAction Stop }
foreach ($old in @($zip, $src)) { if (Test-Path $old) { Remove-Item -Force $old -ErrorAction Stop } }

Write-Host "1/3 เอาโค้ดจาก git ($Ref)..." -ForegroundColor Cyan
git archive --format=zip -o $src $Ref
if ($LASTEXITCODE -ne 0) { throw "git archive ไม่ผ่าน" }
$bot = Join-Path $work "trade-bot"
Expand-Archive -Path $src -DestinationPath $bot -ErrorAction Stop
Remove-Item -Force $src -ErrorAction Stop

Write-Host "2/3 วางบอร์ดพกพา..." -ForegroundColor Cyan
Copy-Item -Recurse $pack (Join-Path $bot "board-pack") -ErrorAction Stop

$readme = @'
ชุดนักเรียน — บอทเทรด + บอร์ด (GRVT testnet, เงินทดลองทั้งหมด)

ติดตั้งครั้งเดียว (PowerShell ในโฟลเดอร์นี้):
    python -m venv .venv
    .venv\Scripts\python.exe -m pip install -e ".[dev]"
    .venv\Scripts\python.exe -m pytest -q          # ต้องผ่านก่อนทำอย่างอื่น

ลองของโดยไม่ต้องมีกุญแจ:
    .venv\Scripts\tradebot.exe --dry-run --paper-live --config config\demo.yaml --iterations 36
    .\board-pack\start.ps1                          # แล้วเปิด http://127.0.0.1:4340/#bot

ดูออเดอร์เดินทางจนจับคู่: เปิด http://127.0.0.1:4340/#order แล้วดูการ์ด "เส้นทางออเดอร์"
(เทรดกระดาษจะขึ้นว่า "จำลอง" — ต้องรันบน testnet ถึงจะเห็นการรอคิวและการจับคู่จริง)

กุญแจ testnet: ใส่เองในไฟล์ config\.env เท่านั้น ห้ามพิมพ์หรือวางกุญแจในแชท
คู่มือเต็ม: docs\student-guide.th.md   ใบงาน: docs\student-lab.th.md
'@
$utf8bom = New-Object System.Text.UTF8Encoding($true)
[System.IO.File]::WriteAllText((Join-Path $bot "00-START-HERE.txt"), $readme, $utf8bom)

Write-Host "3/3 ตรวจว่าไม่มีความลับติดไปแล้วบีบ..." -ForegroundColor Cyan
$bad = Get-ChildItem -Recurse -Force $bot | Where-Object {
    $_.Name -like ".env*" -or $_.Name -eq "config.yaml" -or ($_.PSIsContainer -and $_.Name -eq "logs")
}
if ($bad) {
    $bad | ForEach-Object { Write-Host "  เจอ: $($_.FullName)" -ForegroundColor Red }
    throw "หยุด: มีไฟล์ที่ไม่ควรแจกอยู่ในชุด"
}
Compress-Archive -Path $bot -DestinationPath $zip -ErrorAction Stop
$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "เสร็จ: $zip ($size MB)" -ForegroundColor Green
