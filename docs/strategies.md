# Writing a strategy

A strategy answers one question: *given the market right now, do I want to
open a position, and where is the stop?* It does not size the position, check
risk, or talk to the exchange.

## The shape

```python
from __future__ import annotations

import logging
from decimal import Decimal
from typing import Any, Optional

from ..money import D, ONE
from ..types import Signal
from .base import MarketView, Strategy


class BreakoutStrategy(Strategy):
    description = "enters on a break of the recent high"

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
        if len(prices) < self.lookback + 1:
            return None                      # still warming up

        prior_high = max(prices[-self.lookback - 1 : -1])
        if market.price <= prior_high:
            return None

        return Signal.build(
            symbol=market.symbol,
            side="buy",
            ref_price=market.price,
            stop_price=market.price * (ONE - self.stop_fraction),
            reason=f"broke {self.lookback}-bar high {prior_high}",
            created_at=market.now,
            valid_for_seconds=60,
        )
```

Register it so the CLI can find it:

```python
# tradebot/strategies/__init__.py
STRATEGIES = {
    "random": RandomStrategy,
    "ma_cross": MaCrossStrategy,
    "breakout": BreakoutStrategy,
}
```

## What `MarketView` gives you

| Field | |
|---|---|
| `symbol` | What you are trading |
| `price` | The current mark |
| `now` | Timezone-aware UTC. **Use this, never `datetime.now()`** |
| `equity` | Account equity, for information only — do not size with it |
| `position` | The open position on this symbol, or `None` |
| `recent_prices` | Rolling window of observed marks, oldest first |
| `in_position` | Shorthand for `position is not None` |

`recent_prices` is primed from real candles at start-up - `trading.timeframe`
and `trading.warmup_candles` in the config - and then extended with each
observed mark. So a 30-point indicator has its 30 points on the first
iteration. What it does *not* get is highs, lows or volume: only closes. A
strategy that needs true OHLCV needs `MarketView` widened first (see
`docs/architecture.md`).

## Rules

1. **No I/O.** No `requests`, no file reads, no broker calls, no `datetime.now()`.
2. **Every signal carries a stop**, on the losing side of entry: below for a
   long, above for a short. Further than one tick away.
3. **Return `None` rather than raising** when data is missing. A raise is
   caught and logged, but it wastes an iteration.
4. **Do not size.** No quantity, no notional, no `confidence` that hopes to
   influence size. The gate ignores all of it by design.
5. **Write a `reason` you can read in a week**, with the numbers in it. It
   goes to `logs/audit.jsonl` and is the first thing you will look at.

## Testing

Build `MarketView` objects by hand. No fixtures, no network, no mocks.

```python
def view(price, prices=()):
    return MarketView(
        symbol="BTC_USDT_Perp",
        price=D(price),
        now=datetime(2026, 11, 4, 12, 0, tzinfo=timezone.utc),
        equity=D("10000"),
        recent_prices=tuple(D(p) for p in prices),
    )


def test_fires_on_a_break_of_the_high():
    strategy = BreakoutStrategy(lookback=3, stop_pct="1")
    signal = strategy.get_signal(view(110, prices=[100, 101, 102, 110]))
    assert signal is not None
    assert signal.stop_price < signal.ref_price


def test_does_not_fire_inside_the_range():
    strategy = BreakoutStrategy(lookback=3)
    assert strategy.get_signal(view(101, prices=[100, 101, 102, 101])) is None
```

Cover at least: warm-up silence, the condition firing, the condition *not*
firing one step earlier, the stop side for both directions, and silence while
in a position.

A caution learned while writing `test_strategies.py`: a crossover needs a
price series where the cross happens on the **last** point and nowhere
earlier. `[100] * 7 + [130]` is a cross; a steadily rising ramp is not, even
though it looks like one.

## Then watch it

```bash
pytest tests/test_breakout_strategy.py -v
tradebot --dry-run --strategy breakout --iterations 50
grep -o '"code": "[a-z_]*"' logs/audit.jsonl | sort | uniq -c | sort -rn
```

If every signal is refused, `docs/README.th.md` and
`ai/workflows/debug-refusal.md` explain each code. A wall of
`below_min_notional` usually means the account is too small for that symbol
with that stop — which is a real answer, not a configuration problem to be
tuned away.

## The two that ship

- **`random`** — a coin flip with a fixed stop. No edge; it exists to exercise
  the pipeline end to end.
- **`ma_cross`** — fast/slow SMA crossover. A worked example of structure:
  warm-up handling, cross detection on the last point, an explicit stop. Read
  it as a template, not a recommendation.
