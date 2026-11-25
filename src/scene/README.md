# The scene kit

Every picture in the README and on the website is the real program in front of
real MAVLink. `corvus-scene` makes them again, all in the same way: the same
flight over the same field, the same size of everything, the instruments at the
bottom right. It never enters an artifact.

```bash
cargo run -p corvus_scene -- list
```

```bash
cargo run -p corvus_scene -- shoot flight --publish
```

```bash
cargo run -p corvus_scene -- run map --set theme=blue
```

* `list` names the scenes and the pictures they make.
* `shoot` takes the named pictures, or all of them, into `build/scenes`, and with
  `--publish` puts them where the README (`assets/`) and the website
  (`docs/assets/images`, with an 800 px copy) have them. Look at them first:
  `publish` does that half alone.
* `run` stands one scene up and holds it until its window is closed.
* `--set key=value` changes a field for this run (a number, `true`, a list or an
  object as JSON, or a word), for example `--set airframe=hexa_x`,
  `--set path=figure_eight`, `--set 'path_options={"alt": 70}'`.

A run builds the program and its plugins first. The window opens on the screen;
leave it alone until it closes.

## What a scene says

`data/scenes.json`, one object each. What it leaves out is as `defaults` in
`src/scene.rs` has it.

| Field | What it decides |
| --- | --- |
| `theme`, `language`, `ui` | the appearance, the language, other keys of the config's `ui` |
| `page`, `panel`, `plugin` | the page (as `--page` names it), the side panel's tab or `collapsed`, the plugin that is opened |
| `airframe`, `sensors`, `full_table`, `params`, `transmitter` | the parameters the aircraft answers with, and its sticks |
| `home`, `path`, `path_options`, `takeoff`, `armed`, `mode` | the flight (`parked`, `hover`, `orbit`, `survey`, `figure_eight`, `swoop`, `slalom`) |
| `fleet`, `vehicle_name` | more vehicles, each on a link of its own |
| `geofence`, `plugin_settings`, `terrain`, `three_d` | the area, what plugins have saved, the elevation model, the 3D view |
| `logs`, `review_log`, `pause_after` | the logs on the card, one of them downloaded, the side a calibration waits at |
| `steps` | what is pressed and when, in points of the frame (`src/app/scene.rs`) |
| `settle` | the seconds the picture needs to build |
| `web`, `readme` | the picture's name on the website and in `assets/` |
| `skip` | why the scene is not made |

The interface is laid out as 1600 by 1000 points whatever the screen has room
for (`frame`): the window is zoomed to it, and the picture is scaled to 1600
pixels. A Retina screen gives it 2560 pixels to come from. `--window` sets the
window's own size.

## What is real and what is not

Real: the program, the protocol, the pages, the flown track, the parameters, the
command answers, the calibration, the logs. Not real: the aircraft. It is a
model and no flight simulator. In the AI Chat picture a script stands in for the
language model; the tools it asks for are the station's own. The website says
both, and has to.

## What it does to your computer

The program runs with a sandbox home (`/tmp/corvus-scene-<id>`, removed
afterwards), MAVLink on 14571 and up and HTTP on 8777. Only the map tiles on
disk are shared; `--isolated-tiles` leaves them alone too. The 3D pictures of the
field use a 20 cm surface model imported under Settings, Map; without it they
fall back to the free 30 m model.
