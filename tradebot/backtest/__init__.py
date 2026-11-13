"""Replaying stored candles through the live risk gate.

The gate was written as a pure function so that this package could exist
without a second copy of the trading rules. Nothing here decides whether an
order is allowed or how big it is; it decides only what the market would have
done to an order the gate already approved.
"""

from .report import summary, to_dict, trade_table, write_json
from .result import BacktestResult, EquityPoint, Trade
from .runner import BacktestError, run_backtest

__all__ = [
    "BacktestError",
    "BacktestResult",
    "EquityPoint",
    "Trade",
    "run_backtest",
    "summary",
    "to_dict",
    "trade_table",
    "write_json",
]
