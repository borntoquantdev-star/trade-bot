"""The GRVT broker.

Shaped like :mod:`tradebot.brokers.hyperliquid`: the SDK is imported lazily
inside :meth:`GrvtBroker.connect`, and every reply is parsed by a pure
``read_*`` function that returns a value or raises - never a guess.

Three things differ from Hyperliquid and are handled here, not in the gate:

* **Client order ids are integers.** The SDK does ``int(params["client_order_id"])``
  and defaults to a random uint32, so the canonical hex id from
  :func:`tradebot.risk.client_order_id` is folded down to a uint32 here.
* **Symbols are pairs** - ``BTC_USDT_Perp``, not ``BTC``.
* **There is a published minimum order size** (``min_size``, in the base
  asset) as well as a minimum value, so :class:`InstrumentSpec` carries
  ``min_qty`` and the gate refuses below it.

Install the SDK with ``pip install -e ".[grvt]"``.
Reference: https://api-docs.grvt.io/ and https://github.com/gravity-technologies/grvt-pysdk
"""

from __future__ import annotations

import logging
import time
from collections.abc import Mapping, Sequence
from datetime import datetime, timezone
from decimal import Decimal
from typing import Any

from ..money import ZERO, D
from ..types import Candle, InstrumentSpec, Position, Side, SizedOrder
from .base import BrokerError, OrderReceipt, require_positive

#: Default minimum order value when the venue does not publish one.
DEFAULT_MIN_NOTIONAL = D(500)

#: A perpetual price can never reach this. The raw GRVT API returns prices
#: scaled by 1e9, so a value this large means a raw client was injected where
#: a ccxt-style one was expected - better to say so than to size an order from
#: a number a billion times too big.
IMPLAUSIBLE_PRICE = D(10) ** 12

INTERVALS = ("1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1d", "3d", "1w")

_INTERVAL_MS = {
    "1m": 60_000,
    "3m": 180_000,
    "5m": 300_000,
    "15m": 900_000,
    "30m": 1_800_000,
    "1h": 3_600_000,
    "2h": 7_200_000,
    "4h": 14_400_000,
    "8h": 28_800_000,
    "12h": 43_200_000,
    "1d": 86_400_000,
    "3d": 259_200_000,
    "1w": 604_800_000,
}

_PRICE_KEYS = ("mark_price", "markPrice", "last", "last_price", "lastPrice", "index_price")


# --- pure reply readers (unit tested, no network) ---------------------------


def _unwrap(payload: Mapping[str, Any]) -> Mapping[str, Any]:
    """GRVT often nests the useful part under ``result``."""
    inner = payload.get("result")
    return inner if isinstance(inner, Mapping) else payload


def raise_for_error(payload: Mapping[str, Any], what: str) -> None:
    """Raise :class:`BrokerError` if *payload* describes a failure.

    Any one of these is enough to count as a failure. The bot this grew out of
    required three conditions at once, so several shapes of error slipped
    through as success.
    """
    if not isinstance(payload, Mapping):
        raise BrokerError(f"{what}: expected a mapping, got {type(payload).__name__}")
    code = payload.get("code")
    if code not in (None, 0, "0", 200):
        raise BrokerError(f"{what} failed with code {code}: {payload.get('message') or payload}")
    for key in ("error", "errors"):
        if payload.get(key):
            raise BrokerError(f"{what} failed: {payload[key]}")
    status = payload.get("status")
    if isinstance(status, int) and status >= 400:
        raise BrokerError(f"{what} failed with HTTP {status}: {payload.get('message') or payload}")


def _check_scale(price: Decimal, symbol: str, key: str) -> Decimal:
    if price >= IMPLAUSIBLE_PRICE:
        raise BrokerError(
            f"{key} for {symbol} came back as {price}, which is far too large to be a "
            "price. The raw GRVT API scales prices by 1e9 - use the ccxt-style client "
            "(GrvtCcxt), or divide before handing the value over."
        )
    return price


def read_price(payload: Mapping[str, Any], symbol: str) -> Decimal:
    """Pull a tradable price out of a ticker reply."""
    raise_for_error(payload, f"ticker for {symbol}")
    body = _unwrap(payload)
    for key in _PRICE_KEYS:
        value = body.get(key)
        if value not in (None, "", 0, "0"):
            return _check_scale(require_positive(D(value), f"{key} for {symbol}"), symbol, key)
    raise BrokerError(f"ticker for {symbol} carried no usable price; keys were {sorted(body)}")


def read_instrument(
    payload: Mapping[str, Any],
    symbol: str,
    fallback: InstrumentSpec,
) -> InstrumentSpec:
    """Build an :class:`InstrumentSpec` from a ``fetch_market`` reply.

    GRVT publishes ``tick_size``, ``min_size`` and ``base_decimals``; the lot
    step is ``10 ** -base_decimals``. It does not publish a minimum order
    *value*, so that stays at the configured fallback.
    """
    try:
        raise_for_error(payload, f"instrument {symbol}")
    except BrokerError:
        return fallback
    body = _unwrap(payload)

    tick = body.get("tick_size") or body.get("tickSize")
    min_size = body.get("min_size") or body.get("minSize")
    base_decimals = body.get("base_decimals", body.get("baseDecimals"))
    lot = None
    if base_decimals not in (None, ""):
        try:
            lot = Decimal(1).scaleb(-int(base_decimals))
        except (TypeError, ValueError):
            lot = None

    try:
        return InstrumentSpec.build(
            symbol,
            tick_size=tick if tick else fallback.tick_size,
            lot_step=lot if lot else fallback.lot_step,
            min_notional=fallback.min_notional,
            max_leverage=body.get("max_leverage") or fallback.max_leverage,
            # GRVT prices follow a plain tick; no significant-figure rule.
            price_sig_figs=0,
            min_qty=min_size if min_size else fallback.min_qty,
        )
    except ValueError:
        return fallback


def read_equity(payload: Mapping[str, Any], quote: str = "USDT") -> Decimal:
    """Pull account equity out of a balance reply."""
    raise_for_error(payload, "balance")
    body = _unwrap(payload)
    for key in ("total_equity", "totalEquity", "equity", "total_value"):
        if body.get(key) not in (None, ""):
            return D(body[key])
    total = body.get("total")
    if isinstance(total, Mapping):
        for ccy in (quote, "USD", "USDC"):
            if total.get(ccy) not in (None, ""):
                return D(total[ccy])
    raise BrokerError(f"balance reply carried no equity; keys were {sorted(body)}")


def read_positions(payload: Any) -> dict[str, Position]:
    """Normalise a positions reply.

    ``size`` is signed: negative means short. Zero-size rows are dropped,
    because treating a closed instrument as open would make the risk gate
    refuse every new trade on it forever.
    """
    if isinstance(payload, Mapping):
        raise_for_error(payload, "positions")
        body = _unwrap(payload)
        rows = body.get("positions") if isinstance(body, Mapping) else None
        if rows is None:
            rows = []
    else:
        rows = payload or []

    out: dict[str, Position] = {}
    for row in rows:
        if not isinstance(row, Mapping):
            continue
        symbol = row.get("instrument") or row.get("symbol")
        if not symbol:
            continue
        raw_size = row.get("size", row.get("contracts", row.get("amount", 0)))
        size = D(raw_size or 0)
        if size == ZERO:
            continue
        side_field = row.get("side")
        side = Side.parse(side_field) if side_field else (Side.BUY if size > ZERO else Side.SELL)
        entry = row.get("entry_price", row.get("entryPrice", row.get("avg_entry_price", 0)))
        out[str(symbol)] = Position(
            symbol=str(symbol),
            side=side,
            qty=abs(size),
            entry_price=D(entry or 0),
        )
    return out


def read_candles(payload: Any, symbol: str, interval: str) -> list[Candle]:
    """Normalise an OHLCV reply, oldest first.

    Accepts both the ccxt list-of-lists shape
    ``[[ts, open, high, low, close, volume], ...]`` and a list of mappings.
    """
    if payload is None:
        return []
    rows = payload
    if isinstance(payload, Mapping):
        raise_for_error(payload, f"candles for {symbol}")
        body = _unwrap(payload)
        rows = body.get("result") or body.get("candles") or body.get("klines") or []
    if not isinstance(rows, Sequence):
        raise BrokerError(f"candle reply for {symbol} was not a list")

    out: list[Candle] = []
    for row in rows:
        try:
            if isinstance(row, Mapping):
                stamp = row.get("open_time", row.get("t", row.get("time")))
                values = (
                    row.get("open", row.get("o")),
                    row.get("high", row.get("h")),
                    row.get("low", row.get("l")),
                    row.get("close", row.get("c")),
                )
                volume = row.get("volume", row.get("v")) or 0
            elif isinstance(row, Sequence) and len(row) >= 5:
                stamp, *rest = row
                values = tuple(rest[:4])
                volume = rest[4] if len(rest) > 4 else 0
            else:
                continue
            ms = int(stamp)
            if ms > 10**14:  # nanoseconds, as GRVT reports elsewhere
                ms //= 1_000_000
            out.append(
                Candle(
                    symbol=symbol,
                    interval=interval,
                    open_time=datetime.fromtimestamp(ms / 1000, tz=timezone.utc),
                    open=D(values[0]),
                    high=D(values[1]),
                    low=D(values[2]),
                    close=D(values[3]),
                    volume=D(volume or 0),
                )
            )
        except (KeyError, IndexError, TypeError, ValueError) as exc:
            raise BrokerError(f"unreadable candle for {symbol}: {row!r} ({exc})") from exc
    out.sort(key=lambda candle: candle.open_time)
    return out


def read_order_id(payload: Mapping[str, Any]) -> str:
    """Pull the exchange order id out of a create-order reply, or raise.

    A reply with no id is an error rather than a success with ``id=None``: the
    bot must not believe it holds a position it cannot name.
    """
    raise_for_error(payload, "order placement")
    body = _unwrap(payload)
    for key in ("order_id", "orderId", "id", "client_order_id"):
        value = body.get(key)
        if value:
            return str(value)
    metadata = body.get("metadata")
    if isinstance(metadata, Mapping) and metadata.get("client_order_id"):
        return str(metadata["client_order_id"])
    raise BrokerError(
        f"order reply carried no order id; keys were {sorted(body)}. The order may or "
        "may not have reached the book - reconcile before retrying."
    )


def numeric_client_order_id(client_order_id: str) -> int:
    """Fold our canonical hex id down to the uint32 GRVT expects.

    The SDK does ``int(params["client_order_id"])`` and generates a random
    uint32 when none is given, so the full 128-bit id cannot travel. The first
    eight hex characters are still deterministic for a given signal, which is
    what idempotency needs.

    >>> numeric_client_order_id("89c7ffc74d14687c635c3ab535fbec15")
    2311191495
    """
    digits = client_order_id[2:] if client_order_id.startswith("0x") else client_order_id
    try:
        return int(digits[:8], 16)
    except ValueError as exc:
        raise BrokerError(f"client order id {client_order_id!r} is not hexadecimal") from exc


# --- the broker -------------------------------------------------------------


class GrvtBroker:
    """Talks to GRVT through ``grvt-pysdk``'s ccxt-compatible client.

    A ``client`` can be injected instead, which is how the tests exercise this
    class with no network and no SDK.
    """

    name = "grvt"

    def __init__(
        self,
        settings: Any,
        logger: logging.Logger | None = None,
        client: Any = None,
        fallback_instrument: InstrumentSpec | None = None,
    ) -> None:
        self.settings = settings
        self.log = logger or logging.getLogger(__name__)
        self.client = client
        self._fallback = fallback_instrument
        self._instruments: dict[str, InstrumentSpec] = {}

    # --- lifecycle -------------------------------------------------------

    def connect(self) -> None:
        if self.client is not None:
            self.log.info("grvt broker using an injected client (%s)", type(self.client).__name__)
            return
        try:
            from pysdk.grvt_ccxt import GrvtCcxt  # noqa: PLC0415  (lazy on purpose)
            from pysdk.grvt_ccxt_env import GrvtEnv
        except ImportError as exc:  # pragma: no cover - depends on the environment
            raise BrokerError(
                "the GRVT SDK is not installed. Live trading needs it:\n"
                '    pip install -e ".[grvt]"\n'
                "Paper trading and the test suite do not: use --dry-run."
            ) from exc

        try:
            self.client = GrvtCcxt(
                GrvtEnv(self.settings.env),
                self.log,
                parameters={
                    "api_key": self.settings.api_key.reveal(),
                    "trading_account_id": self.settings.trading_account_id.reveal(),
                    "private_key": self.settings.private_key.reveal(),
                },
            )
        except Exception as exc:  # pragma: no cover - needs real credentials
            raise BrokerError(
                f"could not open a GRVT session on {self.settings.env}: {exc}"
            ) from exc
        self.log.info("connected to GRVT (%s)", self.settings.env)

    def _require_client(self) -> Any:
        if self.client is None:
            raise BrokerError("broker is not connected; call connect() first")
        return self.client

    # --- reads -----------------------------------------------------------

    def fetch_mark_price(self, symbol: str) -> Decimal:
        return read_price(self._require_client().fetch_ticker(symbol) or {}, symbol)

    def fetch_instrument(self, symbol: str) -> InstrumentSpec:
        if symbol in self._instruments:
            return self._instruments[symbol]
        if self._fallback is None:
            raise BrokerError(
                f"no instrument spec for {symbol}: add an 'instrument:' block to the config"
            )
        spec = self._fallback
        getter = getattr(self._require_client(), "fetch_market", None)
        if callable(getter):
            try:
                spec = read_instrument(getter(symbol) or {}, symbol, self._fallback)
            except (BrokerError, TypeError, ValueError) as exc:
                self.log.warning(
                    "could not read %s metadata from the exchange (%s); using config values",
                    symbol,
                    exc,
                )
        self._instruments[symbol] = spec
        self.log.info(
            "%s: tick %s, lot step %s, min size %s, min value %s",
            symbol,
            spec.tick_size,
            spec.lot_step,
            spec.min_qty,
            spec.min_notional,
        )
        return spec

    def fetch_equity(self) -> Decimal:
        return read_equity(self._require_client().fetch_balance() or {})

    def fetch_positions(self, symbol: str | None = None) -> dict[str, Position]:
        client = self._require_client()
        payload = client.fetch_positions([symbol] if symbol else [])
        positions = read_positions(payload)
        if symbol is not None:
            return {k: v for k, v in positions.items() if k == symbol}
        return positions

    def fetch_candles(self, symbol: str, interval: str, limit: int) -> list[Candle]:
        """Real OHLC history, used to warm a strategy up at start-up."""
        if interval not in _INTERVAL_MS:
            raise BrokerError(f"interval must be one of {', '.join(INTERVALS)}, got {interval!r}")
        if limit <= 0:
            return []
        span = _INTERVAL_MS[interval]
        since = int(time.time() * 1000) - span * (limit + 1)
        payload = self._require_client().fetch_ohlcv(symbol, interval, since, limit + 1)
        candles = read_candles(payload, symbol, interval)
        return candles[-limit:] if limit else candles

    # --- writes ----------------------------------------------------------

    def place_order(self, order: SizedOrder) -> OrderReceipt:
        """Send an approved order as a limit order.

        Limit, not market: the limit price the risk gate computed *is* the
        slippage guard, and a market order throws it away.
        """
        payload = self._require_client().create_order(
            symbol=order.symbol,
            order_type="limit",
            side=order.side.value,
            amount=float(order.qty),  # the one float boundary in this package
            price=float(order.limit_price),
            params={
                "client_order_id": numeric_client_order_id(order.client_order_id),
                "time_in_force": "GOOD_TILL_TIME",
                "reduce_only": False,
                "sub_account_id": self.settings.sub_account_id,
            },
        )
        broker_id = read_order_id(payload or {})
        self.log.info(
            "order accepted: %s %s %s @ %s (exchange id %s, our id %s)",
            order.side.value,
            order.qty,
            order.symbol,
            order.limit_price,
            broker_id,
            order.client_order_id,
        )
        return OrderReceipt(
            broker_order_id=broker_id,
            client_order_id=order.client_order_id,
            symbol=order.symbol,
            side=order.side.value,
            qty=order.qty,
            limit_price=order.limit_price,
            simulated=False,
            raw=payload if isinstance(payload, Mapping) else {},
        )

    def close_position(self, symbol: str) -> OrderReceipt | None:
        """Flatten *symbol* with a reduce-only market order.

        Exits use market orders on purpose: when you want out, a protective
        limit price that never fills is the worst outcome.
        """
        held = self.fetch_positions(symbol).get(symbol)
        if held is None:
            self.log.info("nothing open on %s", symbol)
            return None
        payload = self._require_client().create_order(
            symbol=symbol,
            order_type="market",
            side=held.side.opposite.value,
            amount=float(held.qty),
            params={"reduce_only": True, "sub_account_id": self.settings.sub_account_id},
        )
        broker_id = read_order_id(payload or {})
        self.log.info("closed %s %s on %s (id %s)", held.side.value, held.qty, symbol, broker_id)
        return OrderReceipt(
            broker_order_id=broker_id,
            client_order_id=f"close-{symbol}",
            symbol=symbol,
            side=held.side.opposite.value,
            qty=held.qty,
            limit_price=held.entry_price,
            raw=payload if isinstance(payload, Mapping) else {},
        )

    def cancel_order(self, symbol: str, client_order_id: str) -> Any:
        """Cancel by our own client order id."""
        return self._require_client().cancel_order(
            symbol=symbol,
            params={"client_order_id": numeric_client_order_id(client_order_id)},
        )

    def set_leverage(self, symbol: str, leverage: Decimal) -> None:
        """GRVT does not expose leverage through this client.

        The old bot accepted a ``leverage=`` argument on every order and
        silently ignored it, which read as if leverage was being applied.
        Saying so plainly is better than pretending.
        """
        raise BrokerError(
            f"this SDK cannot set leverage; set {leverage}x for {symbol} on the GRVT "
            "web interface before running the bot. The bot caps exposure separately "
            "through risk.max_leverage."
        )
