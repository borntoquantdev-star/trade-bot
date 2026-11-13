"""Which lines of the bot's own code one tick actually ran.

The board draws five cards for the five stages of a decision.  The numbers on
a card say what came *out* of a stage; they never say whether anything ran.
A student watching a quiet bot cannot tell "the strategy looked and chose to
stay out" from "nothing is running at all", and that was the question the
cards answered worst.

So the bot writes it down.  :func:`sys.settrace` hands back a callback for
every line Python executes; this module keeps the ones belonging to this
package and drops the rest, which is nearly all of them.  What ends up in the
audit trail is therefore not an inference from the result - it is the list of
lines the interpreter really stepped through, and how many times each one ran.

Three things keep it cheap enough to leave switched on:

* **Frames from other packages are declined.**  The global callback returns
  ``None`` for them, which tells CPython to send no line events for that frame
  at all.  An HTTP request inside ``requests`` then costs one callback per
  function *call*, not one per line.
* **Nothing is stored twice.**  Lines are counted into a dict, so a loop that
  spins ten thousand times is still one entry.
* **There is a ceiling.**  Past :data:`MAX_LINES` distinct lines it stops
  recording and says so in the row, rather than growing without limit.

It never raises, and it never takes the tracer slot away from a debugger or
from coverage.py: a picture on a card has no business breaking the loop it is
a picture of.
"""

from __future__ import annotations

import sys
from collections.abc import Callable
from pathlib import Path
from types import FrameType
from typing import Any

#: The package whose lines are worth keeping.  Everything else - the standard
#: library, the HTTP client, the exchange SDK - is somebody else's code, and
#: the cards are about this bot's decisions.
PACKAGE = "tradebot"

#: Distinct lines kept per tick.  A tick through the engine, a strategy, the
#: risk gate and a broker is a couple of hundred; the ceiling is here for the
#: pathological case, not the normal one.
MAX_LINES = 600

#: Functions named per tick.  The cards use this list to find the function to
#: show, so it wants to be long enough to hold every stage and no longer.
MAX_CALLS = 80

#: Steps kept **in order**, which is what lets the board walk a card down the
#: path a tick took rather than only colour it in.  A quiet tick is about 140
#: steps; the ceiling is for the tick that spins a loop ten thousand times,
#: where the first thousand steps tell the whole story anyway.
MAX_STEPS = 1200

#: Where ``tradebot/...`` starts, so a filename can be made relative to it.
#: The package may be installed anywhere; the path a card asks for is always
#: written from here.
ROOT = Path(__file__).resolve().parent.parent

#: This file, relative to :data:`ROOT`.  The tracer runs inside the trace, so
#: without this every tick would be mostly a picture of the tracer.
SELF = Path(__file__).resolve().relative_to(ROOT).as_posix()

LocalTrace = Callable[[FrameType, str, Any], Any]


class Trace:
    """Records the package's own lines for as long as it is open.

    Used as a context manager around one tick::

        with Trace() as trace:
            ...
        row["trace"] = trace.as_json()

    ``enabled=False`` makes every method a no-op, so the engine does not grow
    a branch for the switched-off case.
    """

    def __init__(
        self,
        enabled: bool = True,
        max_lines: int = MAX_LINES,
        max_calls: int = MAX_CALLS,
        max_steps: int = MAX_STEPS,
        root: Path | None = None,
    ) -> None:
        self.enabled = enabled
        self.max_lines = max_lines
        self.max_calls = max_calls
        self.max_steps = max_steps
        self.root = (root or ROOT).resolve()
        #: relative file -> line number -> times run
        self.lines: dict[str, dict[int, int]] = {}
        #: the functions entered, in the order they were first entered
        self.calls: list[tuple[str, str, int]] = []
        #: every step, in order, as (index into :attr:`files`, line number)
        self.path: list[tuple[int, int]] = []
        #: the files the path refers to, in the order they were first seen
        self.files: list[str] = []
        self.steps = 0
        #: True once the ceiling was hit and lines started being dropped
        self.full = False
        self._distinct = 0
        self._seen: set[tuple[str, str, int]] = set()
        #: absolute filename -> relative path, or "" for "not ours".  The
        #: callback runs tens of thousands of times a tick; resolving a path
        #: in there would cost more than everything else put together.
        self._known: dict[str, str] = {}
        self._on = False

    # --- the switch ------------------------------------------------------

    def __enter__(self) -> Trace:
        if not self.enabled or sys.gettrace() is not None:
            return self
        self._on = True
        sys.settrace(self._on_call)
        # settrace only reaches frames that start *after* it is installed, and
        # the frame we most want is the one that opened this block - it is
        # already running.  Hooking it by hand is the documented way in, and
        # without it the engine's own `tick` would be the one function missing
        # from a picture of a tick.
        caller = sys._getframe(1)
        self._adopt(caller)
        return self

    def __exit__(self, *_: Any) -> bool:
        self.stop()
        return False

    def stop(self) -> None:
        """Stop recording.

        Safe to call twice, and called before the audit row is written so that
        the writing itself is not part of the picture of the tick.
        """
        if self._on:
            sys.settrace(None)
            self._on = False

    # --- the callbacks ---------------------------------------------------

    def _adopt(self, frame: FrameType) -> None:
        """Start recording a frame that was already running."""
        if not self._note_call(frame):
            return
        frame.f_trace_lines = True
        frame.f_trace = self._on_line  # type: ignore[assignment]

    def _on_call(self, frame: FrameType, event: str, _arg: Any) -> LocalTrace | None:
        if event != "call" or not self._note_call(frame):
            return None
        return self._on_line

    def _on_line(self, frame: FrameType, event: str, _arg: Any) -> LocalTrace | None:
        if event != "line":
            return self._on_line
        name = self._relative(frame.f_code.co_filename)
        if not name:
            return None
        number = frame.f_lineno
        self.steps += 1
        if len(self.path) < self.max_steps:
            self.path.append((self._file_index(name), number))
        else:
            self.full = True
        counts = self.lines.setdefault(name, {})
        if number in counts:
            counts[number] += 1
        elif self._distinct < self.max_lines:
            counts[number] = 1
            self._distinct += 1
        else:
            self.full = True
        return self._on_line

    def _file_index(self, name: str) -> int:
        """Where *name* sits in :attr:`files`, adding it if it is new.

        The path stores an index rather than the name because a path is
        thousands of entries long and a file name is forty characters.
        """
        try:
            return self.files.index(name)
        except ValueError:
            self.files.append(name)
            return len(self.files) - 1

    def _note_call(self, frame: FrameType) -> bool:
        """True when this frame is ours; remembers which function it was."""
        code = frame.f_code
        name = self._relative(code.co_filename)
        if not name:
            return False
        key = (name, code.co_name, code.co_firstlineno)
        if key not in self._seen and len(self.calls) < self.max_calls:
            self._seen.add(key)
            self.calls.append(key)
        return True

    def _relative(self, filename: str) -> str:
        """``tradebot/core/engine.py`` for our files, ``""`` for everyone else."""
        known = self._known.get(filename)
        if known is not None:
            return known
        name = ""
        try:
            relative = Path(filename).resolve().relative_to(self.root)
            text = relative.as_posix()
            if text.startswith(f"{PACKAGE}/") and text != SELF:
                name = text
        except (ValueError, OSError):
            name = ""
        self._known[filename] = name
        return name

    # --- the result ------------------------------------------------------

    def as_json(self) -> dict[str, Any] | None:
        """What goes in the audit row, or ``None`` when nothing was recorded.

        Line numbers become strings, because that is what they would be after
        a round trip through JSON anyway and a reader that had to cope with
        both would be a trap.
        """
        if not self.lines and not self.calls:
            return None
        out: dict[str, Any] = {
            "steps": self.steps,
            "files": list(self.files),
            # In order, so a board can walk a line at a time instead of
            # colouring everything that ran at once. The counts below stay
            # because they survive the ceiling this does not.
            "path": [[index, number] for index, number in self.path],
            "calls": [
                {"file": file, "func": func, "line": line} for file, func, line in self.calls
            ],
            "lines": {
                file: {str(number): count for number, count in sorted(counts.items())}
                for file, counts in self.lines.items()
            },
        }
        if self.full:
            out["full"] = True
        return out


__all__ = ["MAX_CALLS", "MAX_LINES", "MAX_STEPS", "PACKAGE", "ROOT", "SELF", "Trace"]
