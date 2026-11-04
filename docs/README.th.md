# คู่มือ tradebot (ภาษาไทย)

บอทเทรด perpetual futures บน **GRVT** และ **Hyperliquid** ที่ออกแบบให้ **ส่วนที่ทำเงินหายได้ คือส่วนที่เทสง่ายที่สุด**

เลือกตลาดด้วยบรรทัดเดียวใน config

```yaml
exchange: grvt        # หรือ hyperliquid
```

ทุกอย่างเหนือชั้น broker — risk gate, engine, กลยุทธ์, dashboard, เทสทั้งหมด — เป็นโค้ดชุดเดียวกันไม่ว่าจะเลือกตลาดไหน

ทุกออเดอร์ต้องผ่านฟังก์ชันเดียวชื่อ `risk_gate()` ซึ่งคำนวณขนาดไม้จากงบความเสี่ยงกับระยะ stop และปฏิเสธออเดอร์ได้ด้วยเหตุผล 13 แบบที่มีชื่อชัดเจน ฟังก์ชันนี้ไม่ต่อเน็ต ไม่อ่านไฟล์ ไม่มีนาฬิกาของตัวเอง — จึงใช้โค้ดชุดเดียวกันได้ทั้ง live, paper และ backtest (ถ้ามีในอนาคต)

---

## ต้องมีอะไรก่อน

| | |
|---|---|
| Python | 3.10 ขึ้นไป (เช็คด้วย `python --version`) |
| Git | ใช้ clone |
| บัญชีเทรด | **ไม่ต้องมี** สำหรับติดตั้ง รันเทส และเทรดกระดาษ |

แค่นี้จริงๆ ตัว core พึ่งพาแค่ PyYAML ตัวเดียว

## ติดตั้ง

### Windows (PowerShell)

```powershell
git clone https://github.com/borntoquantdev-star/trade-bot.git
cd trade-bot

python -m venv .venv
.venv\Scripts\python.exe -m pip install --upgrade pip
.venv\Scripts\python.exe -m pip install -e ".[dev]"
```

ถ้าอยากพิมพ์ `tradebot` สั้นๆ ไม่ต้องใส่ path ยาว ให้ activate ก่อน

```powershell
.venv\Scripts\Activate.ps1
```

ถ้า PowerShell ขึ้นว่า *"running scripts is disabled on this system"* ให้อนุญาตเฉพาะ session นี้

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
.venv\Scripts\Activate.ps1
```

หรือจะไม่ activate เลยก็ได้ เรียก `.venv\Scripts\tradebot.exe` กับ `.venv\Scripts\python.exe -m pytest` ตรงๆ ผลเหมือนกันเป๊ะ

### macOS / Linux

```bash
git clone https://github.com/borntoquantdev-star/trade-bot.git
cd trade-bot

python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
python -m pip install -e ".[dev]"
```

แล้วแก้ `.vscode/settings.json` หนึ่งบรรทัด

```jsonc
"python.defaultInterpreterPath": "${workspaceFolder}/.venv/bin/python",
```

### เช็คว่าติดตั้งสำเร็จ

```bash
pytest                       # ต้องได้ 236 passed, 8 skipped
tradebot --list-strategies   # ต้องเห็น ma_cross กับ random
tradebot --dry-run --doctor  # ต้องจบด้วย "--- all good ---"
```

ทั้งสามคำสั่งทำงาน **offline** ไม่ต้องมี credential ไม่ต้องต่อเน็ต ไม่ต้องลง SDK ถ้า `pytest` ผ่านก็ถือว่าติดตั้งครบแล้ว

ที่ skip 8 ตัวคือเทสที่จะยิงออเดอร์จริงบน testnet ต้องใส่ `--run-integration` และต้องมี credential จึงจะรัน

### ลงเพิ่มเมื่อจะเทรดจริง

```bash
pip install -e ".[grvt]"            # ลง grvt-pysdk
pip install -e ".[hyperliquid]"     # ลง hyperliquid-python-sdk
```

SDK ไม่ได้อยู่ใน default install โดยเจตนา บอทจะ import มันตอนที่ broker ตัวจริงเชื่อมต่อเท่านั้น เพราะงั้นเทสกับ paper trading จึงไม่ต้องใช้

---

## รันครั้งแรก

```bash
tradebot --dry-run --iterations 5
```

นี่คือเทรดกับตัวจำลองในเครื่อง ดูว่ามันตัดสินใจอะไรไปบ้าง

```powershell
Get-Content logs\audit.jsonl      # macOS/Linux: cat logs/audit.jsonl
```

1 บรรทัด = 1 รอบการทำงาน · บรรทัดที่มี `"order"` คือออเดอร์ที่ผ่านด่าน · มี `"reject"` คือบอกว่าติดเพดานข้อไหน · ไม่มีทั้งสองอย่างคือกลยุทธ์ยังไม่อยากเทรด

⚠️ **ตอนทดลองให้ใส่ `--iterations` ด้วย** ถ้าไม่ใส่มันจะวนไปเรื่อยๆ โดยหลับ `loop_interval` วินาที (ค่า default 60) ระหว่างรอบ · กด `Ctrl+C` หยุดได้สะอาดเสมอ

เติม `--paper-walk` ด้วยจะทำให้ราคาจำลองขยับ ไม่ใช่นิ่งอยู่ที่เดิม — ดูสนุกกว่าและเห็นว่าระบบทำงาน

## ดูบอททำงานแบบ realtime

```bash
tradebot --dry-run --paper-walk --iterations 60   # หน้าต่างที่ 1: บอท
node dashboard/server.js --open                   # หน้าต่างที่ 2: dashboard
```

เปิด http://127.0.0.1:4317 จะเห็น **5 node ต่อกัน** — ข้อมูลตลาด → กลยุทธ์ → ด่านความเสี่ยง → ส่งคำสั่ง → ไม้ที่ถืออยู่ — แต่ละอันเปลี่ยนสีตามสถานะทันทีที่บอทตัดสินใจรอบใหม่ พร้อมกราฟราคาที่บอทเห็นและเหตุผลที่ถูกปฏิเสธเป็นภาษาไทย

สีขอบ: เทา = รอ · เหลือง = กำลังทำ · เขียว = พร้อม · แดง = ถูกปฏิเสธ · ฟ้าประ = จำลอง (paper)

dashboard **อ่านไฟล์ `logs/` ฝ่ายเดียว** สั่งอะไรบอทไม่ได้ ไม่ต่อตลาดเอง และปิดทิ้งก็ไม่กระทบบอทที่กำลังรัน · ไม่ต้อง `npm install` อะไรเลย · รายละเอียดใน [dashboard/README.md](../dashboard/README.md)

## ตั้งค่า

```powershell
Copy-Item config\config.example.yaml config\config.yaml
# macOS/Linux: cp config/config.example.yaml config/config.yaml
```

`config/config.yaml` อยู่ใน .gitignore ไม่ถูก commit ขึ้น git แน่นอน · ไฟล์ตัวอย่างใช้เทรดกระดาษได้ทันทีโดยไม่ต้องแก้

```yaml
trading:
  symbol: "BTC"              # Hyperliquid เรียกชื่อสินค้าเป็นชื่อเหรียญ ไม่ใช่คู่เทรด
  timeframe: "5m"            # แท่งเทียนที่ใช้ warm-up ตอนเริ่ม
  loop_interval: 60          # ตอนทดลองลดเป็น 5 จะดูสนุกกว่า
  strategy: "random"         # ดูตัวเลือกด้วย tradebot --list-strategies
  warmup_candles: 200

risk:
  # เขียนเป็นเปอร์เซ็นต์แบบที่คนพูดกัน (1.0 คือ 1%)
  risk_per_trade_pct: 1.0    # เสียเท่านี้ของพอร์ตถ้าโดน stop
  max_positions: 1           # เปิดพร้อมกันได้กี่ไม้
  max_daily_loss_pct: 3.0    # พอร์ตลดจากต้นวัน (UTC) เท่านี้ → หยุดบอท
  max_drawdown_pct: 10.0     # พอร์ตลดจากจุดสูงสุดเท่านี้ → หยุดบอท
  max_leverage: 5            # เพดาน exposure = ทุน × ค่านี้
  max_slippage_pct: 0.1      # และกลายเป็น limit price ของทุกออเดอร์
  fee_rate_pct: 0.045        # ค่าธรรมเนียมต่อขา ใช้ตอนคำนวณขนาดไม้
  allow_short: true
```

### กุญแจสองตัวของ Hyperliquid — อ่านตรงนี้สองรอบ

เรื่องนี้พลาดกันบ่อยที่สุด และมันพลาดแบบ **เงียบ** ไม่มี error

```yaml
hyperliquid:
  network: "testnet"
  account_address: "0x..."   # บัญชีแม่ (MASTER) — ทุกการ query ใช้ตัวนี้
  api_wallet_key: "0x..."    # private key ของ API wallet — ใช้เซ็นเท่านั้น
```

สร้าง API wallet ได้ที่ [app.hyperliquid.xyz/API](https://app.hyperliquid.xyz/API)

ถ้าเอา address ของ API wallet ไป query แทน address บัญชีแม่ ตลาดจะตอบกลับเป็น **สถานะว่าง** โดยไม่แจ้ง error เลย (doc ของ Hyperliquid เตือนเรื่องนี้ไว้เอง) — เพราะงั้น config ตัวนี้จะตรวจรูปแบบ address ให้ และจะปฏิเสธถ้าเอา private key มาใส่ช่อง address

ถ้าไม่อยากให้กุญแจอยู่ในไฟล์เลย ให้อ้างชื่อตัวแปรสภาพแวดล้อม

```yaml
  api_wallet_key: "${HYPERLIQUID_API_WALLET_KEY}"
```

`${ตัวแปร}` ที่ยังไม่ได้ set จะ error ทันที — ตั้งใจให้เป็นแบบนั้น

## ขึ้น testnet

```bash
pip install -e ".[hyperliquid]"
# ใส่ค่าใน config/config.yaml แล้ว
tradebot --doctor            # ตรวจ config, การเชื่อมต่อ, ราคา, สเปค, ยอดเงิน, แท่งเทียน
tradebot --live
```

ถ้า `network: mainnet` จะ **บังคับ** ให้ใส่ `--live` ด้วย ไม่ใส่คือบอทไม่ยอมเริ่มและไม่ส่งอะไรเลย

## คำสั่งที่ใช้บ่อย

| คำสั่ง | ทำอะไร |
|---|---|
| `tradebot --dry-run` | เทรดกระดาษแบบ offline ไม่ต้องมี credential |
| `tradebot --live` | ยิงออเดอร์จริง (ถ้า `network: mainnet` บังคับต้องมีแฟล็กนี้) |
| `tradebot --doctor` | ตรวจ config, การเชื่อมต่อ, ราคา, สเปคสินค้า, ยอดเงิน, แท่งเทียน |
| `tradebot --list-strategies` | ดูกลยุทธ์ที่ลงทะเบียนไว้ |
| `tradebot --resume` | ปลดล็อกสถานะ halt ที่ risk gate เขียนไว้ |
| `tradebot --close --yes` | ปิด position ของ symbol ที่ตั้งไว้ |
| `tradebot --iterations N` | หยุดหลังวน N รอบ |
| `tradebot --log-level DEBUG` | เวลาอยากดูละเอียด |
| `tradebot --dry-run --paper-walk` | เทรดกระดาษแบบราคาขยับจริง |
| `node dashboard/server.js --open` | เปิด dashboard ดูการทำงาน (อ่านอย่างเดียว) |

---

## ตัดสินใจเทรดอย่างไร

```
broker: ราคา mark, ยอดเงิน, position ที่เปิดอยู่, และแท่งเทียนตอนเริ่ม
        |
        v
strategy.get_signal(market)  -->  Signal (ฝั่ง, ราคาอ้างอิง, ราคา stop, เหตุผล)
        |                          stop เป็นของบังคับ เพราะมันคือตัวกำหนดขนาดไม้
        v
risk_gate(...)
        |
        +-->  Reject(code, detail)   บันทึก log แล้วไม่ส่งอะไรเลย
        |
        +-->  SizedOrder             qty = งบเสี่ยง ÷ (ระยะ stop + ค่าธรรมเนียมสองขา)
                 |                   limit price กัน slippage ไว้ในตัวออเดอร์
                 v
          broker.place_order         จุดเดียวที่แตะเน็ตเวิร์ก
```

engine **ไม่มีกฎการเทรดของตัวเองเลย** มันปรับขนาดไม้ไม่ได้ ข้ามด่านไม่ได้ ลองใหม่ด้วยตัวเลขอื่นไม่ได้ — `risk_gate` คืนอะไรมาก็ส่งอย่างนั้น หรือถ้าปฏิเสธก็แค่บันทึกไว้

## การคำนวณขนาดไม้

ถ้าโดน stop จะเสีย = ระยะ stop **บวก** ค่าธรรมเนียมทั้งขาเข้าและขาออก ซึ่งค่าธรรมเนียมขึ้นกับ qty ที่ยังหาคำตอบไม่ได้ (วนเป็นงูกินหาง) จึงแก้ด้วยการจัดสมการ ไม่ใช่เผื่อเอา

```
qty = งบเสี่ยง / (ระยะstop + (ราคาเข้า + ราคาstop) × feeRate)
```

แล้วปัด qty **ลง** ตาม lot step เสมอ การปัดจึงทำให้เสี่ยงน้อยกว่างบได้เท่านั้น ไม่มีทางเกิน — มีเทสชื่อ `test_risk_amount_never_exceeds_the_budget` ไล่หลายระยะ stop ยืนยันไว้

พอชน `max_daily_loss_pct` หรือ `max_drawdown_pct` บอทจะ **halt** ตัวเอง เขียนเหตุผลลง `logs/state.json` แล้วหยุด — สถานะนี้อยู่ข้ามการรีสตาร์ตโดยเจตนา ปลดได้ด้วย `--resume` เท่านั้น

## เรื่องเฉพาะของ Hyperliquid ที่บอทจัดการให้แล้ว

- **ราคามีกฎสองข้อพร้อมกัน** คือไม่เกิน 5 significant figures **และ** ไม่เกิน `6 − szDecimals` ตำแหน่งทศนิยม ยกเว้นจำนวนเต็มที่ผ่านตลอด · tick size เดียวอธิบายกฎนี้ไม่ได้ `InstrumentSpec` จึงบังคับทั้งสองข้อ และปัดไปในทางที่ปลอดภัยเสมอ
- **ขนาดไม้** ปัดลงตาม `10^−szDecimals` อ่านมาจาก metadata ของตลาดจริง ไม่ได้ตั้งมือ
- **ออเดอร์ขั้นต่ำ $10** ต่ำกว่านั้นตลาดตอบว่า "Order must have minimum value of $10." บอทจึงปฏิเสธก่อนด้วย `below_min_notional`
- **client order id ขนาด 128 bit** ของเราคือ `sha256` ของตัวสัญญาณ 32 ตัวอักษรพอดี แปลว่าตลาดบังคับกฎห้ามซ้ำให้อีกชั้น
- **แท่งเทียน** ถูกโหลดมา warm-up ให้กลยุทธ์ตอนเริ่ม อินดิเคเตอร์ที่ต้องการ 50 จุดจึงมีข้อมูลครบตั้งแต่รอบแรก ไม่ต้องรอ 50 รอบ

## ถ้าบอทไม่ยอมเทรด — ตารางเหตุผล

ดูที่ `logs/audit.jsonl` หรือนับรวม

```bash
grep -o '"code": "[a-z_]*"' logs/audit.jsonl | sort | uniq -c | sort -rn
```

เรียงจากร้ายแรงที่สุดลงมา (gate จะรายงานข้อที่ร้ายแรงที่สุดที่เข้าเงื่อนไข)

| code | แปลว่า | ทำอะไรต่อ |
|---|---|---|
| `halted` | บอทสั่งหยุดตัวเองไว้ | อ่าน `halt_reason` ใน `logs/state.json` ให้เข้าใจก่อน แล้วค่อย `--resume` **ห้ามปลดโดยไม่อ่าน** |
| `max_drawdown` | พอร์ตลดจากจุดสูงสุดเกินเพดาน | นี่คือ kill switch ควรหยุดทบทวนกลยุทธ์ ไม่ใช่ขยายเพดานเพื่อเทรดต่อ |
| `daily_loss_limit` | พอร์ตลดจากต้นวันเกินเพดาน | รีเซ็ตเองตอนเที่ยงคืน UTC · วัดจากการเปลี่ยนของพอร์ตทั้งหมด รวมไม้ที่เปิดมือเองด้วย |
| `expired` | สัญญาณหมดอายุก่อนถึง gate | มักเพราะลูปช้าหรือ broker ตอบช้า ดู `loop_interval` |
| `short_not_allowed` | กลยุทธ์สั่ง sell แต่ตั้ง `allow_short: false` | เลือกอย่างใดอย่างหนึ่ง |
| `invalid_stop` | stop อยู่ผิดฝั่ง ชิดกว่า 1 tick หรือ take profit ผิดฝั่ง | เป็นบั๊กในกลยุทธ์ แก้แล้วเขียนเทสคุม |
| `slippage_exceeded` | ราคาวิ่งหนีสัญญาณเกินเพดาน | ปกติในตลาดเหวี่ยง ถ้าเกิดทุกครั้งแปลว่าลูปช้าเกินไปสำหรับเหรียญนี้ |
| `already_in_position` | มีไม้ค้างอยู่ในเหรียญนั้น | ตั้งใจออกแบบไว้ บอทนี้ไม่เปิดซ้อน · เช็คด้วย `--doctor` ว่ามีไม้เก่าค้างไหม |
| `too_many_positions` | ครบเพดาน `max_positions` | ปกติถ้าเทรดหลายเหรียญ |
| `qty_rounds_to_zero` | งบเสี่ยงซื้อไม่ถึง 1 lot step | ทุนน้อยเกินไปสำหรับระยะ stop นี้ — ขยาย stop, เพิ่ม `risk_per_trade_pct` หรือเติมทุน |
| `below_min_notional` | ออเดอร์เล็กกว่า $10 ที่ตลาดรับ | เจอบ่อยที่สุดกับพอร์ตเล็ก คำตอบที่ตรงไปตรงมาคือ "ทุนยังไม่พอเทรดเหรียญนี้ด้วย stop แคบขนาดนี้" |
| `exceeds_exposure` | มูลค่ารวมจะเกิน ทุน × `max_leverage` | ปิดไม้เก่า หรือลดขนาดไม้ลง |
| `bad_instrument` | tick size / lot step / ราคา ใช้งานไม่ได้ | เทียบบล็อก `instrument:` กับค่าจริงของตลาด หรือดูว่า symbol สะกดถูกไหม |

ถ้าไม่มีบรรทัด `signal` เลยแปลว่ากลยุทธ์ยังไม่ออกสัญญาณ — เช็ค 3 ข้อ: (1) warm-up ได้แท่งเทียนมาน้อยกว่าที่อินดิเคเตอร์ต้องการ (2) มีไม้เปิดอยู่ (3) เงื่อนไขยังไม่เคยเกิดจริง

## ไฟล์ที่บอทเขียน

```
logs/audit.jsonl    1 บรรทัด = 1 การตัดสินใจ (signal / reject / order)
logs/state.json     ทุนต้นวัน, จุดสูงสุด, สถานะ halt, order id ที่ส่งแล้ว
logs/tradebot.log   log อ่านด้วยตาคน หมุนไฟล์อัตโนมัติ
```

`logs/` อยู่ใน .gitignore · order id บันทึกลงดิสก์ทันทีที่ออเดอร์ถูกรับ และไม่ส่งซ้ำ

## เขียนกลยุทธ์ของตัวเอง

3 ขั้น (รายละเอียดเต็มใน [docs/strategies.md](strategies.md))

1. สร้างคลาสสืบทอด `Strategy` ใน `tradebot/strategies/` เขียน `get_signal` แล้วคืน `Signal` ที่มี stop
2. ลงทะเบียนใน `tradebot/strategies/__init__.py`
3. เขียนเทสด้วยราคาสมมติ แล้วลอง `tradebot --dry-run --strategy <ชื่อ>`

หรือให้ AI พาทำ: พิมพ์ `/strategy-builder` ใน Claude Code หรือ Copilot Chat · ถ้าใช้ Codex บอกว่า "อ่าน `ai/workflows/strategy-builder.md` แล้วพาผมสร้างกลยุทธ์" · ดู [docs/vscode.md](vscode.md)

## ข้อจำกัดที่ต้องรู้ก่อนใช้

- **ไม่มี backtest** ไม่มีอะไรในรีโปนี้บอกได้ว่ากลยุทธ์กำไรหรือไม่ · paper กับ testnet พิสูจน์ได้แค่ว่าท่อต่อถูก
- **กลยุทธ์เห็นแค่ราคาปิด ไม่ใช่แท่งเต็ม** window ถูก prime จากแท่งเทียนจริงแล้วต่อด้วยราคาที่สังเกตได้ ยังไม่ใช่ OHLCV ครบชุด
- **paper fill แบบมองโลกสวย** คือได้ราคา limit ทันที ไม่มี partial fill ไม่มีคิว
- **บอทไม่แตะ leverage ของบัญชีคุณ** มันคุม exposure แทน
- `random` คือการโยนหัวก้อย ไม่มี edge อย่าเอาไปใช้กับเงินจริง

## ปัญหาที่เจอบ่อย

| อาการ | สาเหตุและทางแก้ |
|---|---|
| `python: command not found` | ลอง `py -3` บน Windows หรือ `python3` บน mac/Linux · ต้อง 3.10 ขึ้นไป |
| *"running scripts is disabled"* ตอน activate | นโยบายของ PowerShell ดูหัวข้อติดตั้ง หรือเรียก `.venv\Scripts\tradebot.exe` ตรงๆ |
| `tradebot: command not found` | ยังไม่ activate · ใช้ `.venv\Scripts\tradebot.exe` หรือ `python -m tradebot.cli.main` |
| `the Hyperliquid SDK is not installed` | เทรดจริงต้อง `pip install -e ".[hyperliquid]"` · paper ไม่ต้อง |
| `user state carried no accountValue` | คุณ query ด้วย address ของ API wallet · `account_address` ต้องเป็นบัญชีแม่ |
| `BTC_USDT_Perp is not a Hyperliquid perp` | ชื่อสินค้าเป็นชื่อเหรียญ: `BTC` `ETH` `SOL` |
| บอทรันแต่ไม่เทรดเลย | อ่าน reject code จากตารางข้างบน หรือใช้ `/debug-refusal` ให้ AI ช่วยอ่าน |
| `config refers to ${HYPERLIQUID_API_WALLET_KEY}` | ตัวแปรนั้นยังไม่ได้ set ใน shell นี้ · export ก่อน หรือใส่ค่าลง `config/config.yaml` |
| เหมือนค้าง ไม่มีอะไรขึ้น | มันหลับรอ `loop_interval` วินาที · ใส่ `--iterations N` ลด `loop_interval` หรือกด `Ctrl+C` |

## อ่านต่อ

- [docs/vscode.md](vscode.md) — ตั้งค่า VS Code และ AI ทั้งสามตัว (Codex / Claude Code / Copilot)
- [docs/architecture.md](architecture.md) — สถาปัตยกรรมและสัญญาของ risk gate
- [docs/strategies.md](strategies.md) — วิธีเขียนกลยุทธ์ของตัวเอง
- [dashboard/README.md](../dashboard/README.md) — dashboard ดูบอททำงาน realtime
- [docs/from-grvt-demo-bot.md](from-grvt-demo-bot.md) — ของเก่า grvt_demo_bot ต่างจากนี้อย่างไร
- [ai/workflows/strategy-builder.md](../ai/workflows/strategy-builder.md) — บทสัมภาษณ์ที่ AI ใช้พาสร้างกลยุทธ์
