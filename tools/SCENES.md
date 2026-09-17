# Regenerating the screenshots

Every picture in `README.md` is the real interface driven by real MAVLink. That
is what makes them worth having, and it is what made each one expensive: an
aircraft in a particular state, a flight already flown, a parameter set, a
theme, and a page open at the right moment. Rebuilding that by hand for one
changed screenshot is most of an afternoon, and rebuilding it from memory gives
a picture that does not match the others.

`tools/scene.py` holds that setup as data. A **scene** is one picture's whole
state; running it stands up a sandboxed Corvus with a simulated PX4 on the wire
and prints the exact browser steps that take the shot.

```bash
python3 tools/scene.py list
```

```bash
python3 tools/scene.py run flight
```

Then follow the printed recipe in a browser: set the viewport, open the URL,
run the bootstrap snippet, reload, navigate, wait, shoot. `--json` prints the
same recipe as machine-readable steps, which is the form an agent should use.

## What a scene controls

| Field | What it decides |
| --- | --- |
| `theme`, `scale`, `viewport` | the interface's appearance and the frame |
| `page`, `view`, `workspace`, `tab` | what is on screen |
| `hud` | where the floating instrument panel sits |
| `airframe`, `rangefinder`, `optical_flow`, `rc` | what the configuration pages read |
| `param_overrides`, `full_param_table` | any PX4 parameter, and the editor's weight |
| `home`, `path`, `path_options`, `takeoff_time` | the flight, and therefore the flown track |
| `armed`, `mode` | what the top bar and the pages allow |
| `calibration`, `calibration_pause_after` | which calibration runs, and where it stops |
| `logs`, `chatter` | the Analysis page's log card and the console's traffic |
| `settle`, `notes` | how long the picture needs to build, and what to look for |

Airframes: `quad_x`, `quad_x_heavy`, `hexa_x`, `octo_x`, `vtol_quad`,
`fixed_wing`. Paths: `hover`, `orbit`, `survey`, `figure_eight`, `out_and_back`.
Themes: the six in `src/css/themes.css`.

## One-off variations

Any field can be overridden without touching the library:

```bash
python3 tools/scene.py run motors --set airframe=hexa_x --set theme=blue
```

```bash
python3 tools/scene.py run map --set path=figure_eight --set 'path_options={"size":200,"alt":70}'
```

```bash
python3 tools/scene.py run calibration --set calibration=compass --set calibration_pause_after=5
```

Values are parsed as JSON where they can be, so numbers, booleans, lists and
objects all work; a bare word stays a string. An unknown field name is an
error, not a silent no-op.

## A new picture

Add a `Scene` to `tools/scene_kit/library.py`. Nothing in the runner is
special-cased for the built-in scenes — a new one uses the same fields — and
`python3 tools/scene.py check` will build it, render its pages through the real
backend modules, and say so if a page comes out empty.

## What it does to your machine

Nothing that outlives the run:

* The backend is the real `serve.py`, started with `HOME` pointed at a sandbox,
  so `~/.corvus` — config, tlogs, downloaded logs, exported parameters — is a
  throwaway copy and not yours. It is deleted on exit unless you pass
  `--keep-sandbox` or name your own `--sandbox`.
* `CORVUS_ALLOW_MULTI=1` is set for the sandbox only, so a scene can run while
  your own Corvus is open.
* MAVLink is on **14555**, not 14550, so a scene never takes the socket a real
  ground station or a SITL is using.
* HTTP is on **8777**. `--port` moves it; `--no-backend` skips starting one
  altogether and just puts an aircraft on the wire for a Corvus you already
  have running.
* The map tile cache is shared with your real one, read-only in practice, so
  the map shows the imagery already on disk. `--isolated-tiles` opts out, at
  the cost of an empty map.
* The update check is off in the sandbox config: a release dialog across the
  frame would ruin every picture at once.

## What is real and what is not

Real: the application, the protocol, the parsing, the pages, the flown track,
the parameter protocol, the command acknowledgements, the calibration
transcript and the log list.

Not real: the aircraft. It is a model, not a flight simulator — attitude comes
from the path's geometry, and a downloaded log's bytes are filler. The README
says the aircraft producing these pictures is simulated rather than airborne,
and that sentence has to stay true.
