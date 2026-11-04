"""Value types shared by strategies, the risk gate and the brokers.

Everything here is immutable and free of I/O.  A strategy produces a
:class:`Signal`; the risk gate turns it into a :class:`SizedOrder` or a
:class:`Reject`; a broker is the only thing allowed to touch the network.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field, replace
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from enum import Enum

from .money import ZERO, D, Number, floor_to_step, round_to_tick, significant_figure_step


class Side(str, Enum):
    """Order direction."""

    BUY = "buy"
    SELL = "sell"

    @property
    def opposite(self) -> Side:
        return Side.SELL if self is Side.BUY else Side.BUY

    @property
    def sign(self) -> Decimal:
        """``+1`` for a long, ``-1`` for a short."""
        return D(1) if self is Side.BUY else D(-1)

    @classmethod
    def parse(cls, value: str | Side) -> Side:
        if isinstance(value, Side):
            return value
        try:
            return cls(str(value).strip().lower())
        except ValueError as exc:
            raise ValueError(f"side must be 'buy' or 'sell', got {value!r}") from exc


@dataclass(frozen=True)
class InstrumentSpec:
    """Exchange rules for one symbol.

    These come from the exchange (or from ``instrument:`` in the config file as
    a fallback).  Guessing them is how orders get silently rejected or, worse,
    silently rounded to the wrong size.

    Hyperliquid perps have two price rules at once, so a single tick size
    cannot express them:

    * at most ``price_sig_figs`` significant figures (5), except that integer
      prices are always accepted however many digits they have, and
    * at most ``6 - szDecimals`` decimal places, which is what ``tick_size``
      encodes.

    :meth:`round_price` applies both.  Set ``price_sig_figs`` to 0 for a venue
    with a plain fixed tick.
    """

    symbol: str
    tick_size: Decimal
    lot_step: Decimal
    min_notional: Decimal
    max_leverage: Decimal = D(20)
    price_sig_figs: int = 5
    #: Smallest order the venue accepts, counted in the base asset.
    #: GRVT publishes this as ``min_size``; Hyperliquid has no such rule and
    #: leaves it at zero.
    min_qty: Decimal = ZERO

    def __post_init__(self) -> None:
        for name in ("tick_size", "lot_step", "min_notional"):
            if getattr(self, name) <= ZERO:
                raise ValueError(f"{self.symbol}: {name} must be positive")
        if self.max_leverage <= ZERO:
            raise ValueError(f"{self.symbol}: max_leverage must be positive")
        if self.price_sig_figs < 0:
            raise ValueError(f"{self.symbol}: price_sig_figs cannot be negative")
        if self.min_qty < ZERO:
            raise ValueError(f"{self.symbol}: min_qty cannot be negative")

    @classmethod
    def build(
        cls,
        symbol: str,
        tick_size: Number,
        lot_step: Number,
        min_notional: Number,
        max_leverage: Number = 20,
        price_sig_figs: int = 5,
        min_qty: Number = 0,
    ) -> InstrumentSpec:
        return cls(
            symbol,
            D(tick_size),
            D(lot_step),
            D(min_notional),
            D(max_leverage),
            int(price_sig_figs),
            D(min_qty),
        )

    def price_step(self, around: Number) -> Decimal:
        """The coarsest legal increment near the price *around*."""
        step = self.tick_size
        if self.price_sig_figs > 0:
            step = max(step, significant_figure_step(around, self.price_sig_figs))
        return step

    def round_price(self, value: Number, *, favour: str) -> Decimal:
        """Snap *value* to a price the exchange will accept.

        ``favour`` is ``"down"`` or ``"up"`` and must be chosen so the rounding
        stays protective: down for a buy limit cap, up for a sell limit floor.
        """
        price = D(value)
        return round_to_tick(price, self.price_step(price), favour=favour)

    def round_qty(self, value: Number) -> Decimal:
        """Floor *value* to the lot step.  Always down, never up."""
        return floor_to_step(value, self.lot_step)


@dataclass(frozen=True)
class RiskLimits:
    """Every ceiling the bot is not allowed to cross.

    All percentages are fractions here (``0.01`` is 1%); the config layer does
    the conversion so humans can keep writing ``1.0`` in YAML.
    """

    risk_per_trade: Decimal = D("0.01")
    max_positions: int = 1
    max_daily_loss: Decimal = D("0.03")
    max_drawdown: Decimal = D("0.10")
    max_leverage: Decimal = D(5)
    max_slippage: Decimal = D("0.001")
    fee_rate: Decimal = D("0.0005")
    allow_short: bool = True

    def __post_init__(self) -> None:
        if not (ZERO < self.risk_per_trade <= D("0.5")):
            raise ValueError("risk_per_trade must be in (0, 50%]")
        if self.max_positions < 1:
            raise ValueError("max_positions must be at least 1")
        for name in ("max_daily_loss", "max_drawdown"):
            if getattr(self, name) <= ZERO:
                raise ValueError(f"{name} must be positive")
        if self.max_leverage <= ZERO:
            raise ValueError("max_leverage must be positive")
        if self.max_slippage < ZERO:
            raise ValueError("max_slippage cannot be negative")
        if self.fee_rate < ZERO:
            raise ValueError("fee_rate cannot be negative")


@dataclass(frozen=True)
class Candle:
    """One OHLCV bar, as the exchange reports it."""

    symbol: str
    interval: str
    open_time: datetime
    open: Decimal
    high: Decimal
    low: Decimal
    close: Decimal
    volume: Decimal


@dataclass(frozen=True)
class Position:
    """One open position as the exchange reports it."""

    symbol: str
    side: Side
    qty: Decimal
    entry_price: Decimal

    @property
    def notional(self) -> Decimal:
        return self.qty * self.entry_price


@dataclass(frozen=True)
class PortfolioState:
    """Everything the risk gate needs to know about the account.

    Assembled by the engine from broker calls, then handed to the gate as
    plain data, so the gate itself never performs I/O.
    """

    equity: Decimal
    peak_equity: Decimal
    realized_pnl_today: Decimal = ZERO
    positions: Mapping[str, Position] = field(default_factory=dict)
    halted: bool = False
    halt_reason: str = ""

    @property
    def gross_exposure(self) -> Decimal:
        return sum((p.notional for p in self.positions.values()), ZERO)

    def with_halt(self, reason: str) -> PortfolioState:
        return replace(self, halted=True, halt_reason=reason)


@dataclass(frozen=True)
class Signal:
    """A strategy's request to open a position.

    ``stop_price`` is **required**.  Without it the risk gate cannot size the
    order from a risk budget, and a bot that cannot size from risk is a bot
    that sizes from hope.
    """

    symbol: str
    side: Side
    ref_price: Decimal
    stop_price: Decimal
    reason: str = ""
    take_profit: Decimal | None = None
    created_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    valid_for: timedelta = timedelta(seconds=60)

    def __post_init__(self) -> None:
        if self.ref_price <= ZERO:
            raise ValueError("ref_price must be positive")
        if self.stop_price <= ZERO:
            raise ValueError("stop_price must be positive")
        if self.created_at.tzinfo is None:
            raise ValueError("created_at must be timezone-aware (use UTC)")
        if self.valid_for <= timedelta(0):
            raise ValueError("valid_for must be positive")

    @property
    def valid_until(self) -> datetime:
        return self.created_at + self.valid_for

    @property
    def stop_distance(self) -> Decimal:
        return abs(self.ref_price - self.stop_price)

    @classmethod
    def build(
        cls,
        symbol: str,
        side: str | Side,
        ref_price: Number,
        stop_price: Number,
        reason: str = "",
        take_profit: Number | None = None,
        valid_for_seconds: int = 60,
        created_at: datetime | None = None,
    ) -> Signal:
        """Friendly constructor that accepts plain strings and floats."""
        return cls(
            symbol=symbol,
            side=Side.parse(side),
            ref_price=D(ref_price),
            stop_price=D(stop_price),
            reason=reason,
            take_profit=None if take_profit is None else D(take_profit),
            created_at=created_at or datetime.now(timezone.utc),
            valid_for=timedelta(seconds=valid_for_seconds),
        )


@dataclass(frozen=True)
class SizedOrder:
    """What the risk gate approves: an order that is safe to send."""

    client_order_id: str
    symbol: str
    side: Side
    qty: Decimal
    limit_price: Decimal
    stop_price: Decimal
    take_profit: Decimal | None
    risk_amount: Decimal
    reason: str

    @property
    def notional(self) -> Decimal:
        return self.qty * self.limit_price


class RejectCode(str, Enum):
    """Why an order was refused, ordered from most to least serious.

    The gate always reports the most serious applicable reason, so the log
    line answers "what stopped the bot" rather than "what it noticed last".
    """

    HALTED = "halted"
    MAX_DRAWDOWN = "max_drawdown"
    DAILY_LOSS_LIMIT = "daily_loss_limit"
    EXPIRED = "expired"
    SHORT_NOT_ALLOWED = "short_not_allowed"
    INVALID_STOP = "invalid_stop"
    SLIPPAGE_EXCEEDED = "slippage_exceeded"
    ALREADY_IN_POSITION = "already_in_position"
    TOO_MANY_POSITIONS = "too_many_positions"
    QTY_ROUNDS_TO_ZERO = "qty_rounds_to_zero"
    BELOW_MIN_SIZE = "below_min_size"
    BELOW_MIN_NOTIONAL = "below_min_notional"
    EXCEEDS_EXPOSURE = "exceeds_exposure"
    BAD_INSTRUMENT = "bad_instrument"

    @property
    def severity(self) -> int:
        """Lower is more serious."""
        return _SEVERITY[self.value]


_SEVERITY = {code.value: i for i, code in enumerate(RejectCode)}


@dataclass(frozen=True)
class Reject:
    """A refusal, with enough detail to explain it to a human."""

    code: RejectCode
    detail: str

    def __str__(self) -> str:
        return f"{self.code.value}: {self.detail}"
