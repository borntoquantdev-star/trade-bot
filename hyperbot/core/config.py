"""Configuration: YAML file, then environment variables, then validation.

Three bugs from the project this grew out of are fixed here, and each one has
a regression test in ``tests/test_config.py``:

1. The defaults were copied shallowly, so merging a file into one config
   object rewrote the class-level defaults and leaked credentials into every
   later object. Defaults are deep-copied.
2. ``__repr__`` masked a shallow copy, which overwrote the live credentials
   with the literal string ``***MASKED***``. Printing a config destroyed it.
   Secrets are wrapped in :class:`Secret`, so masking is structural and no
   copy has to be remembered.
3. Percentages were read as whole numbers in some places and fractions in
   others. The YAML stays human-friendly (``1.0`` means 1%) and this module
   is the single point of conversion.

Hyperliquid splits the account into two values that are easy to confuse, so
they are separate settings here:

* ``account_address`` - the **master** account. Positions and equity are
  looked up by this address. The API docs call out querying with the agent
  address instead as a common mistake that silently returns nothing.
* ``api_wallet_key`` - the private key of an **API wallet** approved by that
  master account. It only signs.
"""

from __future__ import annotations

import copy
import os
import re
from collections.abc import Mapping, MutableMapping
from dataclasses import dataclass, field
from decimal import Decimal
from pathlib import Path
from typing import Any

import yaml

from ..brokers.hyperliquid import INTERVALS
from ..money import D, pct
from ..types import InstrumentSpec, RiskLimits

_ENV_PATTERN = re.compile(r"^\$\{([A-Za-z_][A-Za-z0-9_]*)\}$")

VALID_NETWORKS = ("testnet", "mainnet")

DEFAULTS: dict[str, Any] = {
    "hyperliquid": {
        "network": "testnet",
        # The MASTER account address, used for every account query.
        "account_address": "",
        # An API wallet private key. Signs only; never queried.
        "api_wallet_key": "",
        # Optional: trade on behalf of a vault.
        "vault_address": "",
    },
    "trading": {
        # Hyperliquid perps are named by coin: BTC, ETH, SOL...
        "symbol": "BTC",
        # Candle interval used to warm the strategy up at start-up.
        "timeframe": "5m",
        "loop_interval": 60,
        "strategy": "random",
        "warmup_candles": 200,
    },
    "risk": {
        # Percentages, as a human would write them.
        "risk_per_trade_pct": 1.0,
        "max_positions": 1,
        "max_daily_loss_pct": 3.0,
        "max_drawdown_pct": 10.0,
        "max_leverage": 5,
        "max_slippage_pct": 0.1,
        "fee_rate_pct": 0.045,
        "allow_short": True,
        "starting_equity_usdt": 10000,
    },
    "instrument": {
        # Fallbacks for paper trading. A live session reads the real values
        # from the exchange's perp metadata instead.
        # BTC has szDecimals 5, so the lot step is 1e-5 and prices may carry
        # at most 6 - 5 = 1 decimal place.
        "tick_size": 0.1,
        "lot_step": 0.00001,
        "min_notional_usdt": 10,
        "max_leverage": 40,
        "price_sig_figs": 5,
    },
}

ENV_OVERRIDES: dict[str, tuple[str, str]] = {
    "HYPERLIQUID_NETWORK": ("hyperliquid", "network"),
    "HYPERLIQUID_ACCOUNT_ADDRESS": ("hyperliquid", "account_address"),
    "HYPERLIQUID_API_WALLET_KEY": ("hyperliquid", "api_wallet_key"),
    "HYPERLIQUID_VAULT_ADDRESS": ("hyperliquid", "vault_address"),
    "HYPERBOT_SYMBOL": ("trading", "symbol"),
    "HYPERBOT_TIMEFRAME": ("trading", "timeframe"),
    "HYPERBOT_LOOP_INTERVAL": ("trading", "loop_interval"),
    "HYPERBOT_STRATEGY": ("trading", "strategy"),
    "HYPERBOT_RISK_PER_TRADE_PCT": ("risk", "risk_per_trade_pct"),
    "HYPERBOT_MAX_POSITIONS": ("risk", "max_positions"),
    "HYPERBOT_MAX_DAILY_LOSS_PCT": ("risk", "max_daily_loss_pct"),
    "HYPERBOT_MAX_DRAWDOWN_PCT": ("risk", "max_drawdown_pct"),
    "HYPERBOT_MAX_LEVERAGE": ("risk", "max_leverage"),
}


class ConfigError(ValueError):
    """The configuration cannot be used as written."""


class Secret:
    """A string that refuses to appear in logs.

    ``repr`` and ``str`` both return ``***``, so a stray ``print(config)``,
    f-string or traceback cannot leak a key. Call :meth:`reveal` at the single
    point where the value is actually needed.
    """

    __slots__ = ("_value",)

    def __init__(self, value: str = "") -> None:
        self._value = str(value or "")

    def reveal(self) -> str:
        return self._value

    def __bool__(self) -> bool:
        return bool(self._value)

    def __len__(self) -> int:
        return len(self._value)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, Secret):
            return self._value == other._value
        return NotImplemented

    def __hash__(self) -> int:
        return hash(("Secret", self._value))

    def __repr__(self) -> str:
        return "Secret(***)" if self._value else "Secret(empty)"

    __str__ = __repr__


@dataclass(frozen=True)
class HyperliquidSettings:
    """Exchange connection settings."""

    network: str = "testnet"
    account_address: Secret = field(default_factory=Secret)
    api_wallet_key: Secret = field(default_factory=Secret)
    vault_address: Secret = field(default_factory=Secret)

    @property
    def is_live(self) -> bool:
        """True on mainnet, where orders cost real money."""
        return self.network == "mainnet"


@dataclass(frozen=True)
class TradingSettings:
    """What to trade and how often to look."""

    symbol: str = "BTC"
    timeframe: str = "5m"
    loop_interval: int = 60
    strategy: str = "random"
    warmup_candles: int = 200


@dataclass(frozen=True)
class BotConfig:
    """The whole configuration, parsed and validated."""

    hyperliquid: HyperliquidSettings
    trading: TradingSettings
    risk: RiskLimits
    instrument: InstrumentSpec
    starting_equity: Decimal
    source: str = "<defaults>"

    def require_live_credentials(self) -> None:
        """Raise unless everything needed to trade for real is present.

        Paper trading deliberately does not call this: ``--dry-run`` must work
        with no credentials at all.
        """
        missing = [
            name
            for name in ("account_address", "api_wallet_key")
            if not getattr(self.hyperliquid, name)
        ]
        if missing:
            raise ConfigError(
                "live trading needs hyperliquid."
                + ", hyperliquid.".join(missing)
                + f" - set them in {self.source} or as environment variables. "
                "account_address is the MASTER account; api_wallet_key is the "
                "private key of an API wallet it has approved."
            )


# --- loading ---------------------------------------------------------------


def _expand_env(value: Any) -> Any:
    """Replace a whole-string ``${VAR}`` with its environment value."""
    if isinstance(value, str):
        match = _ENV_PATTERN.match(value.strip())
        if match:
            name = match.group(1)
            if name not in os.environ:
                raise ConfigError(
                    f"the config refers to ${{{name}}} but that environment variable is not set"
                )
            return os.environ[name]
        return value
    if isinstance(value, Mapping):
        return {k: _expand_env(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_expand_env(v) for v in value]
    return value


def _deep_merge(base: MutableMapping[str, Any], incoming: Mapping[str, Any]) -> None:
    """Merge *incoming* into *base* in place, recursing into nested mappings."""
    for key, value in incoming.items():
        if isinstance(value, Mapping) and isinstance(base.get(key), MutableMapping):
            _deep_merge(base[key], value)
        else:
            base[key] = value


def _as_int(value: Any, what: str) -> int:
    try:
        return int(value)
    except (TypeError, ValueError) as exc:
        raise ConfigError(f"{what} must be a whole number, got {value!r}") from exc


def _as_decimal(value: Any, what: str) -> Decimal:
    try:
        return D(value)
    except ValueError as exc:
        raise ConfigError(f"{what} must be a number, got {value!r}") from exc


def raw_defaults() -> dict[str, Any]:
    """A fresh, deeply copied copy of the default settings.

    Deep, so that nothing a caller does can reach back and modify the module
    level ``DEFAULTS`` - the bug that used to leak credentials between config
    objects.
    """
    return copy.deepcopy(DEFAULTS)


def load_config(
    path: str | Path | None = None,
    data: Mapping[str, Any] | None = None,
    env: Mapping[str, str] | None = None,
) -> BotConfig:
    """Build a :class:`BotConfig` from a YAML file and/or a mapping.

    Precedence, lowest first: built-in defaults, the YAML file, *data*, then
    environment variables.
    """
    merged = raw_defaults()
    source = "<defaults>"

    if path is not None:
        file_path = Path(path)
        if not file_path.exists():
            raise ConfigError(
                f"config file not found: {file_path}\n"
                "Copy config/config.example.yaml to config/config.yaml and fill it in."
            )
        with file_path.open("r", encoding="utf-8") as handle:
            loaded = yaml.safe_load(handle) or {}
        if not isinstance(loaded, Mapping):
            raise ConfigError(f"{file_path} must contain a YAML mapping at the top level")
        _deep_merge(merged, _expand_env(loaded))
        source = str(file_path)

    if data:
        _deep_merge(merged, _expand_env(data))
        if source == "<defaults>":
            source = "<dict>"

    environ = os.environ if env is None else env
    for var, (section, key) in ENV_OVERRIDES.items():
        if environ.get(var) not in (None, ""):
            merged[section][key] = environ[var]

    return _build(merged, source)


def _build(raw: Mapping[str, Any], source: str) -> BotConfig:
    hl_raw = raw.get("hyperliquid", {})
    network = str(hl_raw.get("network", "testnet")).strip().lower()
    if network not in VALID_NETWORKS:
        raise ConfigError(f"hyperliquid.network must be one of {VALID_NETWORKS}, got {network!r}")

    address = str(hl_raw.get("account_address", "") or "")
    if address and not re.fullmatch(r"0x[0-9a-fA-F]{40}", address):
        raise ConfigError(
            "hyperliquid.account_address must be a 42-character 0x address "
            f"(got {len(address)} characters). This is the master account "
            "address, not a private key."
        )

    hyperliquid = HyperliquidSettings(
        network=network,
        account_address=Secret(address),
        api_wallet_key=Secret(hl_raw.get("api_wallet_key", "")),
        vault_address=Secret(hl_raw.get("vault_address", "")),
    )

    trading_raw = raw.get("trading", {})
    loop_interval = _as_int(trading_raw.get("loop_interval", 60), "trading.loop_interval")
    if loop_interval < 1:
        raise ConfigError("trading.loop_interval must be at least 1 second")
    timeframe = str(trading_raw.get("timeframe", "5m"))
    if timeframe not in INTERVALS:
        raise ConfigError(
            f"trading.timeframe must be one of {', '.join(INTERVALS)}, got {timeframe!r}"
        )
    warmup = _as_int(trading_raw.get("warmup_candles", 200), "trading.warmup_candles")
    if warmup < 0:
        raise ConfigError("trading.warmup_candles cannot be negative")
    trading = TradingSettings(
        symbol=str(trading_raw.get("symbol", "BTC")).strip(),
        timeframe=timeframe,
        loop_interval=loop_interval,
        strategy=str(trading_raw.get("strategy", "random")),
        warmup_candles=warmup,
    )
    if not trading.symbol:
        raise ConfigError("trading.symbol cannot be empty")

    risk_raw = raw.get("risk", {})
    try:
        risk = RiskLimits(
            risk_per_trade=pct(
                _as_decimal(risk_raw.get("risk_per_trade_pct", 1), "risk.risk_per_trade_pct")
            ),
            max_positions=_as_int(risk_raw.get("max_positions", 1), "risk.max_positions"),
            max_daily_loss=pct(
                _as_decimal(risk_raw.get("max_daily_loss_pct", 3), "risk.max_daily_loss_pct")
            ),
            max_drawdown=pct(
                _as_decimal(risk_raw.get("max_drawdown_pct", 10), "risk.max_drawdown_pct")
            ),
            max_leverage=_as_decimal(risk_raw.get("max_leverage", 5), "risk.max_leverage"),
            max_slippage=pct(
                _as_decimal(risk_raw.get("max_slippage_pct", 0.1), "risk.max_slippage_pct")
            ),
            fee_rate=pct(_as_decimal(risk_raw.get("fee_rate_pct", 0.045), "risk.fee_rate_pct")),
            allow_short=bool(risk_raw.get("allow_short", True)),
        )
    except ValueError as exc:
        raise ConfigError(f"risk settings are not usable: {exc}") from exc

    inst_raw = raw.get("instrument", {})
    try:
        instrument = InstrumentSpec.build(
            trading.symbol,
            tick_size=_as_decimal(inst_raw.get("tick_size", "0.1"), "instrument.tick_size"),
            lot_step=_as_decimal(inst_raw.get("lot_step", "0.00001"), "instrument.lot_step"),
            min_notional=_as_decimal(
                inst_raw.get("min_notional_usdt", 10), "instrument.min_notional_usdt"
            ),
            max_leverage=_as_decimal(inst_raw.get("max_leverage", 40), "instrument.max_leverage"),
            price_sig_figs=_as_int(inst_raw.get("price_sig_figs", 5), "instrument.price_sig_figs"),
        )
    except ValueError as exc:
        raise ConfigError(f"instrument settings are not usable: {exc}") from exc

    starting_equity = _as_decimal(
        risk_raw.get("starting_equity_usdt", 10000), "risk.starting_equity_usdt"
    )
    if starting_equity <= 0:
        raise ConfigError("risk.starting_equity_usdt must be positive")

    return BotConfig(
        hyperliquid=hyperliquid,
        trading=trading,
        risk=risk,
        instrument=instrument,
        starting_equity=starting_equity,
        source=source,
    )


def describe(config: BotConfig) -> str:
    """A multi-line summary safe to print or log (secrets stay masked)."""
    address = config.hyperliquid.account_address
    shown = address.reveal()[:6] + "..." + address.reveal()[-4:] if address else "not set"
    return "\n".join(
        [
            f"  source            {config.source}",
            f"  network           {config.hyperliquid.network}"
            + ("  *** REAL MONEY ***" if config.hyperliquid.is_live else ""),
            f"  account           {shown}",
            f"  api wallet key    {config.hyperliquid.api_wallet_key}",
            f"  symbol            {config.trading.symbol} ({config.trading.timeframe} candles)",
            f"  strategy          {config.trading.strategy}",
            f"  loop interval     {config.trading.loop_interval}s",
            f"  warm-up candles   {config.trading.warmup_candles}",
            f"  risk per trade    {config.risk.risk_per_trade * 100}%",
            f"  max positions     {config.risk.max_positions}",
            f"  daily loss stop   {config.risk.max_daily_loss * 100}%",
            f"  drawdown stop     {config.risk.max_drawdown * 100}%",
            f"  exposure cap      {config.risk.max_leverage}x equity",
            f"  slippage cap      {config.risk.max_slippage * 100}%",
            f"  short selling     {'allowed' if config.risk.allow_short else 'blocked'}",
            f"  tick / lot / min  {config.instrument.tick_size} / "
            f"{config.instrument.lot_step} / {config.instrument.min_notional}",
        ]
    )


__all__ = [
    "BotConfig",
    "ConfigError",
    "HyperliquidSettings",
    "Secret",
    "TradingSettings",
    "describe",
    "load_config",
    "raw_defaults",
]
