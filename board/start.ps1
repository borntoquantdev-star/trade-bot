# เปิดบอร์ด — คำสั่งเดียว
#
#   .\start.ps1
#
# ทำสามอย่างให้: สร้างหน้าเว็บถ้ายังไม่มี · ชี้ตัวแปรให้ถูกที่ · เปิดเซิร์ฟเวอร์
# ที่ต้องชี้ให้ถูกมีสี่ที่ และจำยากทั้งสี่ ซึ่งคือเหตุผลที่ไฟล์นี้มีอยู่
#
#   BOARD_DIST     หน้าเว็บที่ trunk สร้าง อยู่ที่ app\dist ไม่ใช่ dist
#   BOARD_LOGS     ไฟล์ที่บอทเขียน อยู่ที่ ..\logs
#   BOARD_CANDLES  คลังแท่งเทียน อยู่ที่ ..\data\candles
#   BOARD_CODE     โค้ดของบอท อยู่ที่ .. (การ์ดห้าใบเปิดไฟล์จริงจากตรงนี้)
#   BOARD_CONFIG   คอนฟิกที่บอทใช้ อ่านแค่สองค่า (exchange กับ strategy) เพื่อรู้ว่าจะเปิดไฟล์ไหน
#                  ถ้ารันบอทด้วยคอนฟิกอื่น เช่น config	estnet.yaml ให้แก้บรรทัดข้างล่างตาม
#   BOARD_ENV_FILE กุญแจของบัญชี อยู่ที่ ..\config\.env (ไม่มีก็เปิดได้ ดูข้างล่าง)

param(
    # สำหรับคนพัฒนา: เปิดโหมดควบคุม (ปุ่มเริ่ม/หยุดบอท) ผูกกับ 127.0.0.1 เท่านั้น
    #   .\start.ps1 -Control
    # ไม่ใส่ = บอร์ดอ่านอย่างเดียวเหมือนเดิม นักเรียนใช้ TradeBot.exe แทน ไม่ต้องใช้สวิตช์นี้
    [switch]$Control
)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$page = Join-Path $PSScriptRoot "app\dist\index.html"
if (-not (Test-Path $page)) {
    Write-Host "ยังไม่มีหน้าเว็บ กำลังสร้างด้วย trunk (ครั้งแรกนานหน่อย)..." -ForegroundColor Yellow
    if (-not (Get-Command trunk -ErrorAction SilentlyContinue)) {
        Write-Host "ไม่มี trunk ในเครื่อง ติดตั้งด้วย: cargo install trunk" -ForegroundColor Red
        Write-Host "หรือจะดูแบบหน้าต่างเดสก์ท็อปก็ได้ ไม่ต้องใช้ trunk:" -ForegroundColor Yellow
        Write-Host "    cargo run -p board --bin board-native" -ForegroundColor Cyan
        exit 1
    }
    Push-Location (Join-Path $PSScriptRoot "app")
    trunk build --release
    Pop-Location
}

$env:BOARD_DIST = "app\dist"
$env:BOARD_LOGS = "..\logs"
$env:BOARD_CANDLES = "..\data\candles"
$env:BOARD_CODE = ".."
$env:BOARD_CONFIG = "config\config.yaml"
$env:BOARD_ENV_FILE = "..\config\.env"

# กุญแจไม่ใช่ของบังคับ: ไม่มีก็ดูตลาดได้ทุกอย่าง แค่การ์ดฝั่งบัญชีจะบอกว่ายังไม่ได้ตั้ง
$keyFile = Join-Path $PSScriptRoot "..\config\.env"
if (Test-Path $keyFile) {
    $hasKey = (Get-Content $keyFile | Where-Object { $_ -match "^\s*GRVT_API_KEY\s*=\s*\S" }).Count -gt 0
    if ($hasKey) {
        Write-Host "เจอกุญแจใน config\.env แล้ว การ์ดฝั่งบัญชีจะมีข้อมูลจริง" -ForegroundColor Green
    } else {
        Write-Host "config\.env มีอยู่แต่ยังไม่ได้ใส่ GRVT_API_KEY · การ์ดตลาดยังดูได้ตามปกติ" -ForegroundColor Yellow
    }
} else {
    Write-Host "ยังไม่มี config\.env · เปิดดูตลาดได้ตามปกติ ส่วนการ์ดบัญชีจะบอกว่ายังไม่ได้ตั้งกุญแจ" -ForegroundColor Yellow
}

if ($Control) {
    # เซิร์ฟเวอร์จะเปิดโหมดควบคุมก็ต่อเมื่อมีทั้งสองอย่างนี้ (BOARD_CONTROL=1 และผูกกับ loopback)
    $env:BOARD_CONTROL = "1"
    $env:BOARD_BIND = "127.0.0.1"
    $env:BOARD_BOT_ROOT = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
    Write-Host "โหมดควบคุมเปิด: หน้าเว็บสั่งเริ่ม/หยุดบอทใน $($env:BOARD_BOT_ROOT) ได้ (เฉพาะ 127.0.0.1)" -ForegroundColor Yellow
    Write-Host "บอทต้องมี .venv ก่อน: สร้างเองหรือเปิด TradeBot.exe หนึ่งครั้ง" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "เปิดบอร์ดที่  http://127.0.0.1:4340" -ForegroundColor Cyan
Write-Host "กด Ctrl+C เพื่อหยุด"
Write-Host ""

# --bin board-server: the crate also builds TradeBot.exe, the launcher.
cargo run -p board-server --bin board-server
