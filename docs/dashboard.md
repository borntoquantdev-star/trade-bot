# The dashboard

A pannable, zoomable canvas showing what the bot is doing right now: the
market it sees, the signal the strategy produced, whether the risk gate
approved it, and what reached the exchange — as cards wired together, updating
the moment the bot records a new decision.

Cards can be dragged and their positions stick; a minimap and zoom bar sit in
the bottom right, and a labelled group frame wraps the five pipeline stages.

```bash
hyperbot --dry-run --paper-walk --iterations 60     # window 1: the bot
node dashboard/server.js --open                     # window 2: the dashboard
```

Thai usage guide: [dashboard/README.md](../dashboard/README.md).

## Why it reads files instead of talking to the bot

```
hyperbot ──writes──> logs/audit.jsonl   one JSON line per decision
                     logs/state.json    day's start equity, peak, halt flag
                           │
                           │  server.js tails the files (500ms poll)
                           ▼
                     SSE /api/stream ──> the page draws nodes, chart, log
```

The bot already wrote everything down, because an audit trail was a
requirement before any dashboard existed. So the dashboard needs no hook into
the trading loop, no socket in the engine, no shared memory:

- **The trading loop keeps its timing.** Nothing in `hyperbot` waits on a web
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

## Where the look comes from

Reverse-engineered from **Trade Bot Designer**, a Tauri canvas app for
designing bots by interview. Four things were worth lifting:

| From that app | Used here |
|---|---|
| the camera `(x, y, scale)` as one transform on a layer | pan, wheel zoom, Fit |
| `.node` + `.node-head` cards, absolutely positioned | the eight cards, head colour per state |
| `.group` + `.group-label` | the frame around the five pipeline stages |
| `.board-wires path` | SVG dashed connectors with animated flow |
| `chain(session)` | a pure state-to-nodes function, here over audit rows |
| `spark()` | the price chart: one SVG polyline, no chart library |
| minimap, zoom bar, status strip | the same bottom-corner furniture |

Its tokens came along too (`--accent #5fb4ff`, `--ok #5fd39a`,
`--warn #ffd166`, `--bad #ff8a8a` on a `#0b0b0b` canvas), so the two feel like
one family.

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
- No authentication — which is why it binds to `127.0.0.1` only. Serving it on
  `0.0.0.0` exposes your equity and positions to the network.
