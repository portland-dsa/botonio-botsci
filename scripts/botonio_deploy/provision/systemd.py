"""override.conf render.

Renders each instance's ``override.conf`` from the bundled template, filling the managed
``Environment=`` lines from the non-secret values in the encrypted file (the
``*EnvironmentValues`` allowlists in ``defs``).
"""

from __future__ import annotations

from typing import Optional, Set

from ..defs import Targets


def write_overrides(targets: Optional[Set[Targets]] = None) -> None:
    raise NotImplementedError("override.conf render")
