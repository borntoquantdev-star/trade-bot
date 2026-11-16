---
name: strategy-builder
title: Trading Strategy Builder
description: Interview the user about their trading idea, then turn it into a tested Strategy class for this bot. Use when they want to build, convert or change a trading strategy.
---

# Trading Strategy Builder

Turn a trading idea into a `Strategy` subclass that this repository can run,
with tests, through a guided interview. The person you are talking to may not
be a programmer: ask in plain language, one step at a time, and never dump a
form of twenty questions at once.

**Ask questions in Thai** (keeping English technical terms), because that is
the audience for this project. Write code, comments and tests in English.

## Before you start

Read `ai/conventions.md`. The rules there are not negotiable in this workflow;
in particular a strategy may not size a position, may not touch the network,
and every signal it emits carries a stop price.

Then check what already exists:

```bash
tradebot --list-strategies
ls tradebot/strategies/
```

If the user already has a strategy in this repo and wants to change it, skip
to Phase 3 and work on that file.

If `bot/strategy.yaml` exists, the bot-trade-studio plugin has already run the
interview (`/bot:strategy`): read it, read the rule back to the user as in
Phase 2, and go on to Phase 3 without asking again. The table "กลยุทธ์ที่ลอง"
in `bot/STATUS.md` says which strategies were tried and how far each got.

---

## Phase 0 — where are they starting from

Ask:

> มีกลยุทธ์ในใจอยู่แล้วหรือยัง
> **ก.** มีแล้ว อธิบายได้ → ข้าม Phase 1 ไป Phase 2
> **ข.** ยังไม่มี อยากให้ช่วยคิด → Phase 1
> **ค.** มีไอเดียลางๆ → Phase 1

---

## Phase 1 — discovery

One question at a time. Record each answer; you will read them all back in
Phase 2.

### 1.1 Trading style

> อยากเทรดสไตล์ไหน
> 1. **Trend following** — ไปตามเทรนด์ ซื้อตอนขึ้น ขายตอนลง
> 2. **Mean reversion** — สวนทาง ซื้อตอนลงแรงเกิน ขายตอนขึ้นแรงเกิน
> 3. **Breakout** — เข้าตอนทะลุแนวรับแนวต้าน
> 4. **ยังไม่รู้** — เล่าให้ฟังว่าปกติดูอะไรแล้วตัดสินใจเทรด เดี๋ยวช่วยจัดกลุ่มให้

### 1.2 Instrument and pace

Tell them what the platform allows first. Which exchange is in
`config/testnet.yaml` (`exchange:`); the class runs on GRVT:

> GRVT เป็น crypto perpetual futures เทรด 24/7 มี testnet ให้ลองฟรี
> ชื่อสินค้าแบบ `BTC_USDT_Perp` `ETH_USDT_Perp` · BTC มีมูลค่าขั้นต่ำต่อไม้ 100 USDT
> ถ้าทุนทดลองน้อย ให้ใช้เหรียญที่ขั้นต่ำ 5 ดอลลาร์ เช่น `SOL_USDT_Perp`
> (Hyperliquid ใช้ชื่อเหรียญเฉยๆ `BTC` `ETH` และขั้นต่ำ 10 USD)
> บอทจำกัด exposure ที่ ทุน × `risk.max_leverage` ส่วน leverage ของบัญชีเป็นอีกเรื่อง

Then ask for: symbol, how often it should look at the market
(`trading.loop_interval`), and how long a trade is expected to last.

The bot loads real candles at start-up (`trading.timeframe` and
`trading.warmup_candles`), so an indicator has history from the first
iteration. It still *decides* on a timer, once per `loop_interval`, so
anything that needs sub-minute reaction is the wrong tool. Say so rather than
building it.

### 1.3 The rule itself

> เข้าไม้เมื่อไหร่ บอกเป็นเงื่อนไขที่วัดได้
> ❌ "เมื่อกราฟดูดี"
> ✅ "เมื่อ SMA 10 ตัดขึ้นเหนือ SMA 30"

Push until the condition is something you could compute from a list of prices.
If they name an indicator this repo has no data for (volume, order book,
funding rate), say plainly that it is not available yet and offer the closest
thing that is.

### 1.4 The stop — not optional

> stop loss วางที่ไหน ถ้าเข้าแล้วราคาไปผิดทาง จะยอมเสียเท่าไหร่ถึงจะยอมรับว่าคิดผิด

Explain why you are insisting: `risk_gate` sizes the position from the stop
distance, so without a stop there is no position size. A fixed percentage is a
fine starting answer.

### 1.5 Risk per trade

> ขาดทุนต่อไม้ได้กี่ % ของพอร์ต (ปกติเริ่มที่ 0.5–1%)

This becomes `risk.risk_per_trade_pct` in the config, not something the
strategy decides.

---

## Phase 2 — read it back

Summarise in a table and get an explicit confirmation before writing code:

| | |
|---|---|
| ชื่อกลยุทธ์ | |
| สัญลักษณ์ / รอบการดู | |
| เงื่อนไขเข้า long | |
| เงื่อนไขเข้า short (หรือ long only) | |
| stop | |
| take profit (ถ้ามี) | |
| ความเสี่ยงต่อไม้ | |

> สรุปถูกไหม ถ้าถูกเดี๋ยวเขียนโค้ดให้

Do not continue on silence or a vague answer.

---

## Phase 3 — write the strategy

Create `tradebot/strategies/<name>.py`. Follow `ma_cross.py` as the model: a
warm-up guard, the rule, an explicit stop, and a `reason` string that will
make sense in a log a week later.

```python
"""One paragraph: what this strategy does and what it does not claim."""

from __future__ import annotations

import logging
from decimal import Decimal
from typing import Any, Optional

from ..money import D, ONE
from ..types import Signal
from .base import MarketView, Strategy


class ExampleStrategy(Strategy):
    description = "one line for --list-strategies"

    def __init__(self, config: Any = None, logger: Optional[logging.Logger] = None,
                 *, lookback: int = 20, stop_pct: Decimal | str | float = "1.0") -> None:
        super().__init__(config, logger)
        self.lookback = lookback
        self.stop_fraction = D(stop_pct) / 100
        if not 0 < self.stop_fraction < 1:
            raise ValueError("stop_pct must be between 0 and 100")

    def get_signal(self, market: MarketView) -> Optional[Signal]:
        if market.in_position:
            return None
        prices = list(market.recent_prices)
        if len(prices) < self.lookback:
            return None          # still warming up

        if not self._entry_condition(prices):
            return None

        return Signal.build(
            symbol=market.symbol,
            side="buy",
            ref_price=market.price,
            stop_price=market.price * (ONE - self.stop_fraction),
            reason="say exactly what fired, with the numbers",
            created_at=market.now,
            valid_for_seconds=60,
        )

    def get_exit(self, market: MarketView) -> Optional[str]:
        # The live loop does not stop a position out by itself. Without
        # this, a position rides past its stop for as long as the bot runs.
        held = market.position
        if held is None:
            return None
        stop = held.entry_price * (ONE - self.stop_fraction)
        return "stop hit" if self.stop_hit(market, stop) else None
```

Register it:

```python
# tradebot/strategies/__init__.py
STRATEGIES = {
    "random": RandomStrategy,
    "ma_cross": MaCrossStrategy,
    "<name>": ExampleStrategy,
}
```

Rules for this phase:

- No `import requests`, no broker, no file reads, no `datetime.now()` — use
  `market.now`.
- Return `None` rather than raising when the data is not there yet.
- Stop below entry for a long, above for a short. The gate refuses otherwise,
  and a test should prove you got it right.
- `get_exit()` must close the position once its stop is crossed, worked out
  from `market.position.entry_price`. `tradebot --check` fails a strategy
  without it, because live, nothing else will.

---

## Phase 4 — tests, before any trading

Add `tests/test_<name>_strategy.py`. Build `MarketView` objects by hand; never
touch a network.

Cover at least:

1. says nothing while warming up,
2. fires on the exact condition described in Phase 2,
3. does **not** fire just before that condition,
4. the stop is on the losing side of entry, for both directions,
5. returns `None` while a position is open.

```bash
pytest tests/test_<name>_strategy.py -v
ruff check tradebot tests
```

Then the check, which is the gate to Phase 5 - not your judgement and not
mine:

```bash
tradebot --config config/testnet.yaml --live --fetch-candles --days 1   # real candles, once
tradebot --config config/testnet.yaml --check --strategy <name>
```

It runs four layers (contract, these tests, real candles, paper) and ends in
PASS or NOT YET. On NOT YET, read the failed lines to the user in Thai, fix
the code, and run it again. **Do not go on to Phase 5 until it says PASS**,
and run it again after every change to the strategy file: the fingerprint in
`logs/check-<name>.json` belongs to the exact file that passed.

Then watch it run offline, where nothing can be lost:

```bash
tradebot --dry-run --strategy <name> --iterations 20
```

Read `logs/audit.jsonl`. Every line is one decision. Check that the refusals
make sense — a strategy whose signals are all refused for
`below_min_notional` needs a bigger account or a wider stop, not more code.

---

## Phase 5 — testnet, then a decision about real money

```bash
tradebot --config config/testnet.yaml --check --strategy <name>     # again: must PASS
tradebot --config config/testnet.yaml --live --doctor
tradebot --config config/testnet.yaml --live --strategy <name> --iterations 24 --close-at-end
```

Keys live in `config/.env`, typed by the user, never pasted into the chat.
Afterwards, go through the run with them (the plugin's `/bot:review`): what
fired, what was refused and why, what the exits were. Any change to the
strategy goes back through `--check` before the next `--live`.

Let it run long enough to produce real refusals and at least a few fills.
Then tell the user, in plain terms:

- what the audit trail actually shows (number of signals, fills, refusals by
  code),
- that a check pass, paper and testnet say nothing about profitability; that
  question belongs to `tradebot --backtest` (see `docs/backtest.md`) and to time,
- that going to `prod` is their decision, needs `--live` and a real account,
  and should start at a size they would not mind losing entirely.

Never set `env: prod` for them. Never describe a strategy as profitable on the
evidence this repo can produce.

---

## Finishing up

Leave behind:

- `tradebot/strategies/<name>.py` with the strategy,
- `tests/test_<name>_strategy.py`, passing,
- the entry in `STRATEGIES`,
- `docs/strategies.md` updated with a short section describing the rule,
- a summary of what to watch in the audit trail.
