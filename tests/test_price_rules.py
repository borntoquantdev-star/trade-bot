"""Hyperliquid's two price rules, which a single tick size cannot express.

From the API docs, for perpetuals:

* prices carry at most 5 significant figures, except that integer prices are
  always accepted however many digits they have, and
* prices carry at most ``6 - szDecimals`` decimal places.

Both apply at once, and the stricter one wins. Getting this wrong means orders
the exchange silently rejects, so it is pinned down here with the examples
from the documentation itself.
"""

import pytest

from tradebot.money import D, significant_figure_step
from tradebot.types import InstrumentSpec

# BTC: szDecimals 5 -> 1 price decimal, 1e-5 lots
BTC = InstrumentSpec.build("BTC", tick_size="0.1", lot_step="0.00001", min_notional="10")
# ETH: szDecimals 4 -> 2 price decimals
ETH = InstrumentSpec.build("ETH", tick_size="0.01", lot_step="0.0001", min_notional="10")
# A venue with a plain fixed tick and no significant-figure rule.
PLAIN = InstrumentSpec.build("PLAIN", "0.01", "0.001", "10", price_sig_figs=0)


@pytest.mark.parametrize(
    ("value", "sig_figs", "step"),
    [
        ("1234.5", 5, "0.1"),
        ("1234.56", 5, "0.1"),
        ("0.012345", 5, "0.000001"),
        ("123456", 5, "1"),
        ("99999", 5, "1"),
        ("9.9999", 5, "0.0001"),
        ("100", 3, "1"),
    ],
)
def test_the_significant_figure_step_follows_the_magnitude(value, sig_figs, step):
    assert significant_figure_step(value, sig_figs) == D(step)


def test_zero_and_bad_input():
    assert significant_figure_step(0, 5) == D(1)
    with pytest.raises(ValueError):
        significant_figure_step("1", 0)


# --- the documented examples ------------------------------------------------


def test_five_significant_figures_is_the_cap():
    """The docs: 1234.5 is valid, 1234.56 is not."""
    assert BTC.round_price("1234.56", favour="down") == D("1234.5")
    assert BTC.round_price("1234.56", favour="up") == D("1234.6")


def test_integer_prices_are_always_accepted():
    """The docs: 123456 is valid, 12345.6 is not."""
    assert BTC.round_price("123456.7", favour="down") == D("123456")
    assert BTC.round_price("123456.7", favour="up") == D("123457")


def test_the_decimal_cap_also_applies():
    """ETH has szDecimals 4, so at most 6 - 4 = 2 price decimals."""
    assert ETH.round_price("3000.567", favour="down") == D("3000.5")  # 5 figures
    assert ETH.round_price("1.23456", favour="down") == D("1.23")  # 2 decimals


def test_the_documented_five_decimal_example():
    """The docs: with szDecimals = 1, 0.01234 is valid and 0.012345 is not."""
    low_precision = InstrumentSpec.build(
        "DOGE", tick_size="0.00001", lot_step="0.1", min_notional="10"
    )
    assert low_precision.round_price("0.012345", favour="down") == D("0.01234")
    assert low_precision.round_price("0.012345", favour="up") == D("0.01235")


def test_the_stricter_of_the_two_rules_wins():
    # Significant figures allow 0.012345 (5 figures), but BTC's decimal cap is
    # one place, so the tick size is what bites.
    assert BTC.round_price("0.012345", favour="down") == D("0.0")
    # ETH allows two decimals.
    assert ETH.round_price("0.012345", favour="down") == D("0.01")


def test_a_venue_without_a_significant_figure_rule_just_uses_the_tick():
    assert PLAIN.round_price("1234.567", favour="down") == D("1234.56")
    assert PLAIN.round_price("1234.561", favour="up") == D("1234.57")


# --- direction is always protective -----------------------------------------


@pytest.mark.parametrize("price", ["100000.07", "3000.567", "1234.56", "99999.9"])
def test_rounding_down_never_raises_the_price(price):
    assert BTC.round_price(price, favour="down") <= D(price)


@pytest.mark.parametrize("price", ["100000.07", "3000.567", "1234.56", "99999.9"])
def test_rounding_up_never_lowers_the_price(price):
    assert BTC.round_price(price, favour="up") >= D(price)


def test_an_unknown_direction_is_a_programming_error():
    with pytest.raises(ValueError):
        BTC.round_price("100", favour="nearest")


# --- quantities --------------------------------------------------------------


def test_quantities_floor_to_the_lot_step():
    assert BTC.round_qty("0.123456789") == D("0.12345")
    assert ETH.round_qty("0.00019") == D("0.0001")


def test_a_quantity_below_one_lot_step_becomes_zero():
    """The gate relies on this to refuse an order rather than send dust."""
    assert BTC.round_qty("0.000009") == D(0)


def test_the_price_step_is_reported_for_a_given_magnitude():
    assert BTC.price_step("100000") == D(1)  # integers only up here
    assert BTC.price_step("1000") == D("0.1")  # the tick is the limit
    assert ETH.price_step("1000") == D("0.1")  # 5 figures beats the 0.01 tick
