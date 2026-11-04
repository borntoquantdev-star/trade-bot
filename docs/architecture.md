# Architecture

## Layers

```
cli/main.py        arguments, one-shot actions, choosing paper vs live
      │
core/engine.py     the loop; owns no trading rules
core/config.py     YAML + environment, validated once, then immutable
      │
      ├── strategies/   market data in, Signal or None out. No I/O.
      └── brokers/      the only code that touches the network
                │
      risk.py    risk_gate(): approves or refuses. Pure.
      types.py   immutable values: Signal, SizedOrder, Reject, limits
      money.py   Decimal helpers
```

Imports only ever point inwards. `risk.py` knows nothing about brokers,
strategies, configuration files or the exchange.

## The risk gate contract

`risk_gate()` is the only function that may decide an order exists. It is:

- **pure** — no network, no files, no randomness, no LLM, and `now` is an
  argument rather than a call to the clock;
- **synchronous** — never `async`;
- **blind to confidence** — size comes from the risk budget and the stop
  distance only. A strategy cannot argue for a bigger position.

That is not purism. It is what lets one implementation serve paper trading,
live trading and a future backtest. The moment the gate can read a file or
await something, the backtest needs its own copy, the two copies drift, and
the number you trusted in the backtest is not the number that trades.

### Sizing

Being stopped out costs the stop distance *plus* the fees on both legs, and
the fees depend on the quantity still being solved for. Rather than padding a
guess:

```
risk      = qty x stop_distance + qty x (entry + stop) x fee_rate
qty       = risk_budget / (stop_distance + (entry + stop) x fee_rate)
```

The result is floored to the instrument's lot step — down, never up, so
rounding can only reduce risk below the budget. `test_risk_amount_never_
exceeds_the_budget` asserts that across a range of stop distances.

### Slippage, twice

1. **Before sending:** if the mark has moved against the signal by more than
   `max_slippage`, the order is refused.
2. **Inside the order:** the limit price is `entry x (1 ± max_slippage)`,
   snapped to the tick in the protective direction. However far price travels
   while the order is in flight, the exchange cannot fill us worse than that.

A market order would throw the second guard away, which is why entries are
limit orders. Exits (`--close`) are market orders on purpose: when you want
out, a protective price that never fills is the worst outcome.

### Refusal order

`RejectCode` is declared most serious first, and the gate runs its checks in
that same order, so a log line answers "what stopped the bot" rather than
"what it happened to notice last":

```
halted → max_drawdown → daily_loss_limit → expired → short_not_allowed
→ invalid_stop → slippage_exceeded → already_in_position → too_many_positions
→ qty_rounds_to_zero → below_min_notional → exceeds_exposure → bad_instrument
```

`docs/README.th.md` has the table of what each one means in practice.

## State that outlives the process

`logs/state.json` holds the day's starting equity, peak equity, the halt flag
and the ids of orders already sent. Without it, a restart would reset the
daily loss limit — precisely when you least want it reset — and would forget
which orders had gone out.

The daily loss is measured as **equity change since the first tick of the UTC
day**, not as a sum of closed trades. That catches open-position moves and
anything done by hand in the exchange UI, at the cost of being a blunter
instrument than realised P&L.

Drawdown is measured from the highest equity ever recorded in that file.

## Prices: two rules at once

Hyperliquid perps cap prices at **5 significant figures** *and* at
**`6 - szDecimals` decimal places**, with integer prices always accepted. One
tick size cannot express that, because the allowed increment depends on the
price's magnitude: near 100,000 only whole numbers fit in five figures, while
near 1,000 a tenth does.

`InstrumentSpec.round_price` therefore takes the coarser of two steps - the
tick from `6 - szDecimals`, and the one implied by the significant-figure
limit at that magnitude - and rounds in the protective direction. The
documented examples are pinned down in `tests/test_price_rules.py`.

Sizes are simpler: floor to `10 ** -szDecimals`, read from the exchange's perp
metadata rather than configured by hand.

## Idempotency

`client_order_id` is `sha256(symbol | side | ref_price | stop_price |
valid_until)`, truncated to 32 hex characters - exactly the 128-bit cloid
Hyperliquid accepts. The same signal always produces the same id, so:

- the engine refuses to send an id it has already sent, and
- the exchange receives the id too, so the ambiguous case — a timeout where
  the order did reach the book — is caught on their side.

A broker *rejection* deliberately does not record the id, so a genuine retry
remains possible.

## Deliberate limitations

**Strategies see closes, not bars.** `MarketView.recent_prices` is primed
from real candles at start-up - `trading.timeframe` and
`trading.warmup_candles` - and then extended with each observed mark. So an
indicator has history from the first iteration, but it only ever sees closing
prices: no highs, no lows, no volume. A strategy that needs true OHLCV (ATR,
volume profile) needs `MarketView` widened to carry the `Candle` objects the
broker already returns.

**No backtester.** The gate is pure so that one can be written: feed historical
candles to a strategy, call the same `risk_gate`, simulate fills. Nothing in
this repository currently tells you whether a strategy makes money.

**Optimistic paper fills.** `PaperBroker` fills at the limit price,
immediately, with no partial fills, no queue position and no funding. It
proves the pipeline works.

**Leverage is not configured by the bot.** `HyperliquidBroker.set_leverage`
exists and works, but the engine never calls it: changing an account setting
is not something a trading loop should do behind your back. What the bot
enforces is *exposure*, capped at `equity x risk.max_leverage`, which is a
different thing and the one it can guarantee by itself.

## Where to add things

| You want to | Touch |
|---|---|
| A new trading rule | `hyperbot/strategies/`, register it in `__init__.py` |
| A new refusal reason | `RejectCode` (in severity order) + `risk_gate` + a test |
| Another exchange | `hyperbot/brokers/`, implement the `Broker` protocol |
| OHLCV in strategies | Widen `MarketView` to carry `Candle` objects |
| A backtest | A new runner calling the existing `risk_gate` |

Nothing on that list requires changing `risk.py` except the second one.
