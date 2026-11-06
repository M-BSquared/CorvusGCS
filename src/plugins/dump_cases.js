#!/usr/bin/env node
"use strict";
/**
 * Writes what the web plugins' pure functions answer for fixed inputs, so the
 * Rust crates can be held to those answers (`model_cases.json` beside each
 * crate's `model.rs`). The web code is loaded as the browser loads it, from
 * python/plugins, and not changed.
 *
 *     node src/plugins/dump_cases.js            # rewrite both files
 *     node src/plugins/dump_cases.js --check    # fail when a file is stale
 */
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const ROOT = path.resolve(__dirname, "..", "..");

function load(plugin, name) {
  global.window = global;
  global.Corvus = {};
  global.Corvus.plugins = { register() {} };
  const file = path.join(ROOT, "python", "plugins", plugin, plugin + ".js");
  vm.runInThisContext(fs.readFileSync(file, "utf8"), { filename: file });
  return global.Corvus[name];
}

function run(lib, table) {
  const out = {};
  for (const [fn, inputs] of Object.entries(table)) {
    out[fn] = inputs.map((args) => ({ in: args, out: lib[fn](...args) }));
  }
  return out;
}

// ---- SSH Launcher --------------------------------------------------------
const ssh = load("ssh-launcher", "pluginSshLauncher");
const sshCases = run(ssh, {
  coerceButton: [
    [{ id: "a", command: "./start.sh", label: "Start" }],
    [{ id: "b", command: "  ls -la  ", label: "  ", directory: " ~/mission ", connection: " pi ", mode: "background", restart: true }],
    [{ id: "c", command: "x", detach: true }],
    [{ id: "d", command: "x", detach: false, restart: true }],
    [{ id: "e", command: "x", mode: "weird", detach: true }],
    [{ id: "f", command: "   " }],
    [{ id: "g", command: 42, label: 7 }],
    [{ command: "y", restart: "yes" }].map((b) => Object.assign({ id: "h" }, b)),
    [null],
    ["text"],
  ],
  normalizeButtons: [
    [{ buttons: [{ id: "a", command: "a" }, { id: "b", command: "" }, { id: "c", command: "c", mode: "background" }] }],
    [{ connection: "pi", directory: "/m", command: "./go.sh", detach: true, id: "one" }],
    [{}],
    [null],
    [{ buttons: Array.from({ length: 15 }, (_, i) => ({ id: "k" + i, command: "c" + i })) }],
  ],
  remoteLine: [
    [{ directory: "/home/p'x", command: " ./a.sh " }],
    [{ directory: "", command: "x" }],
    [{ directory: "/d", command: "  " }],
    [{ directory: "-rf", command: "ls" }],
    [null],
  ],
  previewLine: [
    [{ connection: "pi", directory: "/m", command: "./s.sh", mode: "background" }],
    [{ connection: "", directory: "", command: "./s.sh", mode: "terminal" }],
    [{ connection: "pi", directory: "/m", command: "./s.sh", mode: "terminal" }],
    [{ connection: "pi", directory: "/m", command: " " }],
    [{ connection: " pi ", directory: "", command: "a b", detach: true }],
  ],
  derivedName: [
    [{ host: " 10.0.0.7 ", username: "pilot" }],
    [{ host: "h", username: "" }],
    [{ host: "", username: "u" }],
  ],
  newConnectionError: [
    [{ host: "", port: "22", name: "", username: "" }, []],
    [{ host: "h", port: "70000", name: "", username: "" }, []],
    [{ host: "h", port: "2.5", name: "", username: "" }, []],
    [{ host: "h", port: "abc", name: "", username: "" }, []],
    [{ host: "h", port: "22", name: "pi", username: "" }, ["pi"]],
    [{ host: "h", port: "", name: "", username: "u" }, ["u@h"]],
    [{ host: "h", port: "22", name: "", username: "u" }, []],
  ],
  newConnectionBody: [
    [{ host: " h ", port: "2222", name: "", username: " u ", password: " p ", key_path: " /k " }],
    [{ host: "h", port: "0", name: "n", username: "", password: "", key_path: "" }],
    [{ host: "h", port: "x", name: "n", username: "", password: "", key_path: "" }],
  ],
  resultSummary: [
    [{ ok: true, stdout: "pid 123\nmore", pid: 123 }],
    [{ ok: true }],
    [{ ok: false, stderr: "boom\nsecond" }],
    [{ ok: false, error: "no route" }],
    [{ ok: false }],
    [null],
  ],
  missingConnections: [
    [[{ connection: "a" }, { connection: "b" }, { connection: "a" }, { connection: "" }], [{ name: "b" }]],
    [[], []],
    [[{ connection: " x " }], null],
  ],
  restarts: [
    [{ restart: true, mode: "terminal" }],
    [{ restart: true, mode: "background" }],
    [{ restart: true, detach: true }],
    [{ restart: false }],
    [null],
  ],
  coerceMode: [
    [{ mode: "background" }], [{ mode: "terminal" }], [{ detach: true }], [{ detach: false }], [{ mode: "x", detach: true }], [{}],
  ],
});

// ---- Map Entities --------------------------------------------------------
const me = load("map-entities", "pluginMapEntities");
const gate = { lng: 11.5, lat: 48.1, lng2: 11.5004, lat2: 48.1, base: 2, height: 5 };
const meCases = run(me, {
  parseDegrees: [["48.1234", 90], [" -11,5 ", 180], ["", 90], ["91", 90], ["abc", 90], ["1e1", 90], ["+5", 90], ["0x10", 90]],
  parseCoordinatePair: [["48.1234, 11.5678"], ["48.1234 11.5678"], ["48,1234; 11,5678"], ["48.1"], ["1,2,3"], [""], ["91, 11"], ["48.1; 181"]],
  distanceM: [[[11.5, 48.1], [11.5, 48.2]], [[11.5, 48.1], [11.6, 48.1]], [[0, 0], [0, 0]]],
  bearingDeg: [[[11.5, 48.1], [11.5, 48.2]], [[11.5, 48.1], [11.6, 48.1]], [[11.5, 48.1], [11.4, 48.0]], [[0, 0], [-1, 1]]],
  gateCrossing: [
    [gate, { lng: 11.5002, lat: 48.0999, alt: 3 }, { lng: 11.5002, lat: 48.1001, alt: 5 }],
    [gate, { lng: 11.5002, lat: 48.1001, alt: 3 }, { lng: 11.5002, lat: 48.0999, alt: 9 }],
    [gate, { lng: 11.5010, lat: 48.0999, alt: 3 }, { lng: 11.5010, lat: 48.1001, alt: 5 }],
    [gate, { lng: 11.5002, lat: 48.0999, alt: 3 }, { lng: 11.5002, lat: 48.1, alt: 4 }],
    [gate, { lng: 11.5002, lat: 48.0990, alt: 3 }, { lng: 11.5002, lat: 48.0995, alt: 5 }],
  ],
  gateVerdict: [
    [gate, { alt: 3 }], [gate, { alt: 8 }], [gate, { alt: 1 }], [gate, { alt: 2 }], [gate, { alt: 7 }],
  ],
  sectorAngle: [
    [{ kind: "radar", angle: 90 }], [{ kind: "tree", angle: 90 }], [{ kind: "radar" }], [{ kind: "radar", angle: 0 }], [{ kind: "radar", angle: 400 }],
  ],
  inSector: [
    [{ kind: "radar", angle: 90, bearing: 0 }, [0, 10]],
    [{ kind: "radar", angle: 90, bearing: 0 }, [10, 0]],
    [{ kind: "radar", angle: 90, bearing: 90 }, [10, 0]],
    [{ kind: "radar", angle: 360, bearing: 0 }, [-5, -5]],
    [{ kind: "radar", angle: 90, bearing: 350 }, [-3, 10]],
    [{ kind: "tree", angle: 10, bearing: 0 }, [-3, -10]],
    [{ kind: "radar", angle: 90, bearing: 0 }, [0, 0]],
  ],
  sectorRing: [
    [{ kind: "radar", lng: 11.5, lat: 48.1, radius: 100, angle: 90, bearing: 45 }],
    [{ kind: "radar", lng: 11.5, lat: 48.1, radius: 100, angle: 360, bearing: 0 }],
    [{ kind: "tree", lng: 11.5, lat: 48.1, radius: 3, angle: 90, bearing: 45 }],
  ],
  normalizeEntity: [
    [{ id: "r1", kind: "radar", name: " Radar  1 ", lng: 11.5, lat: 48.1, radius: 250.5, color: "#ff0000", note: " hi ", alert: 0, angle: 90, bearing: -90 }],
    [{ id: "g1", kind: "gate", lng: 11.5, lat: 48.1, lng2: 11.51, lat2: 48.1, base: -5, height: 99999 }],
    [{ id: "t1", kind: "tree", lng: 11.5, lat: 48.1, radius: 4, height: 12 }],
    [{ id: "bad", kind: "nonsense", lng: 11.5, lat: 48.1 }],
    [{ id: "x", kind: "radar", lng: 400, lat: 48.1 }],
    [{ id: "g2", kind: "gate", lng: 11.5, lat: 48.1 }],
    [{ id: "p1", kind: "person", lng: 11.5, lat: 48.1, color: "zzz", name: "" }],
    [{ id: "f1", kind: "fire", lng: 11.5, lat: 48.1, radius: -5, angle: 500 }],
    ["text"],
  ],
});

// Entities come back with fields that depend on the web's tables; keep what
// the native side also holds.
function slim(e) {
  if (!e) return e;
  const keep = ["id", "kind", "name", "color", "lng", "lat", "note", "lng2", "lat2", "base", "height", "radius", "alert", "angle", "bearing"];
  const o = {};
  for (const k of keep) if (k in e) o[k] = e[k];
  return o;
}
meCases.normalizeEntity = meCases.normalizeEntity.map((c) => ({ in: c.in, out: slim(c.out) }));

// ---- Vibration Monitor ---------------------------------------------------
const vib = load("vibration", "pluginVibration");
const vibSteps = [
  [{ vibration_x: 0.1, vibration_y: 0.2, vibration_z: 0.3 }, 0],
  [{ vibration_x: 0.1, vibration_y: 0.2, vibration_z: 0.3 }, 1],
  [{ vibration_x: 0.1, vibration_y: 0.2, vibration_z: 0.9 }, 2],
  [{ vibration_x: "0.5", vibration_y: true, vibration_z: null }, 3],
  [{ vibration_x: "x", vibration_y: undefined }, 4],
  [{}, 5],
  [{ vibration_x: "  7 ", vibration_y: "", vibration_z: "1e2" }, 6],
  [{ vibration_x: NaN }, 7],
];
const vibBuf = { t: [], vx: [], vy: [], vz: [] };
const vibCases = { updateBuffer: vibSteps.map(([state, t]) => {
  const added = vib.updateBuffer(vibBuf, state, t);
  return { in: [state, t], out: { added, buffer: JSON.parse(JSON.stringify(vibBuf)) } };
}) };
// A long run: the window keeps the last MAX_POINTS.
const longBuf = { t: [], vx: [], vy: [], vz: [] };
for (let i = 0; i < vib.MAX_POINTS + 25; i++) vib.updateBuffer(longBuf, { vibration_x: i, vibration_y: 0, vibration_z: 0 }, i);
vibCases.window = { max: vib.MAX_POINTS, first: longBuf.vx[0], length: longBuf.vx.length, last: longBuf.vx[longBuf.vx.length - 1] };

const files = {
  "src/plugins/vibration/src/model_cases.json": vibCases,
  "src/plugins/ssh-launcher/src/model_cases.json": sshCases,
  "src/plugins/map-entities/src/model_cases.json": meCases,
};
let stale = false;
for (const [rel, data] of Object.entries(files)) {
  const text = JSON.stringify(data, (k, v) => (v === undefined ? null : v), 1) + "\n";
  const file = path.join(ROOT, rel);
  if (process.argv.includes("--check")) {
    if (!fs.existsSync(file) || fs.readFileSync(file, "utf8") !== text) {
      console.error(rel + " is not what the web code answers now");
      stale = true;
    }
  } else {
    fs.writeFileSync(file, text);
    console.log("wrote " + rel);
  }
}
process.exit(stale ? 1 : 0);
