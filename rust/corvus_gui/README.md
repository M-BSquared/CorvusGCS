# corvus_gui

The Corvus GCS desktop interface as a native Rust app (egui/eframe). It is
replacing the web interface in `src/`, page by page, and later becomes the
Android app as well.

It is a client of the backend's HTTP/SSE API, the same one the web interface
uses, so the backend does not change for it:

```bash
cargo run --release                                   # start serve.py here and show it
cargo run --release -- --server http://10.0.0.5:8080  # a backend elsewhere
cargo run --release -- --root DIR --python PATH       # where serve.py is, what runs it
cargo run --release -- --page safety                  # open on a page (setup, parameters, safety)
```

Locally, `serve.py` runs as a child on a free port. Every way out (window
closed, SIGINT, SIGTERM) sends it SIGTERM, which runs its own teardown, and
SIGKILL if it is still there after 8 s. If the window does not close within
10 s of a signal, the child is killed and the process exits.

## What is here

| | |
| --- | --- |
| `src/backend.rs` | starts and stops `serve.py` |
| `src/http.rs` | a small HTTP/1.1 client, with a read timeout per read so a stream reader can stop |
| `src/sse.rs` | server-sent events |
| `src/telemetry.rs` | the `/api/telemetry` reader thread and the latest state |
| `src/units.rs` | display units from the config's `ui.units`, rounded the way the web interface rounds |
| `src/topbar.rs` | the status bar, word for word the web interface's (`src/js/topbar.js`) |
| `src/map.rs` | the map (walkers) on the backend's tile proxy, vehicle and home markers |
| `src/track.rs` | the flown track, recorded and smoothed as `src/js/map.js` does |
| `src/hud.rs` | compass, attitude indicator and readouts (`src/js/instruments.js`) |
| `src/flight.rs` | the flight bar, takeoff card and mode picker (`src/js/app.js`) |
| `src/style.rs` | the design tokens of the six themes (light, light-orange, green, blue, pink, orange) and the type |
| `src/setup.rs` | the Setup page: head, vehicle card, the tile grid and the sub-pages (`src/js/setup.js`) |
| `src/params.rs` | the Parameters page: download, search, filter, defaults, edit and apply (`src/js/setup-parameters.js`) |
| `src/safety.rs` | the Safety & Sensors page: the backend's description in two stacks, the landing preset, the sensor tiles (`src/js/setup-safety.js`) |
| `src/schema_form.rs` | the schema-driven form the backend describes (enum, number, sign, bitmask), shared with the Motors page to come (`src/js/setup-shared.js`) |
| `src/pagekit.rs` | page furniture: header, section title, tile, back button, segmented control |
| `src/notify.rs` | the notification board, its read and dismiss rules, the command dedupe and the toasts (`src/js/topbar.js`) |
| `src/notify_ui.rs` | the NOTIFICATIONS pill, the board and the toast stack |
| `src/frost.rs` | the frosted glass under every `.glass` surface |
| `src/fonts.rs`, `src/icons.rs`, `build.rs` | Inter, JetBrains Mono and the Lucide icons, from the web's own files |

## Looking like the web interface

Every measure is taken from the CSS with one CSS pixel as one egui point,
so a block here is the size, weight and colour of the element it copies.
The fonts are the web's WOFF2 files and the icons are cut out of its
`lucide.min.js`, both at build time, so there is no second copy of either.
The glass panels are frosted as the CSS frosts them (`backdrop-filter:
blur(24px) saturate(180%)`, then 60 % white, 55 % on the map rail): a paint
callback on the OpenGL renderer copies what is already drawn under the
panel, blurs and saturates it inside the panel's rounded corners
(`src/frost.rs`), and the fill paints over it. The copy is taken before the
panel's shadow, because a browser draws a box shadow outside the box only.

The theme is the config's `theme.name`, as in the web interface, and is read
once at start; the palettes are `themes.css` value for value (the dark
themes share one base, blue and pink and orange change the accent family,
blue also the navigation colour). The frost saturates 190 % and brightens
10 % on the dark themes, and the logo is the white cut there.

The window title comes from `GET /api/version`; the crate carries no
version of its own.

## Toolchain

eframe 0.37 needs Rust 1.95 or newer. `rust-toolchain.toml` pins 1.99 for
this folder only, so the default toolchain and `rust/corvus_link` are left
alone; rustup installs it on first use.

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Every rule a block of the bar follows has a test with the web interface's
expected text, including `toFixed` and `Math.round` rounding.
