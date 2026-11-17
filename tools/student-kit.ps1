# ชุดนักเรียน — zip เดียว ได้ทั้งบอทและบอร์ด ไม่ต้องพึ่ง GitHub ไม่ต้องลง Rust
# (ทำโดย Claude คืน 14 พ.ย. 2026 — รอรีวิว)
#
#   .\board\pack.ps1          # ก่อน: build ชุดบอร์ด (exe + หน้าเว็บ)
#   .\tools\student-kit.ps1   # แล้ว: รวมเป็นชุดนักเรียน
#
# ได้ board\pack\student-kit.zip ข้างในมีสองโฟลเดอร์
#   trade-bot\                  โค้ดบอททั้งหมดจาก git (ของที่ commit แล้วเท่านั้น: ไม่มี logs\ .env .venv)
#     TradeBot.exe              ดับเบิลคลิกอันนี้ - บอร์ด + ปุ่มสั่งบอท + ติดตั้ง .venv ให้เอง (ผ่าน uv)
#     board-pack\               บอร์ดพกพา วางตรงตำแหน่งที่ start.ps1 ของมันหาไฟล์บอทเจอ
#                               (มี uv.exe ที่ TradeBot.exe ใช้ติดตั้งครั้งแรกด้วย)
#     00-START-HERE.txt         บรรทัดแรก: ดับเบิลคลิก TradeBot.exe · คำสั่งบรรทัดคำสั่งอยู่ในภาคผนวก
#   bot-trade-studio\           ปลั๊กอิน /bot:* (จาก git ของ ..ot-trade-studio) ติดตั้งจากโฟลเดอร์ได้
#                               ไม่ต้องพึ่ง marketplace บน GitHub
#
# ทำไมต้องมี: แม่แบบบน GitHub ที่คู่มือให้ clone ยังเป็นรุ่นก่อนมีบอร์ด
# จนกว่าจะ push งานล่าสุดขึ้นไป zip นี้คือทางเดียวที่นักเรียนจะได้รุ่นเดียวกับที่ผู้สอนใช้
#
# ตรวจก่อนบีบทุกครั้ง: ถ้าเจอไฟล์ .env, โฟลเดอร์ logs หรือ config.yaml ในชุด จะหยุดทันที
param(
    [string]$Ref = "HEAD",
    # The plugin repo, beside this one in the teacher's workspace. Missing is
    # not fatal: the kit still works, students just install the plugin the
    # usual way.
    [string]$Plugin = (Join-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) "bot-trade-studio"),
    [string]$PluginRef = "HEAD"
)

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$pack = Join-Path $root "board\pack\board-pack"
if (-not (Test-Path (Join-Path $pack "board-server.exe"))) {
    throw "ยังไม่มีชุดบอร์ด: รัน .\board\pack.ps1 ก่อน"
}
# The two files that make "double-click and go" work. A pack built before
# they existed would give students a kit whose first instruction fails.
foreach ($needed in @("TradeBot.exe", "uv.exe")) {
    if (-not (Test-Path (Join-Path $pack $needed))) {
        throw "ชุดบอร์ดไม่มี $needed (เป็นชุดเก่า): รัน .\board\pack.ps1 ใหม่"
    }
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

if (Test-Path (Join-Path $Plugin ".claude-plugin\marketplace.json")) {
    Write-Host "    + ปลั๊กอินจาก $Plugin ($PluginRef)" -ForegroundColor Cyan
    git -C $Plugin archive --format=zip -o $src $PluginRef
    if ($LASTEXITCODE -ne 0) { throw "git archive ของปลั๊กอินไม่ผ่าน" }
    Expand-Archive -Path $src -DestinationPath (Join-Path $work "bot-trade-studio") -ErrorAction Stop
    Remove-Item -Force $src -ErrorAction Stop
} else {
    Write-Host "    ไม่เจอปลั๊กอินที่ $Plugin - ชุดนี้จะไม่มีปลั๊กอิน" -ForegroundColor Yellow
}

Write-Host "2/3 วางบอร์ดพกพา..." -ForegroundColor Cyan
Copy-Item -Recurse $pack (Join-Path $bot "board-pack") -ErrorAction Stop
# TradeBot.exe is the first thing a student should see, so it sits at the root
# of trade-bot\ and not inside board-pack\. It works out the bot folder from
# where it sits, which is why it must be here. uv.exe stays in board-pack\,
# where the launcher looks for it.
Move-Item (Join-Path $bot "board-pack\TradeBot.exe") (Join-Path $bot "TradeBot.exe") -ErrorAction Stop

$readme = @'
ชุดนักเรียน — บอทเทรด + บอร์ด (GRVT testnet, เงินทดลองทั้งหมด)

เริ่มตรงนี้ — ไม่ต้องพิมพ์คำสั่งอะไรเลย
=======================================

  1. ดับเบิลคลิก TradeBot.exe
       - หน้าต่างสีดำเปิดขึ้น แล้วเบราว์เซอร์เปิดหน้าบอร์ดที่ส่วนควบคุมบอท
         (ถ้าเบราว์เซอร์ไม่เปิดเอง ให้เปิด http://127.0.0.1:4340/#control เอง)
       - ครั้งแรกมันติดตั้งตัวบอทให้เอง ต้องต่ออินเทอร์เน็ต ใช้เวลาไม่กี่นาที
         อย่าปิดหน้าต่างสีดำระหว่างนั้น
  2. ในหน้าเว็บ เลือก "กรณีศึกษา 1 · ping_pong" แล้วลอง "เทรดกระดาษ" ก่อน:
     ราคาจริง ออเดอร์เป็นกระดาษ ไม่ต้องมีกุญแจ
     (กรณีศึกษา 2 · ma_cross ตามเทรนด์ ไว้ลองรอบถัดไป)
     กลยุทธ์ที่คุณให้ AI เขียน (/bot:strategy) จะโผล่ในรายการเดียวกันว่า "ของคุณ · <ชื่อ>"
  3. จะใช้ testnet: ใส่กุญแจ GRVT testnet ในหน้าเว็บ (ช่องกุญแจ) แล้วให้กลยุทธ์ผ่านด่านตรวจก่อน
     บอทไม่ยอมเทรดบน testnet ถ้ายังไม่ผ่านด่านตรวจ
  4. เลิกใช้: กดหยุดในหน้าเว็บ แล้วปิดหน้าต่างสีดำ
     ปิดหน้าต่างสีดำ = ปิดโปรแกรม (บอทที่ยังรันอยู่จะถูกสั่งหยุดอย่างสุภาพก่อน)

ถ้า Windows ขึ้น "Windows protected your PC" (SmartScreen) ตอนเปิด TradeBot.exe:
  กด "More info" แล้วกด "Run anyway" · ไฟล์นี้ยังไม่ได้เซ็นชื่อดิจิทัล
  ถ้าโปรแกรมแอนตี้ไวรัสบล็อก ให้ครูช่วยอนุญาตโฟลเดอร์ trade-bot

กุญแจ testnet: ใส่ในหน้าเว็บ หรือในไฟล์ config\.env เท่านั้น ห้ามพิมพ์หรือวางกุญแจในแชท
คู่มือเต็ม: docs\student-guide.th.md   ใบงาน: docs\student-lab.th.md


ภาคผนวก — สำหรับครูและคนที่อยากใช้บรรทัดคำสั่งเอง (นักเรียนข้ามได้)
=====================================================================

ติดตั้งครั้งเดียว (PowerShell ในโฟลเดอร์นี้) — ถ้าเคยเปิด TradeBot.exe แล้ว มี .venv อยู่แล้ว ข้ามได้:
    python -m venv .venv
    .venv\Scripts\python.exe -m pip install -e ".[dev]"
    .venv\Scripts\python.exe -m pytest -q          # ต้องผ่านก่อนทำอย่างอื่น

ติดตั้งปลั๊กอิน /bot:* จากชุดนี้ (ถ้ามีโฟลเดอร์ bot-trade-studio อยู่ข้างๆ trade-bot):
    เปิด Claude Code ในโฟลเดอร์ trade-bot แล้วพิมพ์ทีละบรรทัด
    /plugin marketplace add <พาธเต็มของโฟลเดอร์ bot-trade-studio>
    /plugin install bot@bot-trade-studio-marketplace

ลองของโดยไม่ต้องมีกุญแจ:
    .venv\Scripts\tradebot.exe --dry-run --paper-live --config config\demo.yaml --iterations 36
    .\board-pack\start.ps1                          # แล้วเปิด http://127.0.0.1:4340/#bot

บอร์ดสองแบบ:
    TradeBot.exe            บอร์ด + ปุ่มสั่งบอท · ผูกกับ 127.0.0.1 เท่านั้น · ติดตั้ง .venv ให้เองด้วย uv
    board-pack\start.ps1    บอร์ดอ่านอย่างเดียว ไม่มีปุ่มสั่งบอท (แบบเดิม)
  อย่าเปิดสองตัวพร้อมกัน เพราะใช้พอร์ต 4340 เหมือนกัน

คำสั่งที่ปุ่มในหน้าเว็บรันให้ (ถ้าอยากรันเองจาก PowerShell ในโฟลเดอร์นี้):
    เทรดกระดาษ       .venv\Scripts\tradebot.exe --config config\demo.yaml --dry-run --paper-live --close-at-end
    เทรด testnet     .venv\Scripts\tradebot.exe --config config\testnet.yaml --live --close-at-end
    ตรวจกลยุทธ์      .venv\Scripts\tradebot.exe --config config\testnet.yaml --check
    ปลดล็อกหลังหยุด  .venv\Scripts\tradebot.exe --config config\demo.yaml --resume            (paper)
                     .venv\Scripts\tradebot.exe --config config\testnet.yaml --live --resume  (testnet)
  ปุ่ม "หยุด" ไม่ได้ฆ่าบอท: มันสร้างไฟล์ logs\stop.request ให้บอทจบรอบเอง (แล้วปิดสถานะที่ค้างด้วย --close-at-end)
  ถ้าผ่านไป 90 วินาทียังไม่จบ ค่อยบังคับปิด

ดูออเดอร์เดินทางจนจับคู่: เปิด http://127.0.0.1:4340/#order แล้วดูการ์ด "เส้นทางออเดอร์"
(เทรดกระดาษจะขึ้นว่า "จำลอง" — ต้องรันบน testnet ถึงจะเห็นการรอคิวและการจับคู่จริง)

ครูสร้างชุดนี้ใหม่: .\board\pack.ps1 แล้ว .\tools\student-kit.ps1
  (ชุดดึงโค้ดบอทจาก git ที่ commit แล้วเท่านั้น: แก้อะไรไว้ที่ยังไม่ commit จะไม่เข้าชุด)
'@
$utf8bom = New-Object System.Text.UTF8Encoding($true)
[System.IO.File]::WriteAllText((Join-Path $bot "00-START-HERE.txt"), $readme, $utf8bom)

Write-Host "3/3 ตรวจว่าไม่มีความลับติดไปแล้วบีบ..." -ForegroundColor Cyan
$bad = Get-ChildItem -Recurse -Force $work | Where-Object {
    $_.Name -like ".env*" -or $_.Name -eq "config.yaml" -or ($_.PSIsContainer -and $_.Name -eq "logs")
}
if ($bad) {
    $bad | ForEach-Object { Write-Host "  เจอ: $($_.FullName)" -ForegroundColor Red }
    throw "หยุด: มีไฟล์ที่ไม่ควรแจกอยู่ในชุด"
}
Compress-Archive -Path (Join-Path $work "*") -DestinationPath $zip -ErrorAction Stop
$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "เสร็จ: $zip ($size MB)" -ForegroundColor Green
