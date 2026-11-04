# Coming from grvt_demo_bot

This project started as
[grvt_demo_bot](https://github.com/borntoquantdev-star/grvt_demo_bot), a demo
bot for GRVT. The core was rewritten, Hyperliquid support was added, and GRVT
came back as a first-class broker - so it now trades either venue from one
config line. If you have code or notes from that repository, this is what
changed.

## Why it was rewritten rather than patched

`grvt_demo_bot` 1.0.0 could not be installed or tested from a clean clone:

- `grvt_bot/core/executor.py` imported `pysdk` at module level, and `pysdk/`
  was never committed - `requirements.txt` claimed it was "included locally in
  the project". Every fresh clone failed at import, including the test suite.
- `tests/` imported modules that had been moved to `legacy/`, so `pytest`
  failed during collection. `pyproject.toml` also forced `--cov`, which is not
  installed by `requirements.txt`, so a bare `pytest` errored before reaching
  the real problem.
- The integration tests it shipped placed four real orders with no check that
  the configured environment was a testnet.

And three bugs with teeth, each of which now has a regression test:

- `ConfigManager` copied its class-level defaults shallowly, so loading one
  config wrote credentials into the shared dictionary and the next
  `ConfigManager()` inherited them.
- `__repr__` masked a shallow copy, overwriting the live credentials with the
  string `***MASKED***`. Printing a config destroyed it.
- The loop rounded quantity with `round(500 / price, 3)` unconditionally,
  which becomes `0.0` at a high enough price, and nothing checked for zero.

There was also no risk management of any kind: no stop loss, no daily loss
limit, no position cap, no check for an existing position. A random strategy
could stack 500 USDT at 10x every 60 seconds without limit.

## The exchange

| | grvt_demo_bot | tradebot |
|---|---|---|
| Exchange | GRVT only | GRVT **or** Hyperliquid, chosen in config |
| Symbols | `BTC_USDT_Perp` | same on GRVT; bare coins on Hyperliquid |
| SDK | `grvt-pysdk` (vendored, missing) | `grvt-pysdk` or `hyperliquid-python-sdk`, optional extras |
| Credentials | API key + private key + account id | master `account_address` + `api_wallet_key` |
| Minimum order | ~500 USDT | $10 |
| Price rule | fixed tick | 5 significant figures **and** `6 - szDecimals` decimals |
| Candles | none | `candles_snapshot`, used to warm up |
| Client order id | not used | deterministic, folded per venue (uint32 / 128-bit hex) |
| Leverage | web interface only | settable through the API, but the bot does not |

The two Hyperliquid identities are easy to confuse and the config refuses to
let you: `account_address` is the master account, used for every query, and
the API docs note that querying with the agent address returns an empty state
with no error. `api_wallet_key` only signs.

## Renames

| grvt_demo_bot | tradebot |
|---|---|
| package `grvt_bot` | package `tradebot` |
| command `grvt-bot` | command `tradebot` |
| `grvt_bot.core.executor.GRVTExecutor` | `tradebot.brokers.hyperliquid.HyperliquidBroker` |
| `ConfigManager(config_path=...)` | `load_config(path)` -> frozen `BotConfig` |
| `config.SYMBOL`, `config.LEVERAGE` | `config.trading.symbol`, `config.risk.max_leverage` |
| `config.GRVT_API_KEY` (str) | `config.hyperliquid.api_wallet_key` (`Secret`, call `.reveal()`) |
| `BaseStrategy` | `Strategy` (in `tradebot.strategies.base`) |
| `get_signal() -> dict` | `get_signal(market: MarketView) -> Signal \| None` |
| - | `risk_gate()`, which did not exist |
| `config.yaml: grvt:` | `config.yaml: hyperliquid:` |

`legacy/`, `scripts/check_leverage.py`, `setup.py` and six overlapping
markdown files are gone. `tradebot --doctor` replaces the leverage script.

## Code changes

### Configuration

```python
# before
from grvt_bot.core.config import ConfigManager
config = ConfigManager(config_path="config/config.yaml")
symbol = config.SYMBOL

# now
from tradebot.core.config import load_config
config = load_config("config/config.yaml")
symbol = config.trading.symbol
key = config.hyperliquid.api_wallet_key.reveal()   # explicit, only where used
```

`BotConfig` is frozen and validated on load: a bad network, a zero
`loop_interval`, an unknown timeframe or an address in the key field raises
`ConfigError` immediately instead of failing at the first order.

### Strategies

A signal is now a typed object and **must carry a stop price**, because the
stop is what sizes the position.

```python
# before
def get_signal(self):
    return {"side": "buy", "amount_usdt": 500, "confidence": 0.85}

# now
def get_signal(self, market):
    return Signal.build(
        symbol=market.symbol,
        side="buy",
        ref_price=market.price,
        stop_price=market.price * D("0.99"),
        reason="MA crossover",
        created_at=market.now,
    )
```

`amount_usdt` is gone: strategies no longer choose size. So is `confidence` -
it had no effect on anything, and implying that it might was the problem.

Strategies also receive history now: `market.recent_prices` is primed from
real candles at start-up, so an indicator does not have to wait for
iterations.

### Placing orders

```python
# before - size decided by the caller, leverage argument silently ignored
price = executor.get_market_price(symbol)
executor.place_market_order(symbol, "buy", amount=500 / price, leverage=10)

# now - the gate decides size, the broker only sends
decision = risk_gate(signal, state, config.risk, spec, mark_price, now)
if isinstance(decision, Reject):
    log.info("refused: %s", decision)
else:
    broker.place_order(decision)
```

Entries are GTC limit orders at the slippage-capped price the gate computed,
carrying our deterministic cloid. Exits cross the book with a reduce-only IOC
order, because a protective price that never fills is the worst outcome when
you want out.

## Installing and running

```bash
# before
pip install -r requirements.txt     # ImportError: no module named 'pysdk'
python main.py

# now
pip install -e ".[dev]"             # tests and paper trading: no SDK needed
pip install -e ".[hyperliquid]"     # adds the SDK, for live orders
tradebot --dry-run                  # offline simulator, no credentials
tradebot --doctor                   # check config and connection
tradebot --live                     # required for real orders
```

## Tests

```bash
pytest                    # before: collection error.  now: 236 offline tests
pytest --run-integration  # the only way to reach a real exchange
```

Integration tests refuse to run against mainnet, skip without credentials, and
cancel what they place.

## Antigravity workflows

`.agent/workflows/` and `ANTIGRAVITY_GUIDE.md` are replaced by `ai/workflows/`
plus generated adapters for Codex (`AGENTS.md`), Claude Code (`CLAUDE.md`,
`.claude/`) and Copilot (`.github/`). See [vscode.md](vscode.md). The strategy
builder interview survives, rewritten against the new API.
