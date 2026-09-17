#!/usr/bin/env python3
"""Corvus scene generator — the screenshots in the README, on demand.

Every picture in the README is the real interface driven by real MAVLink. That
is what makes them worth having and what made them expensive: each one needed
an aircraft in a particular state, a flight already flown, a parameter set, a
theme and a page open at the right moment. Rebuilding that by hand for one
changed screenshot is most of an afternoon, and doing it from memory produces a
picture that does not match the others.

This script is that setup, held as data. A *scene* is one picture's whole
state — aircraft, flight, theme, page, staging — and running it stands up a
sandboxed Corvus with a simulated PX4 on the wire, then prints the exact
browser steps that take the shot.

    tools/scene.py list                     what scenes exist, and what each shoots
    tools/scene.py show flight              the recipe, without starting anything
    tools/scene.py run flight               stand the scene up and hold it
    tools/scene.py run map --json           the same, as machine-readable steps
    tools/scene.py run dark --set theme=blue --set path=orbit
    tools/scene.py check                    every scene builds, and renders

Nothing it runs touches the operator's own Corvus: the backend runs against a
sandbox ``HOME``, opts out of the single-instance lock, and takes MAVLink on
14555 rather than the 14550 a real ground station holds. The one thing it
shares is the tile cache, so the map shows the imagery already on disk
(``--isolated-tiles`` if even that is too much).

A note on honesty. The README says the aircraft producing these pictures is
simulated, not airborne. That remains true and must stay written down: this
script makes the simulation reproducible, not real.
"""
from __future__ import annotations

import argparse
import json
import signal
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from scene_kit import flight, library, params  # noqa: E402
from scene_kit import recipe as recipe_mod  # noqa: E402
from scene_kit.runner import (  # noqa: E402
    DEFAULT_HTTP_PORT,
    DEFAULT_MAV_PORT,
    RunnerOptions,
    SceneRunner,
)


def _parse_set(pairs: list[str]) -> dict[str, object]:
    """``--set key=value`` into scene fields, typed by what the value looks like.

    JSON first, so ``--set path_options={"radius":200}`` and ``--set armed=false``
    work; a bare word falls back to a string, which is what ``--set theme=blue``
    wants. Unknown field names are rejected by :meth:`Scene.with_overrides`
    rather than silently ignored — a typo that quietly does nothing is how a
    screenshot comes out wrong for reasons nobody can see.
    """
    out: dict[str, object] = {}
    for pair in pairs:
        if "=" not in pair:
            raise SystemExit(f"--set expects key=value, got {pair!r}")
        key, raw = pair.split("=", 1)
        try:
            value: object = json.loads(raw)
        except json.JSONDecodeError:
            value = raw
        if key in ("home", "viewport") and isinstance(value, list):
            value = tuple(value)
        out[key.strip()] = value
    return out


def cmd_list(args: argparse.Namespace) -> int:
    scenes = sorted(library.SCENES.values(), key=lambda s: s.id)
    if args.json:
        print(json.dumps([{
            "id": s.id, "asset": s.asset, "title": s.title, "theme": s.theme,
            "page": s.page, "view": s.view, "settle": s.settle,
        } for s in scenes], indent=2))
        return 0
    width = max(len(s.id) for s in scenes)
    print(f"{'scene'.ljust(width)}  picture                              what it shows")
    for scene in scenes:
        print(f"{scene.id.ljust(width)}  {scene.asset:36} {scene.title}")
    print()
    print("themes:", ", ".join(library.THEMES))
    print("paths: ", ", ".join(sorted(flight.PATHS)))
    print("frames:", ", ".join(sorted(params.AIRFRAMES)))
    return 0


def cmd_show(args: argparse.Namespace) -> int:
    scene = library.get(args.scene).with_overrides(_parse_set(args.set or []))
    url = f"http://127.0.0.1:{args.port}/"
    if args.json:
        print(json.dumps(recipe_mod.as_json(scene, url), indent=2))
    else:
        print(recipe_mod.as_text(scene, url))
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    scene = library.get(args.scene).with_overrides(_parse_set(args.set or []))
    options = RunnerOptions(
        http_port=args.port,
        mav_port=args.mav_port,
        sandbox=Path(args.sandbox).expanduser() if args.sandbox else None,
        reuse_tiles=not args.isolated_tiles,
        start_backend=not args.no_backend,
        verbose=args.verbose,
        keep_sandbox=args.keep_sandbox,
    )

    stopping = False

    def _stop(*_args: object) -> None:
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)

    runner = SceneRunner(scene, options)
    print(f"scene      {scene.id} -> {scene.asset}")
    try:
        runner.start()
    except RuntimeError as exc:
        print(f"error      {exc}", file=sys.stderr)
        runner.stop()
        return 1

    print()
    if args.json:
        print(json.dumps(runner.recipe_json(), indent=2))
    else:
        print(runner.recipe_text())
    print()
    if scene.notes:
        print(f"note       {scene.notes}")
    deadline = time.monotonic() + args.seconds if args.seconds else None
    print(f"holding    {'for %d s' % args.seconds if args.seconds else 'until Ctrl-C'}"
          f" — the aircraft keeps flying, so the track keeps growing")
    try:
        while not stopping and (deadline is None or time.monotonic() < deadline):
            time.sleep(0.25)
    finally:
        print("\nstopping   …")
        runner.stop()
        print("stopped")
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    """Build every scene, and render its pages through the real backend modules.

    This is the guard that keeps the library honest when the product moves: a
    parameter the Motors page starts reading, or a renamed calibration type,
    shows up here as a page that renders empty rather than as a screenshot
    somebody notices is wrong three months later.
    """
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from corvus import motor_config, rc_config, safety_config, tuning_config

    failures = 0
    for scene in sorted(library.SCENES.values(), key=lambda s: s.id):
        table = scene.build_params()
        model = scene.build_model()
        motors = motor_config.build(table)
        safety = safety_config.build(table, [])
        tuning = tuning_config.build(table)
        rc = rc_config.build(table)

        problems = []
        if motors["rotor_count"] < 1:
            problems.append("no motors")
        if not safety.get("sections"):
            problems.append("safety page empty")
        if not tuning.get("groups") and not tuning.get("sections"):
            problems.append("tuning page empty")
        if scene.view == "control" and not rc.get("sections"):
            problems.append("radio page empty")
        if scene.logs and not scene.build_logs():
            problems.append("no logs")
        if model.duration <= 0:
            problems.append("path has no length")

        status = "ok" if not problems else "FAIL: " + ", ".join(problems)
        failures += bool(problems)
        print(f"{scene.id:12} params={len(table):5} motors={motors['rotor_count']} "
              f"safety={len(safety.get('sections', []))} "
              f"lap={model.duration:6.1f}s  {status}")
    return 1 if failures else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="scene.py", description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    p_list = sub.add_parser("list", help="the scenes, and what each one shoots")
    p_list.add_argument("--json", action="store_true")
    p_list.set_defaults(func=cmd_list)

    p_show = sub.add_parser("show", help="one scene's recipe, without running it")
    p_show.add_argument("scene")
    p_show.add_argument("--port", type=int, default=DEFAULT_HTTP_PORT)
    p_show.add_argument("--set", action="append", metavar="KEY=VALUE")
    p_show.add_argument("--json", action="store_true")
    p_show.set_defaults(func=cmd_show)

    p_run = sub.add_parser("run", help="stand a scene up and hold it")
    p_run.add_argument("scene")
    p_run.add_argument("--port", type=int, default=DEFAULT_HTTP_PORT,
                       help="HTTP port for the sandboxed Corvus")
    p_run.add_argument("--mav-port", type=int, default=DEFAULT_MAV_PORT,
                       help="UDP port the aircraft talks on")
    p_run.add_argument("--set", action="append", metavar="KEY=VALUE",
                       help="override any scene field, e.g. --set theme=blue")
    p_run.add_argument("--seconds", type=float, default=0.0,
                       help="hold for this long, then stop (0 = until Ctrl-C)")
    p_run.add_argument("--sandbox", help="where the throwaway ~/.corvus goes")
    p_run.add_argument("--keep-sandbox", action="store_true",
                       help="do not delete the sandbox on exit")
    p_run.add_argument("--isolated-tiles", action="store_true",
                       help="do not reuse the operator's map tile cache")
    p_run.add_argument("--no-backend", action="store_true",
                       help="only fly the aircraft; talk to a Corvus already running")
    p_run.add_argument("--json", action="store_true",
                       help="print the recipe as JSON, for an agent to follow")
    p_run.add_argument("-v", "--verbose", action="store_true",
                       help="stream the backend's log and the aircraft's events")
    p_run.set_defaults(func=cmd_run)

    p_check = sub.add_parser(
        "check", help="every scene builds, and its pages render from its parameters")
    p_check.set_defaults(func=cmd_check)

    # Line-buffered: a scene is usually started in the background with its
    # output tailed, and a recipe that only appears when the run ends is no
    # recipe at all.
    try:
        sys.stdout.reconfigure(line_buffering=True)
    except AttributeError:  # pragma: no cover - a stdout without reconfigure
        pass

    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except KeyError as exc:
        print(f"error      {exc.args[0] if exc.args else exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
