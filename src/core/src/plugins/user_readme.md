# Corvus GCS plugins

Drop one folder per plugin into this directory, then press Reload plugins
in Settings > Plugins (or restart Corvus).

    plugins/
      my-plugin/
        plugin.json
        my-plugin.js
        my-plugin.css      (optional)
        lang/de.json       (optional, translations of the plugin's own texts)

`plugin.json` describes the plugin:

    {
      "id": "my-plugin",
      "name": "My Plugin",
      "icon": "puzzle",
      "description": "What it does, one line.",
      "version": "1.0.0",
      "author": "Your name",
      "order": 100,
      "scripts": ["my-plugin.js"],
      "styles": ["my-plugin.css"]
    }

`icon` is a Lucide icon name (https://lucide.dev). `id` defaults to the folder
name; `scripts` defaults to `<id>.js`. `order` decides where the card sits in
the grid (lower is earlier, default 100); ties are broken by name.

Each script registers the plugin when it loads:

    Corvus.plugins.register("my-plugin", {
      name: "My Plugin",
      icon: "puzzle",
      description: "What it does, one line.",
      init: function (containerEl, api) { /* build your UI */ },
      destroy: function (containerEl) { /* tear it down again */ },
    });

More keys are optional. `start: function (api) { ... }` runs once when Corvus
starts (and after Reload plugins), whether or not the plugin is opened (to put
a saved line back on the map, say). `tab: true` lets it have a tab of its own
in the side panel, switched on with "Own tab" in its settings; otherwise it is
a card like any other, so it has to work as both. `options: [...]` gives it
settings, opened from its gear in Settings > Plugins:

    options: [
      { key: "labels", label: "Show labels", type: "toggle", default: true },
      { key: "units", label: "Units", type: "select", default: "m",
        choices: [{ value: "m", label: "Metres" }, { value: "ft", label: "Feet" }] },
    ],
    optionsChanged: function (options, api) { /* follow the new values */ },

The types are toggle, select, text and number; read them with
`api.getOptions()`.

`api.map` draws on the Home map. Lines and areas lie under the flown track:
`api.map.drawLine(key, [[lng, lat], ...], {color, width, dashed})`,
`api.map.drawPolygon(key, [[lng, lat], ...], {color, fillOpacity, width})`
(the area the points span) and `api.map.drawCircle(key, [lng, lat], metres,
opts)`. `api.map.drawText(key, [lng, lat], "text", {color, size, dot})` puts a
label there. `api.map.drawButton(key, [lng, lat], "label", {onClick, title, color, size, disabled})`
puts a clickable button there. `api.map.drawMarker(key, [lng, lat], {icon, label, color, size, selected, draggable, onClick, onDrag, onDragEnd})`
puts a round icon badge there (`icon` is a symbol name from `api.map.icons()`,
such as "tree", or your own SVG path strings on a 24 by 24 grid).
`api.map.pickPoint(onPick, {onCancel})` waits for one click on the map and
`api.map.track()` returns the track flown so far. `api.map.remove(key)`, `api.map.setVisible(key, on)`,
`api.map.fit(coords)` and `api.map.colors` for the colours to offer. The keys
are the plugin's own.

Whatever a plugin saves through `api.saveSettings` is written to
`<id>/config.json` in this folder, one file per plugin and separate from the
application's own config. To set up another computer the same way, copy that
file (or the whole plugin folder) to the same place there and restart. This
works for the plugins that ship with Corvus too: their folder here holds only
the config. Settings > Export and import does the same in one file: switch on a
plugin's Settings, and for a plugin of your own its Files, and an import on
the other computer puts them here.

`api` is documented at the top of `src/js/plugins.js`. The plugins shipped in
the application's own `plugins/` folder are complete worked examples:
`ssh-launcher` for a form, saved settings, a backend call and a terminal,
`vibration` for a live chart on the telemetry stream. Two more are installed
into this folder from their own repositories: the Trajectory Viewer
(https://github.com/M-BSquared/corvus-trajectory-viewer) for reading a file, a start hook and a line on the map,
and the NuttX Console (https://github.com/M-BSquared/corvus-nuttx-console) for a tab of its own and a
stream from `/api/events`. Copy one and start from there.

A plugin may keep its own tests in a `tests/` folder beside its files. Corvus
never loads them; the Plugins page of the guide says how to run them.
