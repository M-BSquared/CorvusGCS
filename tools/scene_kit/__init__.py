"""Corvus screenshot scenes — the state behind the pictures in the README.

See ``tools/scene.py`` for the command line, and ``tools/SCENES.md`` for how a
picture is regenerated. The modules split along the seams the work has:

* :mod:`params`   the PX4 parameter table every configuration page is drawn from
* :mod:`flight`   the path, and where the aircraft is on it at a given moment
* :mod:`vehicle`  a PX4 autopilot on a UDP socket, simulated well enough to photograph
* :mod:`library`  the scenes themselves, one per picture
* :mod:`recipe`   a scene turned into the browser steps that photograph it
* :mod:`runner`   a sandboxed Corvus plus an aircraft, and the URL between them
"""
from __future__ import annotations

from .library import SCENES, Scene, get  # noqa: F401
from .runner import RunnerOptions, SceneRunner  # noqa: F401

__all__ = ["SCENES", "Scene", "get", "SceneRunner", "RunnerOptions"]
