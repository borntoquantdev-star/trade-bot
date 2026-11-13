"""Market history: downloading it once, and reading it back offline.

:mod:`tradebot.data.store` owns the files on disk; :mod:`tradebot.data.fetch`
fills them from a broker.  Neither knows anything about strategies or risk -
they deal in :class:`~tradebot.types.Candle` values and nothing else.
"""

from .store import CandleStore, Coverage, StoreError, closes, find_gaps

__all__ = ["CandleStore", "Coverage", "StoreError", "closes", "find_gaps"]
