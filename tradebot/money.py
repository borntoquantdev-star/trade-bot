"""Decimal helpers.

Money never touches ``float`` in this package.  Every price, quantity and
amount is a :class:`decimal.Decimal`, and every conversion from the outside
world (JSON, YAML, exchange payloads) goes through :func:`D`.
"""

from __future__ import annotations

from decimal import ROUND_DOWN, ROUND_UP, Decimal, InvalidOperation

Number = int | str | float | Decimal

ZERO = Decimal("0")
ONE = Decimal("1")
HUNDRED = Decimal("100")


def D(value: Number) -> Decimal:
    """Convert *value* to :class:`Decimal` without binary float error.

    ``float`` inputs are stringified first, so ``D(0.1)`` is exactly ``0.1``
    and not ``0.1000000000000000055511151231257827``.

    >>> D(0.1) + D(0.2) == D("0.3")
    True
    """
    if isinstance(value, Decimal):
        return value
    if isinstance(value, float):
        return Decimal(repr(value))
    try:
        return Decimal(value)
    except (InvalidOperation, TypeError, ValueError) as exc:
        raise ValueError(f"cannot read {value!r} as a decimal number") from exc


def pct(value: Number) -> Decimal:
    """Turn a percentage into a fraction: ``pct(1.5) -> 0.015``."""
    return D(value) / HUNDRED


def floor_to_step(value: Number, step: Number) -> Decimal:
    """Round *value* DOWN to a multiple of *step* (order quantities).

    Rounding down is deliberate: it can only ever make an order smaller than
    the risk budget allows, never larger.

    >>> floor_to_step("0.0127", "0.001")
    Decimal('0.012')
    """
    v, s = D(value), D(step)
    if s <= ZERO:
        raise ValueError(f"step must be positive, got {s}")
    return (v / s).to_integral_value(rounding=ROUND_DOWN) * s


def ceil_to_step(value: Number, step: Number) -> Decimal:
    """Round *value* UP to a multiple of *step*."""
    v, s = D(value), D(step)
    if s <= ZERO:
        raise ValueError(f"step must be positive, got {s}")
    return (v / s).to_integral_value(rounding=ROUND_UP) * s


def round_to_tick(value: Number, tick: Number, *, favour: str = "down") -> Decimal:
    """Snap a price to the instrument tick size.

    ``favour="down"`` floors, ``favour="up"`` ceils.  Used for slippage limit
    prices, where the direction decides whether the cap stays protective.
    """
    if favour == "down":
        return floor_to_step(value, tick)
    if favour == "up":
        return ceil_to_step(value, tick)
    raise ValueError("favour must be 'down' or 'up'")


def significant_figure_step(value: Number, sig_figs: int) -> Decimal:
    """The coarsest step that keeps *value* within *sig_figs* significant digits.

    Hyperliquid caps perp prices at 5 significant figures, with integers always
    accepted however many digits they have.  That is not a fixed tick: the
    allowed increment depends on the price's magnitude.

    >>> significant_figure_step("1234.56", 5)
    Decimal('0.1')
    >>> significant_figure_step("100100.0", 5)
    Decimal('1')
    """
    v = D(value)
    if sig_figs <= 0:
        raise ValueError("sig_figs must be positive")
    if v == ZERO:
        return ONE
    decimals_allowed = sig_figs - 1 - v.adjusted()
    return Decimal(1).scaleb(-max(0, decimals_allowed))


def quantize_money(value: Number, places: int = 2) -> Decimal:
    """Present an amount with *places* decimals (display only, never sizing)."""
    exp = Decimal(1).scaleb(-places)
    return D(value).quantize(exp)
