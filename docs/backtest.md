# Backtesting

Two commands. The first downloads price history once; the second replays it
through the same `risk_gate()` the live bot calls.

```bash
tradebot --fetch-candles --days 365      # writes data/candles/...
tradebot --backtest --days 365 --trades  # reads it back, offline
```

`--backtest` never opens a connection. It needs no credentials and no SDK.

## Where the bars live

```
data/candles/<exchange>/<symbol>/<interval>.csv
```

Plain CSV with `open_time,open,high,low,close,volume`, sorted oldest first,
one row per bar, prices written as text so a Decimal survives the round trip.
You can open it in a spreadsheet, and so can your future self debugging a
result that looks too good.

The store is **idempotent**: downloading the same period twice leaves the file
byte for byte identical. A download that dies halfway has already written the
pages that arrived, so running it again resumes instead of starting over.

```bash
tradebot --fetch-candles --since 2024-01-01   # a date instead of a day count
tradebot --fetch-candles --refresh            # re-ask for bars already stored
```

The bar that is still forming is always dropped. Its high, low and close are
whatever the market has done so far this hour, and a strategy that sees one in
a backtest is reading a bar that did not exist yet.

## What the replay does on each bar

1. An order approved on the **previous** bar meets this one.
2. A position that is open may hit its stop or its target.
3. The account is marked to market and the equity curve gets a point.
4. The strategy sees the bar close and may produce a `Signal`.
5. `risk_gate()` sizes it, or refuses it. Same function, same config file,
   same thirteen refusal reasons as the live bot.

Step 5 is the whole point. Nothing in `tradebot/backtest/` decides whether an
order is allowed or how big it is.

## The fill model, and why it is pessimistic

| Situation | What the replay does |
|---|---|
| Signal on a bar | Fills on the **next** bar, never the one it was made on |
| Bar never trades through the limit | No fill. Counted as an unfilled order |
| Market gaps past the limit in your favour | Fills at the open, which is better than the limit |
| Bar contains both the stop and the target | Takes the **stop** |
| Bar opens straight through the stop | Exits at the **open**, not the stop price |
| Position still open at the end | Closed at the last close, marked as such |

The two that matter most: acting one bar late, and taking the stop on an
ambiguous bar. Without tick data there is no way to know which price came
first inside a bar, and assuming the happy one is how a losing strategy
produces a rising equity curve.

Fees are charged on entry and exit at `risk.fee_rate_pct`. **Funding is not
charged at all** - on a perpetual held for days that is real money the report
does not show.

## When the backtest halts

The live engine stops trading and waits for a human when the gate reports
`max_drawdown` or `daily_loss_limit`. The backtest does the same, and says so:

```
HALTED
  at 2026-10-12 05:00 UTC
  daily_loss_limit: today's realised loss ... has reached the daily budget ...
  The live bot would have stopped here and waited for a human.
```

A backtest that carried on would report profits the bot was never going to be
allowed to take. To see the rest of the period anyway, call `run_backtest`
from Python with `halt_on_limits=False`.

## Reading the report

```
RESULT
  starting equity   10,000.00
  final equity      9,801.59
  net profit        -198.41  (-1.98%)
  annualised        -21.64%
  fees paid         12.40
  max drawdown      509.92  (4.95%)
  time underwater   29.5 days  (8483 bars below the previous peak)
  worst month       2026-10  -1.98%

AGAINST DOING NOTHING
  bot               -1.98%
  buy and hold      -51.77%   <- the bot won

TRADES
  closed            2  (0 won, 2 lost)
  per year          24.3
  win rate          0.00%   (average win / average loss: 0.00)
  profit factor     0.00
  expectancy        -1.00R per trade
  sharpe / sortino  -1.51 / -2.06
  longest losing    2 in a row
  time in market    1.99%
  minus best 2      0.00
```

Three of those exist to stop a good-looking result from fooling you:

- **against doing nothing** - if buy and hold beat the bot, the strategy is
  machinery for losing a race.
- **time underwater** - the months you would have spent watching the account
  sit below its high, wondering whether it is broken. Most people quit here,
  and a strategy you quit is a strategy that loses money.
- **minus best 2** - the profit with the two biggest winners removed. If that
  turns negative, the result rests on luck, not on an edge.

`expectancy` is the number to watch: the average result per trade measured in
units of the risk taken. A strategy that wins 40% of the time at +1.5R is a
good strategy, and no win rate on its own tells you that.

If nothing traded, the report says which of the four reasons it was: no
candles, no signals, every signal refused (with the most common reject code),
or approved orders that never filled.

## No trades? Read the reject codes

```
REFUSED BY THE RISK GATE
  invalid_stop         412
```

That is the gate telling you something concrete - here, that the strategy put
a long's stop above its entry price. `tradebot --backtest` surfaces the same
codes `/debug-refusal` explains.

## The JSON

Every run also writes `logs/backtest.json`: the summary, the equity curve and
every round trip, with all numbers as strings so a browser cannot turn a
Decimal into the nearest float it fancies.

```bash
tradebot --backtest --backtest-out logs/experiment-a.json
```

The dashboard reads that file. Start it and switch to the `backtest` tab:

```bash
node dashboard/server.js --open
node dashboard/server.js --backtest-file logs/experiment-a.json   # a saved run
```

The tab shows the equity curve with every stretch below the previous peak
shaded, the result against buy and hold, what the risk gate refused, every
round trip, and the same caveats this page ends with. A year of 5m bars is
115,199 curve points and an 11 MB file; the server thins it to a few hundred
points before sending, keeping the low and the high of every stretch so a
drawdown cannot disappear into a prettier line.

## What a good number does not mean

A backtest is a filter for bad ideas, not evidence for good ones. It cannot
see slippage on a size the book could not absorb, funding, an exchange outage,
or the fact that you fitted the parameters to this exact period. Fewer than
about thirty trades is an anecdote.

The honest sequence is: backtest, then paper, then the testnet, then the
smallest real size you can place - and only then a size that would hurt.
