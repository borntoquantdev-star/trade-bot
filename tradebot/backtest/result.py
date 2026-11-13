"""What a backtest produces, and the arithmetic on top of it.

Deliberately separate from the replay loop: the loop decides what happened,
this decides what it means.  Keeping them apart means the statistics can be
tested against a handful of hand-written trades, with no candles and no
strategy involved.

Every number here is a :class:`~decimal.Decimal` for the same reason the rest
of the package is: a return of "19.999999999999996 percent" is a number nobody
should have to explain.
"""

from __future__ import annotations

from collections import Counter
from collections.abc import Sequence
from dataclasses import dataclass, field
from datetime import datetime
from decimal import Decimal

from ..intervals import interval_seconds
from ..money import ZERO, D
from ..types import RejectCode, Side

HUNDRED = D(100)
ONE = D(1)


def _power(base: Decimal, exponent: Decimal) -> Decimal:
    """Raise *base* to a fractional *exponent* without leaving Decimal."""
    if base <= ZERO:
        return ZERO
    return (base.ln() * exponent).exp()


@dataclass(frozen=True)
class Trade:
    """One round trip: a fill, and the fill that closed it."""

    symbol: str
    side: Side
    qty: Decimal
    entry_at: datetime
    entry_price: Decimal
    exit_at: datetime
    exit_price: Decimal
    stop_price: Decimal
    risk_amount: Decimal
    fees: Decimal
    exit_reason: str
    signal_reason: str = ""

    @property
    def gross_pnl(self) -> Decimal:
        """Profit before costs, signed by direction."""
        return (self.exit_price - self.entry_price) * self.qty * self.side.sign

    @property
    def net_pnl(self) -> Decimal:
        """What actually reached the account."""
        return self.gross_pnl - self.fees

    @property
    def is_win(self) -> bool:
        return self.net_pnl > ZERO

    @property
    def r_multiple(self) -> Decimal:
        """Result measured in units of the risk taken.

        The number that matters: a strategy with a 40 percent hit rate and an
        average of +1.5R is a good strategy, and no win rate on its own can
        tell you that.
        """
        if self.risk_amount <= ZERO:
            return ZERO
        return self.net_pnl / self.risk_amount


@dataclass(frozen=True)
class EquityPoint:
    """One mark-to-market reading of the account."""

    at: datetime
    equity: Decimal
    in_position: bool = False


@dataclass
class BacktestResult:
    """Everything one run produced."""

    symbol: str
    interval: str
    strategy: str
    exchange: str = ""
    starting_equity: Decimal = ZERO
    bars: int = 0
    first_price: Decimal = ZERO
    last_price: Decimal = ZERO
    start: datetime | None = None
    end: datetime | None = None
    trades: list[Trade] = field(default_factory=list)
    equity_curve: list[EquityPoint] = field(default_factory=list)
    rejects: Counter[str] = field(default_factory=Counter)
    signals: int = 0
    unfilled_orders: int = 0
    halted_at: datetime | None = None
    halt_reason: str = ""
    warnings: list[str] = field(default_factory=list)

    # --- headline ---------------------------------------------------------

    @property
    def final_equity(self) -> Decimal:
        return self.equity_curve[-1].equity if self.equity_curve else self.starting_equity

    @property
    def net_pnl(self) -> Decimal:
        return self.final_equity - self.starting_equity

    @property
    def return_pct(self) -> Decimal:
        if self.starting_equity <= ZERO:
            return ZERO
        return self.net_pnl / self.starting_equity * HUNDRED

    @property
    def fees_paid(self) -> Decimal:
        return sum((t.fees for t in self.trades), ZERO)

    # --- trade statistics -------------------------------------------------

    @property
    def wins(self) -> list[Trade]:
        return [t for t in self.trades if t.is_win]

    @property
    def losses(self) -> list[Trade]:
        return [t for t in self.trades if not t.is_win]

    @property
    def win_rate_pct(self) -> Decimal:
        if not self.trades:
            return ZERO
        return D(len(self.wins)) / D(len(self.trades)) * HUNDRED

    @property
    def profit_factor(self) -> Decimal | None:
        """Gross winnings divided by gross losses.

        ``None`` when nothing was lost, because "infinite" is not a number a
        report should print as if it were measured.
        """
        won = sum((t.net_pnl for t in self.wins), ZERO)
        lost = -sum((t.net_pnl for t in self.losses), ZERO)
        if lost <= ZERO:
            return None
        return won / lost

    @property
    def expectancy_r(self) -> Decimal:
        """Average result per trade, in units of risk."""
        if not self.trades:
            return ZERO
        return sum((t.r_multiple for t in self.trades), ZERO) / D(len(self.trades))

    @property
    def best_trade(self) -> Trade | None:
        return max(self.trades, key=lambda t: t.net_pnl, default=None)

    @property
    def worst_trade(self) -> Trade | None:
        return min(self.trades, key=lambda t: t.net_pnl, default=None)

    # --- risk -------------------------------------------------------------

    @property
    def peak_equity(self) -> Decimal:
        return max((p.equity for p in self.equity_curve), default=self.starting_equity)

    @property
    def max_drawdown(self) -> Decimal:
        """Largest peak-to-trough fall in account value."""
        return max_drawdown(self.equity_curve)

    @property
    def max_drawdown_pct(self) -> Decimal:
        peak = ZERO
        worst = ZERO
        for point in self.equity_curve:
            peak = max(peak, point.equity)
            if peak > ZERO:
                worst = max(worst, (peak - point.equity) / peak * HUNDRED)
        return worst

    @property
    def exposure_pct(self) -> Decimal:
        """Share of bars spent holding something."""
        if not self.equity_curve:
            return ZERO
        held = sum(1 for p in self.equity_curve if p.in_position)
        return D(held) / D(len(self.equity_curve)) * HUNDRED

    @property
    def longest_losing_streak(self) -> int:
        worst = streak = 0
        for trade in self.trades:
            streak = 0 if trade.is_win else streak + 1
            worst = max(worst, streak)
        return worst

    # --- measured against time -------------------------------------------

    @property
    def years(self) -> Decimal:
        """Length of the test in years, for anything annualised."""
        if self.start is None or self.end is None:
            return ZERO
        seconds = D(str((self.end - self.start).total_seconds()))
        return seconds / D(365 * 24 * 60 * 60)

    @property
    def cagr_pct(self) -> Decimal | None:
        """Compound annual growth rate.

        ``None`` when the test is shorter than about a month, or the account
        was wiped out: an annualised figure from three weeks of data is a
        number with a decimal point and no meaning.
        """
        years = self.years
        if years < D("0.08") or self.starting_equity <= ZERO or self.final_equity <= ZERO:
            return None
        growth = self.final_equity / self.starting_equity
        return (_power(growth, ONE / years) - ONE) * HUNDRED

    @property
    def buy_and_hold_pct(self) -> Decimal | None:
        """What simply holding the asset over the same period would have paid.

        The benchmark that matters.  If the bot cannot beat doing nothing,
        the whole apparatus is machinery for losing a race.
        """
        if self.first_price <= ZERO or self.last_price <= ZERO:
            return None
        return (self.last_price - self.first_price) / self.first_price * HUNDRED

    @property
    def bars_underwater(self) -> int:
        """Longest run of bars spent below a previous peak."""
        peak = ZERO
        longest = current = 0
        for point in self.equity_curve:
            if point.equity >= peak:
                peak = point.equity
                current = 0
            else:
                current += 1
                longest = max(longest, current)
        return longest

    @property
    def days_underwater(self) -> Decimal:
        """The same stretch in days, which is the unit a person actually feels."""
        return D(self.bars_underwater) * D(interval_seconds(self.interval)) / D(86_400)

    @property
    def monthly_returns(self) -> list[tuple[str, Decimal]]:
        """Return per calendar month, oldest first."""
        months: dict[str, list[Decimal]] = {}
        for point in self.equity_curve:
            months.setdefault(f"{point.at:%Y-%m}", []).append(point.equity)
        out: list[tuple[str, Decimal]] = []
        for month, values in sorted(months.items()):
            opening, closing = values[0], values[-1]
            if opening > ZERO:
                out.append((month, (closing - opening) / opening * HUNDRED))
        return out

    @property
    def worst_month(self) -> tuple[str, Decimal] | None:
        months = self.monthly_returns
        return min(months, key=lambda row: row[1]) if months else None

    @property
    def trades_per_year(self) -> Decimal:
        years = self.years
        if years <= ZERO:
            return ZERO
        return D(len(self.trades)) / years

    # --- return against volatility ----------------------------------------

    @property
    def periods_per_year(self) -> Decimal:
        """How many bars of this interval fit in a year."""
        return D(365 * 24 * 60 * 60) / D(interval_seconds(self.interval))

    def _bar_returns(self) -> list[Decimal]:
        returns: list[Decimal] = []
        for previous, current in zip(self.equity_curve, self.equity_curve[1:], strict=False):
            if previous.equity > ZERO:
                returns.append((current.equity - previous.equity) / previous.equity)
        return returns

    def sharpe(self, periods_per_year: Decimal | None = None) -> Decimal | None:
        """Return per unit of volatility, annualised, risk-free rate zero.

        ``None`` when the account never moved.  Dividing by no volatility at
        all produces a number that reads as "perfect" and means "no data".
        """
        returns = self._bar_returns()
        if len(returns) < 2:
            return None
        scale = self.periods_per_year if periods_per_year is None else periods_per_year
        mean = sum(returns, ZERO) / D(len(returns))
        variance = sum(((r - mean) ** 2 for r in returns), ZERO) / D(len(returns) - 1)
        if variance <= ZERO:
            return None
        return mean / variance.sqrt() * scale.sqrt()

    def sortino(self, periods_per_year: Decimal | None = None) -> Decimal | None:
        """Sharpe, counting only the volatility that hurt."""
        returns = self._bar_returns()
        if len(returns) < 2:
            return None
        scale = self.periods_per_year if periods_per_year is None else periods_per_year
        mean = sum(returns, ZERO) / D(len(returns))
        downside = [r for r in returns if r < ZERO]
        if not downside:
            return None
        variance = sum((r**2 for r in downside), ZERO) / D(len(returns))
        if variance <= ZERO:
            return None
        return mean / variance.sqrt() * scale.sqrt()

    @property
    def average_win(self) -> Decimal:
        wins = self.wins
        return sum((t.net_pnl for t in wins), ZERO) / D(len(wins)) if wins else ZERO

    @property
    def average_loss(self) -> Decimal:
        losses = self.losses
        return sum((t.net_pnl for t in losses), ZERO) / D(len(losses)) if losses else ZERO

    @property
    def win_loss_ratio(self) -> Decimal | None:
        """Average winner divided by average loser.

        Always reported next to the win rate: a 35 percent win rate is fine
        when this is above 2, and reading either number on its own is how
        people talk themselves into a losing system.
        """
        loss = -self.average_loss
        if loss <= ZERO:
            return None
        return self.average_win / loss

    def profit_without_best(self, drop: int = 2) -> Decimal:
        """Net profit with the *drop* biggest winners removed.

        If the whole result rests on two trades, it rests on luck.
        """
        ranked = sorted(self.trades, key=lambda t: t.net_pnl, reverse=True)
        return sum((t.net_pnl for t in ranked[drop:]), ZERO)

    # --- what stopped it --------------------------------------------------

    @property
    def was_halted(self) -> bool:
        return self.halted_at is not None

    @property
    def top_rejects(self) -> list[tuple[str, int]]:
        """Refusal reasons, most frequent first."""
        return self.rejects.most_common()

    def reject_total(self) -> int:
        return sum(self.rejects.values())

    @property
    def no_trade_reason(self) -> str:
        """Why a run produced no trades, as a code.  Empty when it traded.

        The single most common question from someone who just ran their first
        backtest, so the answer belongs in the result rather than in whatever
        the reader can piece together from a log.

        A code rather than a sentence because two readers need it in two
        languages: :meth:`never_traded_because` words it in English for the
        console, and the dashboard words it in Thai.  The ladder of
        conditions lives here once; the wording is each reader's own problem.
        """
        if self.trades:
            return ""
        if self.bars == 0:
            return "no_candles"
        if self.signals == 0:
            return "no_signal"
        if self.rejects:
            return "all_refused"
        if self.unfilled_orders:
            return "never_filled"
        return "unrecorded"

    def never_traded_because(self) -> str:
        """:attr:`no_trade_reason`, worded in English."""
        code = self.no_trade_reason
        if code == "":
            return ""
        if code == "no_candles":
            return "there were no candles to run on"
        if code == "no_signal":
            return "the strategy never produced a signal on this data"
        if code == "all_refused":
            reject, count = self.top_rejects[0]
            return f"every signal was refused; most often {reject} ({count} times)"
        if code == "never_filled":
            return (
                f"{self.unfilled_orders} order(s) were approved but the price never "
                "came back to the limit, so nothing filled"
            )
        return "no trade was opened, and nothing recorded why"


def max_drawdown(curve: Sequence[EquityPoint]) -> Decimal:
    """Largest peak-to-trough fall, in account currency."""
    peak = ZERO
    worst = ZERO
    for point in curve:
        peak = max(peak, point.equity)
        worst = max(worst, peak - point.equity)
    return worst


def reject_counter(codes: Sequence[RejectCode]) -> Counter[str]:
    """Group reject codes by name, for the summary."""
    return Counter(code.value for code in codes)


__all__ = [
    "BacktestResult",
    "EquityPoint",
    "Trade",
    "max_drawdown",
    "reject_counter",
]
