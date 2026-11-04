"""The Hyperliquid broker.

Everything here follows the published API rather than guesswork:

* Prices obey two rules at once - at most 5 significant figures, and at most
  ``6 - szDecimals`` decimal places, with integer prices always accepted.
  :meth:`tradebot.types.InstrumentSpec.round_price` applies both; this module
  only derives the numbers from ``meta``.
* Sizes are rounded to the asset's ``szDecimals``, so the lot step is
  ``10 ** -szDecimals``.
* A client order id ("cloid") is a 128-bit hex string. Our deterministic
  :func:`tradebot.risk.client_order_id` is exactly 32 hex characters, so the
  exchange enforces the same no-duplicates rule we do.
* An **API wallet** signs, but account queries use the **master account
  address**. The docs call out querying with the agent address as a common
  mistake that silently returns nothing, so the two are separate settings and
  :meth:`HyperliquidBroker.connect` refuses to start without the master one.

The SDK (``hyperliquid-python-sdk``) is imported lazily inside
:meth:`connect`, so importing this module costs nothing and the test suite
runs without it.

Reference: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api
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

#: Perpetuals allow at most ``MAX_DECIMALS - szDecimals`` price decimals.
MAX_DECIMALS_PERP = 6

#: Prices carry at most this many significant figures (integers excepted).
PRICE_SIG_FIGS = 5

#: The docs do not state a minimum as a rule, but the exchange answers
#: "Order must have minimum value of $10." Used only as a fallback default.
DEFAULT_MIN_NOTIONAL = D(10)

#: Intervals the candle endpoint accepts.
INTERVALS = (
    "1m",
    "3m",
    "5m",
    "15m",
    "30m",
    "1h",
    "2h",
    "4h",
    "8h",
    "12h",
    "1d",
    "3d",
    "1w",
    "1M",
)

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
    "1M": 2_592_000_000,
}


# --- pure readers (unit tested against payloads, no network) ----------------


def read_universe(meta: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    """Index the perp universe by coin name.

    The position in ``universe`` is the asset id the exchange endpoint wants,
    so it is kept alongside the precision fields.
    """
    if not isinstance(meta, Mapping):
        raise BrokerError(f"meta: expected a mapping, got {type(meta).__name__}")
    universe = meta.get("universe")
    if not isinstance(universe, Sequence) or not universe:
        raise BrokerError(f"meta carried no universe; keys were {sorted(meta)}")
    out: dict[str, dict[str, Any]] = {}
    for index, entry in enumerate(universe):
        if not isinstance(entry, Mapping):
            continue
        name = entry.get("name")
        if not name:
            continue
        out[str(name)] = {
            "asset": index,
            "sz_decimals": int(entry.get("szDecimals", 0)),
            "max_leverage": D(entry.get("maxLeverage", 1) or 1),
        }
    if not out:
        raise BrokerError("meta universe had no usable entries")
    return out


def instrument_from_universe(
    symbol: str,
    entry: Mapping[str, Any],
    min_notional: Decimal = DEFAULT_MIN_NOTIONAL,
) -> InstrumentSpec:
    """Turn one ``universe`` entry into an :class:`InstrumentSpec`.

    ``szDecimals`` drives both precisions: the lot step directly, and the
    price decimal cap through ``6 - szDecimals``.
    """
    sz_decimals = int(entry.get("sz_decimals", entry.get("szDecimals", 0)))
    if sz_decimals < 0 or sz_decimals > MAX_DECIMALS_PERP:
        raise BrokerError(f"{symbol}: szDecimals {sz_decimals} is out of range")
    price_decimals = MAX_DECIMALS_PERP - sz_decimals
    return InstrumentSpec.build(
        symbol,
        tick_size=Decimal(1).scaleb(-price_decimals),
        lot_step=Decimal(1).scaleb(-sz_decimals),
        min_notional=min_notional,
        max_leverage=entry.get("max_leverage", entry.get("maxLeverage", 1)) or 1,
        price_sig_figs=PRICE_SIG_FIGS,
    )


def read_mark_price(payload: Any, symbol: str) -> Decimal:
    """Pull ``markPx`` for *symbol* out of a ``metaAndAssetCtxs`` reply.

    The reply is a two element array: the meta object, then one context per
    asset in the same order as ``universe``.
    """
    if not isinstance(payload, Sequence) or len(payload) < 2:
        raise BrokerError("metaAndAssetCtxs did not return [meta, contexts]")
    meta, contexts = payload[0], payload[1]
    universe = read_universe(meta)
    if symbol not in universe:
        raise BrokerError(f"{symbol} is not in the perp universe")
    index = universe[symbol]["asset"]
    if not isinstance(contexts, Sequence) or index >= len(contexts):
        raise BrokerError(f"no asset context at index {index} for {symbol}")
    context = contexts[index]
    if not isinstance(context, Mapping):
        raise BrokerError(f"asset context for {symbol} was not a mapping")
    for key in ("markPx", "oraclePx", "midPx"):
        value = context.get(key)
        if value not in (None, "", "0"):
            return require_positive(D(value), f"{key} for {symbol}")
    raise BrokerError(f"asset context for {symbol} carried no price")


def read_mid_price(mids: Mapping[str, Any], symbol: str) -> Decimal:
    """Pull one price out of an ``allMids`` reply."""
    if not isinstance(mids, Mapping):
        raise BrokerError("allMids did not return a mapping")
    value = mids.get(symbol)
    if value in (None, ""):
        raise BrokerError(f"allMids has no entry for {symbol}")
    return require_positive(D(value), f"mid price for {symbol}")


def read_equity(user_state: Mapping[str, Any]) -> Decimal:
    """Account value from ``clearinghouseState``."""
    if not isinstance(user_state, Mapping):
        raise BrokerError(f"user state: expected a mapping, got {type(user_state).__name__}")
    for section in ("marginSummary", "crossMarginSummary"):
        summary = user_state.get(section)
        if isinstance(summary, Mapping) and summary.get("accountValue") not in (None, ""):
            return D(summary["accountValue"])
    if user_state.get("withdrawable") not in (None, ""):
        return D(user_state["withdrawable"])
    raise BrokerError(
        "user state carried no accountValue. If you are querying with the API "
        "wallet address instead of the master account address, the exchange "
        f"returns an empty state. Keys were {sorted(user_state)}"
    )


def read_positions(user_state: Mapping[str, Any]) -> dict[str, Position]:
    """Open positions from ``clearinghouseState``.

    ``szi`` is signed: positive is long, negative is short. Zero-size entries
    are dropped, because treating a closed symbol as open would make the risk
    gate refuse every new trade on it forever.
    """
    if not isinstance(user_state, Mapping):
        raise BrokerError(f"user state: expected a mapping, got {type(user_state).__name__}")
    rows = user_state.get("assetPositions") or []
    out: dict[str, Position] = {}
    for row in rows:
        position = row.get("position") if isinstance(row, Mapping) else None
        if not isinstance(position, Mapping):
            continue
        coin = position.get("coin")
        if not coin:
            continue
        size = D(position.get("szi") or 0)
        if size == ZERO:
            continue
        out[str(coin)] = Position(
            symbol=str(coin),
            side=Side.BUY if size > ZERO else Side.SELL,
            qty=abs(size),
            entry_price=D(position.get("entryPx") or 0),
        )
    return out


def read_candles(rows: Any, symbol: str, interval: str) -> list[Candle]:
    """Normalise a ``candleSnapshot`` reply, oldest first."""
    if rows is None:
        return []
    if not isinstance(rows, Sequence):
        raise BrokerError(f"candle snapshot for {symbol} was not a list")
    out: list[Candle] = []
    for row in rows:
        if not isinstance(row, Mapping):
            continue
        try:
            out.append(
                Candle(
                    symbol=str(row.get("s") or symbol),
                    interval=str(row.get("i") or interval),
                    open_time=datetime.fromtimestamp(int(row["t"]) / 1000, tz=timezone.utc),
                    open=D(row["o"]),
                    high=D(row["h"]),
                    low=D(row["l"]),
                    close=D(row["c"]),
                    volume=D(row.get("v") or 0),
                )
            )
        except (KeyError, TypeError, ValueError) as exc:
            raise BrokerError(f"unreadable candle for {symbol}: {row!r} ({exc})") from exc
    out.sort(key=lambda candle: candle.open_time)
    return out


def read_order_result(payload: Any) -> tuple[str, bool]:
    """Return ``(order_id, was_filled)`` from an exchange reply, or raise.

    A reply with no id is an error rather than a success with ``id=None``: the
    bot must not believe it holds a position it cannot name.
    """
    if not isinstance(payload, Mapping):
        raise BrokerError(f"order reply: expected a mapping, got {type(payload).__name__}")
    if payload.get("status") not in (None, "ok"):
        raise BrokerError(f"order rejected: {payload.get('response') or payload}")

    response = payload.get("response")
    data = response.get("data") if isinstance(response, Mapping) else None
    statuses = None
    for candidate in (data, response, payload):
        if isinstance(candidate, Mapping) and isinstance(candidate.get("statuses"), Sequence):
            statuses = candidate["statuses"]
            break
    if not statuses:
        raise BrokerError(
            f"order reply carried no statuses: {payload!r}. The order may or may "
            "not have reached the book - reconcile before retrying."
        )

    status = statuses[0]
    if not isinstance(status, Mapping):
        raise BrokerError(f"unreadable order status: {status!r}")
    if "error" in status:
        raise BrokerError(f"order rejected: {status['error']}")
    for key, filled in (("filled", True), ("resting", False)):
        body = status.get(key)
        if isinstance(body, Mapping) and body.get("oid") is not None:
            return str(body["oid"]), filled
    raise BrokerError(
        f"order status had no oid: {status!r}. The order may or may not have "
        "reached the book - reconcile before retrying."
    )


# --- the broker -------------------------------------------------------------


class HyperliquidBroker:
    """Talks to Hyperliquid through ``hyperliquid-python-sdk``.

    Install with ``pip install -e ".[hyperliquid]"``. ``info`` and ``exchange``
    can be injected instead, which is how the tests exercise this class with
    no network and no SDK.
    """

    name = "hyperliquid"

    def __init__(
        self,
        settings: Any,
        logger: logging.Logger | None = None,
        info: Any = None,
        exchange: Any = None,
        min_notional: Decimal = DEFAULT_MIN_NOTIONAL,
    ) -> None:
        self.settings = settings
        self.log = logger or logging.getLogger(__name__)
        self.info = info
        self.exchange = exchange
        self.min_notional = D(min_notional)
        self._universe: dict[str, dict[str, Any]] = {}
        self._instruments: dict[str, InstrumentSpec] = {}
        self._cloid_factory: Any = None

    # --- lifecycle -------------------------------------------------------

    @property
    def base_url(self) -> str:
        if self.settings.network == "mainnet":
            return "https://api.hyperliquid.xyz"
        return "https://api.hyperliquid-testnet.xyz"

    def connect(self) -> None:
        if not self.settings.account_address:
            raise BrokerError(
                "hyperliquid.account_address is required: positions and equity are "
                "looked up by the MASTER account address, not the API wallet address"
            )

        if self.info is None or (self.exchange is None and self.settings.api_wallet_key):
            self._build_clients()

        self._universe = read_universe(self.info.meta())
        self.log.info(
            "connected to Hyperliquid %s, %s perp markets, account %s",
            self.settings.network,
            len(self._universe),
            self.settings.account_address.reveal()[:10] + "...",
        )
        if self.exchange is None:
            self.log.warning(
                "no API wallet key configured: this session can read the market "
                "and the account, but cannot place orders"
            )

    def _build_clients(self) -> None:
        try:
            from eth_account import Account  # noqa: PLC0415  (lazy on purpose)
            from hyperliquid.exchange import Exchange
            from hyperliquid.info import Info
            from hyperliquid.utils.types import Cloid
        except ImportError as exc:  # pragma: no cover - depends on the environment
            raise BrokerError(
                "the Hyperliquid SDK is not installed. Live trading needs it:\n"
                '    pip install -e ".[hyperliquid]"\n'
                "Paper trading and the test suite do not: use --dry-run."
            ) from exc

        self._cloid_factory = Cloid
        if self.info is None:
            self.info = Info(self.base_url, skip_ws=True)
        if self.exchange is None and self.settings.api_wallet_key:
            wallet = Account.from_key(self.settings.api_wallet_key.reveal())
            self.exchange = Exchange(
                wallet,
                self.base_url,
                account_address=self.settings.account_address.reveal(),
                vault_address=(
                    self.settings.vault_address.reveal() if self.settings.vault_address else None
                ),
            )

    def _require_info(self) -> Any:
        if self.info is None:
            raise BrokerError("broker is not connected; call connect() first")
        return self.info

    def _require_exchange(self) -> Any:
        if self.exchange is None:
            raise BrokerError(
                "this session cannot trade: set hyperliquid.api_wallet_key to the "
                "private key of an API wallet approved by the master account"
            )
        return self.exchange

    def _cloid(self, client_order_id: str) -> Any:
        """Wrap our 32-character id as the SDK's Cloid type."""
        raw = client_order_id if client_order_id.startswith("0x") else f"0x{client_order_id}"
        if self._cloid_factory is None:
            try:
                from hyperliquid.utils.types import Cloid  # noqa: PLC0415
            except ImportError as exc:  # pragma: no cover
                raise BrokerError("the Hyperliquid SDK is not installed") from exc
            self._cloid_factory = Cloid
        return self._cloid_factory.from_str(raw)

    # --- reads -----------------------------------------------------------

    def _entry(self, symbol: str) -> dict[str, Any]:
        if symbol not in self._universe:
            raise BrokerError(
                f"{symbol} is not a Hyperliquid perp. Use a coin name such as "
                f"BTC or ETH; known markets include "
                f"{', '.join(sorted(self._universe)[:6])}..."
            )
        return self._universe[symbol]

    def fetch_mark_price(self, symbol: str) -> Decimal:
        return read_mark_price(self._require_info().meta_and_asset_ctxs(), symbol)

    def fetch_instrument(self, symbol: str) -> InstrumentSpec:
        if symbol in self._instruments:
            return self._instruments[symbol]
        spec = instrument_from_universe(symbol, self._entry(symbol), self.min_notional)
        self._instruments[symbol] = spec
        self.log.info(
            "%s: lot step %s, price tick %s, %s significant figures, max leverage %s",
            symbol,
            spec.lot_step,
            spec.tick_size,
            spec.price_sig_figs,
            spec.max_leverage,
        )
        return spec

    def fetch_equity(self) -> Decimal:
        state = self._require_info().user_state(self.settings.account_address.reveal())
        return read_equity(state)

    def fetch_positions(self, symbol: str | None = None) -> dict[str, Position]:
        state = self._require_info().user_state(self.settings.account_address.reveal())
        positions = read_positions(state)
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
        end = int(time.time() * 1000)
        # One extra bar, because the most recent one is still forming.
        start = end - span * (limit + 1)
        rows = self._require_info().candles_snapshot(symbol, interval, start, end)
        candles = read_candles(rows, symbol, interval)
        return candles[-limit:] if limit else candles

    # --- writes ----------------------------------------------------------

    def place_order(self, order: SizedOrder) -> OrderReceipt:
        """Send an approved order as a GTC limit order.

        Limit, not market: the limit price the risk gate computed *is* the
        slippage guard, and a market order throws it away.
        """
        exchange = self._require_exchange()
        payload = exchange.order(
            order.symbol,
            order.side is Side.BUY,
            float(order.qty),  # the one float boundary in this package
            float(order.limit_price),
            {"limit": {"tif": "Gtc"}},
            reduce_only=False,
            cloid=self._cloid(order.client_order_id),
        )
        oid, filled = read_order_result(payload)
        self.log.info(
            "order %s: %s %s %s @ %s (oid %s, cloid %s)",
            "filled" if filled else "resting",
            order.side.value,
            order.qty,
            order.symbol,
            order.limit_price,
            oid,
            order.client_order_id,
        )
        return OrderReceipt(
            broker_order_id=oid,
            client_order_id=order.client_order_id,
            symbol=order.symbol,
            side=order.side.value,
            qty=order.qty,
            limit_price=order.limit_price,
            simulated=False,
            raw=payload if isinstance(payload, Mapping) else {},
        )

    def close_position(self, symbol: str) -> OrderReceipt | None:
        """Flatten *symbol* with an aggressive reduce-only IOC order.

        Crossing the book by 5% rather than sending a protective limit: when
        you want out, a price that never fills is the worst outcome.
        """
        held = self.fetch_positions(symbol).get(symbol)
        if held is None:
            self.log.info("nothing open on %s", symbol)
            return None
        exchange = self._require_exchange()
        spec = self.fetch_instrument(symbol)
        mark = self.fetch_mark_price(symbol)
        exiting = held.side.opposite
        if exiting is Side.BUY:
            price = spec.round_price(mark * D("1.05"), favour="up")
        else:
            price = spec.round_price(mark * D("0.95"), favour="down")

        payload = exchange.order(
            symbol,
            exiting is Side.BUY,
            float(held.qty),
            float(price),
            {"limit": {"tif": "Ioc"}},
            reduce_only=True,
        )
        oid, _ = read_order_result(payload)
        self.log.info("closed %s %s on %s (oid %s)", held.side.value, held.qty, symbol, oid)
        return OrderReceipt(
            broker_order_id=oid,
            client_order_id=f"close-{symbol}",
            symbol=symbol,
            side=exiting.value,
            qty=held.qty,
            limit_price=price,
            raw=payload if isinstance(payload, Mapping) else {},
        )

    def cancel_order(self, symbol: str, client_order_id: str) -> Any:
        """Cancel by our own client order id."""
        return self._require_exchange().cancel_by_cloid(symbol, self._cloid(client_order_id))

    def set_leverage(self, symbol: str, leverage: Decimal, cross: bool = True) -> Any:
        """Set account leverage for *symbol*.

        Unlike some venues, Hyperliquid does expose this. It changes an account
        setting, not an order: the bot still caps exposure separately through
        ``risk.max_leverage``.
        """
        capped = min(D(leverage), self.fetch_instrument(symbol).max_leverage)
        if capped != D(leverage):
            self.log.warning(
                "%s allows at most %sx; requesting %sx instead of %sx",
                symbol,
                capped,
                capped,
                leverage,
            )
        result = self._require_exchange().update_leverage(int(capped), symbol, cross)
        self.log.info(
            "set %s leverage to %sx (%s)", symbol, capped, "cross" if cross else "isolated"
        )
        return result
