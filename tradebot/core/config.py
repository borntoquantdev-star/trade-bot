"""Configuration: YAML file, then environment variables, then validation.

One bot, two exchanges. ``exchange:`` picks which broker runs; each venue keeps
its own credential block, because their shapes have nothing in common:

* **GRVT** - API key + private key + trading account id, symbols like
  ``BTC_USDT_Perp``, integer client order ids, a published ``min_size``.
* **Hyperliquid** - a master account address plus an API wallet key that only
  signs, symbols like ``BTC``, 128-bit hex client order ids, prices capped at
  five significant figures.

Instrument defaults follow the chosen exchange, so a config that only says
``exchange: grvt`` still has sensible tick and lot sizes to paper trade with.

Three bugs from the project this grew out of are fixed here, each with a
regression test in ``tests/test_config.py``:

1. The defaults were copied shallowly, so merging a file into one config
   object rewrote the class-level defaults and leaked credentials into every
   later object. Defaults are deep-copied.
2. ``__repr__`` masked a shallow copy, which overwrote the live credentials
   with the literal string ``***MASKED***``. Secrets are wrapped in
   :class:`Secret`, so masking is structural.
3. Percentages were read as whole numbers in some places and fractions in
   others. The YAML stays human-friendly (``1.0`` means 1%) and this module is
   the single point of conversion.
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

from ..money import D, pct
from ..types import InstrumentSpec, RiskLimits

_ENV_PATTERN = re.compile(r"^\$\{([A-Za-z_][A-Za-z0-9_]*)\}$")

EXCHANGES = ("grvt", "hyperliquid")
GRVT_ENVS = ("testnet", "prod")
HYPERLIQUID_NETWORKS = ("testnet", "mainnet")

#: Candle intervals both venues accept.
INTERVALS = ("1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1d", "3d", "1w")

#: What each venue looks like when nothing is configured. Paper trading uses
#: these; a live session replaces them with the exchange's own metadata.
EXCHANGE_DEFAULTS: dict[str, dict[str, Any]] = {
    "grvt": {
        "symbol": "BTC_USDT_Perp",
        "instrument": {
            "tick_size": 0.1,
            "lot_step": 0.001,
            "min_notional_usdt": 500,
            "min_qty": 0.001,
            "max_leverage": 20,
            # GRVT uses a plain tick; no significant-figure rule.
            "price_sig_figs": 0,
        },
    },
    "hyperliquid": {
        "symbol": "BTC",
        "instrument": {
            # BTC has szDecimals 5: lot step 1e-5, at most 6 - 5 = 1 price decimal.
            "tick_size": 0.1,
            "lot_step": 0.00001,
            "min_notional_usdt": 10,
            "min_qty": 0,
            "max_leverage": 40,
            "price_sig_figs": 5,
        },
    },
}

DEFAULTS: dict[str, Any] = {
    "exchange": "grvt",
    "grvt": {
        "env": "testnet",
        "api_key": "",
        "private_key": "",
        "trading_account_id": "",
        "sub_account_id": "0",
    },
    "hyperliquid": {
        "network": "testnet",
        # The MASTER account address, used for every account query.
        "account_address": "",
        # An API wallet private key. Signs only; never queried.
        "api_wallet_key": "",
        "vault_address": "",
    },
    "trading": {
        # Empty means "use the chosen exchange's default symbol".
        "symbol": "",
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
        "fee_rate_pct": 0.05,
        "allow_short": True,
        "starting_equity_usdt": 10000,
    },
    # Empty means "inherit from the chosen exchange".
    "instrument": {},
}

ENV_OVERRIDES: dict[str, tuple[str, str]] = {
    "TRADEBOT_EXCHANGE": ("", "exchange"),
    "GRVT_ENV": ("grvt", "env"),
    "GRVT_API_KEY": ("grvt", "api_key"),
    "GRVT_PRIVATE_KEY": ("grvt", "private_key"),
    "GRVT_TRADING_ACCOUNT_ID": ("grvt", "trading_account_id"),
    "GRVT_SUB_ACCOUNT_ID": ("grvt", "sub_account_id"),
    "HYPERLIQUID_NETWORK": ("hyperliquid", "network"),
    "HYPERLIQUID_ACCOUNT_ADDRESS": ("hyperliquid", "account_address"),
    "HYPERLIQUID_API_WALLET_KEY": ("hyperliquid", "api_wallet_key"),
    "HYPERLIQUID_VAULT_ADDRESS": ("hyperliquid", "vault_address"),
    "TRADEBOT_SYMBOL": ("trading", "symbol"),
    "TRADEBOT_TIMEFRAME": ("trading", "timeframe"),
    "TRADEBOT_LOOP_INTERVAL": ("trading", "loop_interval"),
    "TRADEBOT_STRATEGY": ("trading", "strategy"),
    "TRADEBOT_RISK_PER_TRADE_PCT": ("risk", "risk_per_trade_pct"),
    "TRADEBOT_MAX_POSITIONS": ("risk", "max_positions"),
    "TRADEBOT_MAX_DAILY_LOSS_PCT": ("risk", "max_daily_loss_pct"),
    "TRADEBOT_MAX_DRAWDOWN_PCT": ("risk", "max_drawdown_pct"),
    "TRADEBOT_MAX_LEVERAGE": ("risk", "max_leverage"),
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
class GrvtSettings:
    """GRVT connection settings."""

    env: str = "testnet"
    api_key: Secret = field(default_factory=Secret)
    private_key: Secret = field(default_factory=Secret)
    trading_account_id: Secret = field(default_factory=Secret)
    sub_account_id: str = "0"

    @property
    def is_live(self) -> bool:
        return self.env == "prod"


@dataclass(frozen=True)
class HyperliquidSettings:
    """Hyperliquid connection settings."""

    network: str = "testnet"
    account_address: Secret = field(default_factory=Secret)
    api_wallet_key: Secret = field(default_factory=Secret)
    vault_address: Secret = field(default_factory=Secret)

    @property
    def is_live(self) -> bool:
        return self.network == "mainnet"


@dataclass(frozen=True)
class TradingSettings:
    """What to trade and how often to look."""

    symbol: str = "BTC_USDT_Perp"
    timeframe: str = "5m"
    loop_interval: int = 60
    strategy: str = "random"
    warmup_candles: int = 200


@dataclass(frozen=True)
class BotConfig:
    """The whole configuration, parsed and validated."""

    exchange: str
    grvt: GrvtSettings
    hyperliquid: HyperliquidSettings
    trading: TradingSettings
    risk: RiskLimits
    instrument: InstrumentSpec
    starting_equity: Decimal
    source: str = "<defaults>"

    @property
    def venue(self) -> GrvtSettings | HyperliquidSettings:
        """The settings block for the exchange actually in use."""
        return self.grvt if self.exchange == "grvt" else self.hyperliquid

    @property
    def venue_name(self) -> str:
        """The environment name of the chosen venue, for logs."""
        return self.grvt.env if self.exchange == "grvt" else self.hyperliquid.network

    @property
    def is_live(self) -> bool:
        """True when the chosen venue points at real money."""
        return self.venue.is_live

    def require_live_credentials(self) -> None:
        """Raise unless everything needed to trade for real is present.

        Paper trading deliberately does not call this: ``--dry-run`` must work
        with no credentials at all.
        """
        if self.exchange == "grvt":
            missing = [
                name
                for name in ("api_key", "private_key", "trading_account_id")
                if not getattr(self.grvt, name)
            ]
            if missing:
                raise ConfigError(
                    "live trading on GRVT needs grvt."
                    + ", grvt.".join(missing)
                    + f" - set them in {self.source} or as environment variables"
                )
            return

        missing = [
            name
            for name in ("account_address", "api_wallet_key")
            if not getattr(self.hyperliquid, name)
        ]
        if missing:
            raise ConfigError(
                "live trading on Hyperliquid needs hyperliquid."
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
    # จำไว้ว่าผู้ใช้ระบุ exchange เองหรือไม่ ก่อนที่ค่า default จะกลบ
    exchange_was_set = False

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
        exchange_was_set = exchange_was_set or "exchange" in loaded
        _deep_merge(merged, _expand_env(loaded))
        source = str(file_path)

    if data:
        exchange_was_set = exchange_was_set or "exchange" in data
        _deep_merge(merged, _expand_env(data))
        if source == "<defaults>":
            source = "<dict>"

    environ = os.environ if env is None else env
    for var, (section, key) in ENV_OVERRIDES.items():
        if environ.get(var) in (None, ""):
            continue
        if section:
            merged[section][key] = environ[var]
        else:
            merged[key] = environ[var]
            if key == "exchange":
                exchange_was_set = True

    return _build(merged, source, exchange_was_set)


def _build(raw: Mapping[str, Any], source: str, exchange_was_set: bool = True) -> BotConfig:
    exchange = str(raw.get("exchange", "grvt")).strip().lower()
    if exchange not in EXCHANGES:
        raise ConfigError(f"exchange must be one of {EXCHANGES}, got {exchange!r}")
    venue_defaults = EXCHANGE_DEFAULTS[exchange]

    # --- GRVT ---
    grvt_raw = raw.get("grvt", {})
    grvt_env = str(grvt_raw.get("env", "testnet")).strip().lower()
    if grvt_env not in GRVT_ENVS:
        raise ConfigError(f"grvt.env must be one of {GRVT_ENVS}, got {grvt_env!r}")
    grvt = GrvtSettings(
        env=grvt_env,
        api_key=Secret(grvt_raw.get("api_key", "")),
        private_key=Secret(grvt_raw.get("private_key", "")),
        trading_account_id=Secret(grvt_raw.get("trading_account_id", "")),
        sub_account_id=str(grvt_raw.get("sub_account_id", "0")),
    )

    # --- Hyperliquid ---
    hl_raw = raw.get("hyperliquid", {})
    network = str(hl_raw.get("network", "testnet")).strip().lower()
    if network not in HYPERLIQUID_NETWORKS:
        raise ConfigError(
            f"hyperliquid.network must be one of {HYPERLIQUID_NETWORKS}, got {network!r}"
        )
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

    # --- trading ---
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

    symbol = str(trading_raw.get("symbol", "") or "").strip() or venue_defaults["symbol"]
    _check_symbol_shape(exchange, symbol, source, exchange_was_set)

    trading = TradingSettings(
        symbol=symbol,
        timeframe=timeframe,
        loop_interval=loop_interval,
        strategy=str(trading_raw.get("strategy", "random")),
        warmup_candles=warmup,
    )

    # --- risk ---
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
            fee_rate=pct(_as_decimal(risk_raw.get("fee_rate_pct", 0.05), "risk.fee_rate_pct")),
            allow_short=bool(risk_raw.get("allow_short", True)),
        )
    except ValueError as exc:
        raise ConfigError(f"risk settings are not usable: {exc}") from exc

    # --- instrument: start from the venue's defaults, then the user's values ---
    inst_raw = {**venue_defaults["instrument"], **(raw.get("instrument") or {})}
    try:
        instrument = InstrumentSpec.build(
            symbol,
            tick_size=_as_decimal(inst_raw["tick_size"], "instrument.tick_size"),
            lot_step=_as_decimal(inst_raw["lot_step"], "instrument.lot_step"),
            min_notional=_as_decimal(inst_raw["min_notional_usdt"], "instrument.min_notional_usdt"),
            max_leverage=_as_decimal(inst_raw["max_leverage"], "instrument.max_leverage"),
            price_sig_figs=_as_int(inst_raw["price_sig_figs"], "instrument.price_sig_figs"),
            min_qty=_as_decimal(inst_raw.get("min_qty", 0), "instrument.min_qty"),
        )
    except ValueError as exc:
        raise ConfigError(f"instrument settings are not usable: {exc}") from exc

    starting_equity = _as_decimal(
        risk_raw.get("starting_equity_usdt", 10000), "risk.starting_equity_usdt"
    )
    if starting_equity <= 0:
        raise ConfigError("risk.starting_equity_usdt must be positive")

    return BotConfig(
        exchange=exchange,
        grvt=grvt,
        hyperliquid=hyperliquid,
        trading=trading,
        risk=risk,
        instrument=instrument,
        starting_equity=starting_equity,
        source=source,
    )


def _check_symbol_shape(
    exchange: str,
    symbol: str,
    source: str = "<defaults>",
    exchange_was_set: bool = False,
) -> None:
    """Catch a symbol written for the other venue before it reaches the API.

    GRVT names instruments like ``BTC_USDT_Perp``; Hyperliquid uses the bare
    coin, ``BTC``. Sending one to the other fails with an unhelpful error from
    the exchange, so it is refused here where the message can name the file to
    edit - which matters most when an older config predates the setting.
    """
    if not symbol:
        raise ConfigError("trading.symbol cannot be empty")

    looks_like_grvt = "_" in symbol
    if (exchange == "grvt") == looks_like_grvt:
        return

    other = "hyperliquid" if exchange == "grvt" else "grvt"
    expected = (
        "names like BTC_USDT_Perp" if exchange == "grvt" else "the coin on its own, such as BTC"
    )
    where = (
        f"exchange is {exchange!r}"
        if exchange_was_set
        else f"exchange is {exchange!r}, which is the default - your config does not set it"
    )
    raise ConfigError(
        f"trading.symbol {symbol!r} does not look like a {exchange.upper()} instrument.\n"
        f"  config:   {source}\n"
        f"  {where}\n"
        f"{exchange.upper()} uses {expected}.\n"
        f"Either set `exchange: {other}`, write a {exchange.upper()}-style symbol, or "
        "leave trading.symbol empty to take the venue's default."
    )


def describe(config: BotConfig) -> str:
    """A multi-line summary safe to print or log (secrets stay masked)."""
    if config.exchange == "grvt":
        where = f"GRVT {config.grvt.env}"
        who = f"  account         {config.grvt.trading_account_id}"
    else:
        address = config.hyperliquid.account_address
        shown = address.reveal()[:6] + "..." + address.reveal()[-4:] if address else "not set"
        where = f"Hyperliquid {config.hyperliquid.network}"
        who = f"  account         {shown}"
    return "\n".join(
        [
            f"  source            {config.source}",
            f"  exchange          {where}" + ("  *** REAL MONEY ***" if config.is_live else ""),
            who,
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
            f"{config.instrument.lot_step} / {config.instrument.min_notional}"
            + (f" (min size {config.instrument.min_qty})" if config.instrument.min_qty else ""),
        ]
    )


__all__ = [
    "BotConfig",
    "ConfigError",
    "EXCHANGES",
    "GrvtSettings",
    "HyperliquidSettings",
    "Secret",
    "TradingSettings",
    "describe",
    "load_config",
    "raw_defaults",
]
