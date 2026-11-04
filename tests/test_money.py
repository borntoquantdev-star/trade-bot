"""Money and rounding never go through binary floats."""

from decimal import Decimal

import pytest

from tradebot.money import (
    D,
    ceil_to_step,
    floor_to_step,
    pct,
    quantize_money,
    round_to_tick,
)


def test_float_inputs_do_not_bring_binary_error_with_them():
    assert D(0.1) + D(0.2) == D("0.3")
    assert D(0.1) == Decimal("0.1")


def test_reads_ints_strings_and_decimals():
    assert D(5) == Decimal(5)
    assert D("2.50") == Decimal("2.50")
    assert D(Decimal("7")) == Decimal(7)


def test_rejects_nonsense():
    with pytest.raises(ValueError):
        D("not a number")
    with pytest.raises(ValueError):
        D(None)


def test_percent_becomes_a_fraction():
    assert pct(1) == D("0.01")
    assert pct("2.5") == D("0.025")


@pytest.mark.parametrize(
    ("value", "step", "expected"),
    [
        ("0.0127", "0.001", "0.012"),
        ("0.012", "0.001", "0.012"),
        ("0.0009", "0.001", "0"),
        ("17", "5", "15"),
    ],
)
def test_floor_to_step_always_rounds_down(value, step, expected):
    assert floor_to_step(value, step) == D(expected)


def test_ceil_to_step_always_rounds_up():
    assert ceil_to_step("0.0121", "0.001") == D("0.013")
    assert ceil_to_step("0.012", "0.001") == D("0.012")


def test_a_step_of_zero_is_a_programming_error():
    with pytest.raises(ValueError):
        floor_to_step("1", "0")
    with pytest.raises(ValueError):
        ceil_to_step("1", "-1")


def test_round_to_tick_picks_the_protective_direction():
    assert round_to_tick("100100.07", "0.1", favour="down") == D("100100.0")
    assert round_to_tick("99899.93", "0.1", favour="up") == D("99900.0")
    with pytest.raises(ValueError):
        round_to_tick("1", "0.1", favour="nearest")


def test_quantize_money_is_for_display_only():
    assert quantize_money("1234.5678") == D("1234.57")
    assert quantize_money("1234.5678", 4) == D("1234.5678")
