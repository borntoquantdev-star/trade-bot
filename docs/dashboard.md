# The dashboard

A pannable, zoomable canvas with two tabs over the same machinery:

- **live** — what the bot is doing right now: the market it sees, the signal
  the strategy produced, whether the risk gate approved it, and what reached
  the exchange, updating the moment the bot records a new decision.
- **backtest** — what a replay produced: the equity curve, the result against
  buy and hold, what the gate refused, and every round trip.

Both are cards wired together on one canvas; each tab remembers its own card
layout and camera.

Cards can be dragged and their positions stick; a minimap and zoom bar sit in
the bottom right, and a labelled group frame wraps the five pipeline stages.

```bash
tradebot --dry-run --paper-walk --iterations 60     # window 1: the bot
node dashboard/server.js --open                     # window 2: the dashboard
```

The backtest tab needs `logs/backtest.json`, which `tradebot --backtest`
writes. Point elsewhere with `--backtest-file`.

Thai usage guide: [dashboard/README.md](../dashboard/README.md).

## Why it reads files instead of talking to the bot

```
tradebot ──writes──> logs/audit.jsonl   one JSON line per decision
                     logs/state.json    day's start equity, peak, halt flag
                     logs/backtest.json one replay's result
                           │
                           │  server.js tails the files (500ms poll)
                           ▼
                     SSE /api/stream ──> the page draws nodes, chart, log
                       events: snapshot (live), backtest (replay)
```

The bot already wrote everything down, because an audit trail was a
requirement before any dashboard existed. So the dashboard needs no hook into
the trading loop, no socket in the engine, no shared memory:

- **The trading loop keeps its timing.** Nothing in `tradebot` waits on a web
  server, serialises for a browser, or blocks on a slow client.
- **The dashboard cannot affect trading.** It is read-only by construction,
  not by discipline. There is no code path from the page to an order.
- **Either side can die alone.** Kill the dashboard and the bot does not
  notice; restart the bot and the dashboard picks the new lines up.

This is the same reasoning that keeps `risk_gate` pure: the part that must not
break gets no extra responsibilities.

## The five stages

`lib/pipeline.js` turns audit rows into node states. It is a pure function —
rows in, view out — so every state below is unit tested rather than eyeballed.

| Stage | `ready` when | `blocked` / `error` when |
|---|---|---|
| Market data | price and equity read | `broker read failed` |
| Strategy | a signal this iteration | the strategy raised |
| Risk gate | approved a `SizedOrder` | refused (shows the reject code), or the bot halted |
| Broker | the exchange accepted it | the broker refused; `locked` (blue dashed) for a paper fill |
| Position | something is open | — |

All thirteen reject codes have a plain-language explanation, and a test asserts
none is missing — add a code to `RejectCode` without a translation and the
dashboard tests fail.

## The backtest tab

`lib/backtest.js` is the second pure function, the same shape as the first:
`logs/backtest.json` in, a view out, loaded by both the server and the browser.

Two things it exists to do. Every number in that file is a **string**, because
`report.to_dict` writes Decimals whole; this is the one place they become
numbers, and a `null` meaning "could not be measured" must not arrive as a `0`
meaning "measured zero". And the equity curve carries one point per bar, which
for a year of 5m candles is 115,199 points in an 11 MB file: it is thinned to
a few hundred points **on the server**, keeping each stretch's lowest and
highest point, so the wire stays small and no drawdown is smoothed away.

Judgements stay on the Python side and travel as codes. `no_trade_reason`
says which dead end a run hit, and `thin_sample_below` carries the threshold
under which a sample is an anecdote; the console words them in English and the
dashboard in Thai, from one ladder of conditions. Reject codes reuse the same
translation table as the live tab.

## Where the look comes from

The mechanics were reverse-engineered from **Trade Bot Designer**, a Tauri
canvas app for designing bots by interview. Four things were worth lifting:

| From that app | Used here |
|---|---|
| the camera `(x, y, scale)` as one transform on a layer | pan, wheel zoom, Fit |
| `.node` + `.node-head` cards, absolutely positioned | the eight cards, head colour per state |
| `.group` + `.group-label` | the frame around the five pipeline stages |
| `.board-wires path` | SVG dashed connectors with animated flow |
| `chain(session)` | a pure state-to-nodes function, here over audit rows |
| `spark()` | the price chart: one SVG polyline, no chart library |
| minimap, zoom bar, status strip | the same bottom-corner furniture |

## The visual language

The surface is styled after **Symvibe's board**: a warm near-black canvas with
a dotted grid, chrome that floats as rounded islands rather than solid bars
pinned to the edges, hairline borders at a tenth of an alpha, and monospace for
anything a person reads as a value - numbers, times, ids, commands.

| token | |
|---|---|
| `--canvas #14110e`, `--surface #1a1612` | warm, not blue-black |
| `--text #f1e9dd`, `--muted #cdc2b3`, `--dim #8b7e72` | cream, not blue-white |
| `--accent #ff6b3d` | orange |
| `--ok #5fbe86`, `--warn #e8a33d`, `--bad #f0564d` | status |

**Orange means "system", never "money".** The selected tab, the held tool, a
wire with data moving through it. Status is green, amber and red on their own
scale. Mixing them would put the colour for "this is selected" next to the
colour for "this lost money", which is the one confusion a trading dashboard
cannot afford - and it is why the live price chart is drawn in plain cream: a
price is not a profit.

The chrome floats because that is the whole of the phrase *infinite board* -
the canvas runs under everything and has no edge to find. Gaps between the
islands pass pointer events through, so you can drag the board from anywhere
the islands are not.

No webfont is loaded. The page binds to `127.0.0.1` and has to open with the
network down, so it uses the system sans for Thai prose and the system
monospace for values.

What that app did **not** have is live data — it is a simulator. Here the same
shapes are driven by a bot that really ran.

## No dependencies, no build

`server.js` uses only Node's standard library. The page is three files served
as-is: no bundler, no framework, no `npm install`. `lib/pipeline.js` is loaded
by both the server (CommonJS) and the browser (a global), so the two can never
disagree about what a row means.

That is a deliberate trade: a React dashboard would be prettier, and would
also mean a `node_modules` tree, a build step and a second thing to keep
upgraded inside a repository whose point is a trustworthy risk gate.

## Limits

- It shows the **past**: every pixel is something already recorded.
- It can lag the bot by one poll interval (500ms default).
- It keeps the last 2,000 rows in memory; older decisions stay in the file.
- The chart is the price **the bot saw when deciding**, not an OHLC chart.
- The backtest tab shows one saved file. It does not run anything, compare two
  runs, or keep a history of them.
- No authentication — which is why it binds to `127.0.0.1` only. Serving it on
  `0.0.0.0` exposes your equity and positions to the network.
