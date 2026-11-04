"use strict";
window.Corvus = window.Corvus || {};

/*
  Corvus.units — the display units the operator reads numbers in.

  Everything in the app is SI underneath: the state store, the MAVLink
  bridge, a mission file and every parameter are metres, metres per second
  and degrees Celsius, and they stay that way. This module is only the last
  step before a number reaches the screen (and the first step back when the
  operator types one in), so a unit choice can never change what is sent to
  the aircraft.

  Four independent quantities, because real stations mix them: a glider pilot
  reads feet and knots, a European multirotor crew metres and km/h.

    length       altitude, height, radius, accuracy   m | ft
    distance     route length, distance to a place    km | mi | nmi
    speed        ground and vertical speed            ms | kmh | mph | kn
    temperature  battery temperature                  c | f

  SYSTEMS are presets over all four and nothing more: picking "Imperial" sets
  the four keys, and a station that then changes one of them is simply on no
  preset. The stored value is always the four keys, never the preset name, so
  there is no second truth to disagree with them.

  Parameter pages are deliberately not converted. A parameter is written to
  the firmware in the unit the firmware defines, and a converted figure in an
  editor that writes the raw value would be a trap.

  Persistence mirrors Corvus.theme: localStorage first so the first telemetry
  frame already reads right, the backend config (`ui.units`) second so it
  survives a cache clear. A change is announced as `corvus:unitschange` so
  anything that drew a static caption can redraw it.
*/
Corvus.units = (function () {
  const KEY = "corvus.units";
  const FT_PER_M = 1 / 0.3048;

  const QUANTITIES = {
    length: [
      { id: "m",  label: "Metres (m)", symbol: "m",  perSi: 1 },
      { id: "ft", label: "Feet (ft)",  symbol: "ft", perSi: FT_PER_M },
    ],
    distance: [
      { id: "km",  label: "Kilometres (km)",     symbol: "km", perSi: 1 / 1000,    shortBelow: 1000 },
      { id: "mi",  label: "Miles (mi)",          symbol: "mi", perSi: 1 / 1609.344, shortBelow: 160.9344 },
      { id: "nmi", label: "Nautical miles (NM)", symbol: "NM", perSi: 1 / 1852,    shortBelow: 185.2 },
    ],
    speed: [
      { id: "ms",  label: "Metres per second (m/s)",   symbol: "m/s",  perSi: 1 },
      { id: "kmh", label: "Kilometres per hour (km/h)", symbol: "km/h", perSi: 3.6 },
      { id: "mph", label: "Miles per hour (mph)",       symbol: "mph",  perSi: 3600 / 1609.344 },
      { id: "kn",  label: "Knots (kn)",                 symbol: "kn",   perSi: 3600 / 1852 },
    ],
    temperature: [
      { id: "c", label: "Celsius (°C)",    symbol: "°C" },
      { id: "f", label: "Fahrenheit (°F)", symbol: "°F" },
    ],
  };

  const SYSTEMS = [
    { id: "metric",   label: "Metric",   desc: "m, km, m/s, °C",
      units: { length: "m", distance: "km", speed: "ms", temperature: "c" } },
    { id: "imperial", label: "Imperial", desc: "ft, mi, mph, °F",
      units: { length: "ft", distance: "mi", speed: "mph", temperature: "f" } },
    { id: "aviation", label: "Aviation", desc: "ft, NM, kn, °C",
      units: { length: "ft", distance: "nmi", speed: "kn", temperature: "c" } },
  ];

  const DEFAULTS = Object.freeze(Object.assign({}, SYSTEMS[0].units));

  let prefs = Object.assign({}, DEFAULTS);

  function unitOf(quantity, id) {
    const list = QUANTITIES[quantity] || [];
    return list.find((u) => u.id === id) || list[0];
  }

  /** Keep the four known keys with known values; anything else falls back to
   *  the metric default, so a config from a newer build never leaves a
   *  quantity without a unit. */
  function normalize(raw) {
    const src = (raw && typeof raw === "object") ? raw : {};
    const out = {};
    Object.keys(QUANTITIES).forEach((q) => {
      const ok = QUANTITIES[q].some((u) => u.id === src[q]);
      out[q] = ok ? src[q] : DEFAULTS[q];
    });
    return out;
  }

  /** The preset the current choice matches, or null when it matches none. */
  function systemOf(p) {
    const cur = normalize(p || prefs);
    const hit = SYSTEMS.find((s) => Object.keys(s.units).every((q) => s.units[q] === cur[q]));
    return hit ? hit.id : null;
  }

  /** Apply *next* (a full or partial set of the four keys) and cache it.
   *  Returns the units now in force. */
  function set(next) {
    prefs = normalize(Object.assign({}, prefs, next || {}));
    try { localStorage.setItem(KEY, JSON.stringify(prefs)); } catch (_e) {}
    try {
      window.dispatchEvent(new CustomEvent("corvus:unitschange", { detail: get() }));
    } catch (_e) {}
    return get();
  }

  /** Apply one of SYSTEMS by id. Unknown ids change nothing. */
  function setSystem(id) {
    const s = SYSTEMS.find((x) => x.id === id);
    return s ? set(s.units) : get();
  }

  function get() { return Object.assign({}, prefs); }

  /** Apply the locally cached choice (called before the config fetch lands). */
  function applySaved() {
    let saved = null;
    try { saved = JSON.parse(localStorage.getItem(KEY) || "null"); } catch (_e) {}
    prefs = normalize(saved);
    return get();
  }

  /** The units a backend config asks for, or null when it names none. */
  function fromConfig(cfg) {
    const u = cfg && cfg.ui && cfg.ui.units;
    return (u && typeof u === "object") ? normalize(u) : null;
  }

  // ---- conversions: SI in, display unit out (and back) --------------------

  function length(m) { return Number(m) * unitOf("length", prefs.length).perSi; }
  function lengthToSi(v) { return Number(v) / unitOf("length", prefs.length).perSi; }
  function speed(ms) { return Number(ms) * unitOf("speed", prefs.speed).perSi; }
  function speedToSi(v) { return Number(v) / unitOf("speed", prefs.speed).perSi; }
  function distance(m) { return Number(m) * unitOf("distance", prefs.distance).perSi; }
  function distanceToSi(v) { return Number(v) / unitOf("distance", prefs.distance).perSi; }
  function temperature(c) { return prefs.temperature === "f" ? Number(c) * 9 / 5 + 32 : Number(c); }

  function lengthSymbol() { return unitOf("length", prefs.length).symbol; }
  function distanceSymbol() { return unitOf("distance", prefs.distance).symbol; }
  function speedSymbol() { return unitOf("speed", prefs.speed).symbol; }
  function temperatureSymbol() { return unitOf("temperature", prefs.temperature).symbol; }

  // ---- formatting ----------------------------------------------------------

  function fixed(v, digits, signed) {
    const s = digits > 0 ? v.toFixed(digits) : String(Math.round(v));
    return signed && v >= 0 ? "+" + s : s;
  }

  /** An altitude or other length, e.g. "120 m" / "394 ft". `digits` defaults
   *  to 0, `bare` leaves the symbol off for a caller that prints it apart. */
  function formatLength(m, opts) {
    const o = opts || {};
    const n = Number(m);
    if (!isFinite(n)) return "";
    const s = fixed(length(n), o.digits || 0, o.signed);
    return o.bare ? s : `${s} ${lengthSymbol()}`;
  }

  /** A speed, one decimal unless told otherwise. */
  function formatSpeed(ms, opts) {
    const o = opts || {};
    const n = Number(ms);
    if (!isFinite(n)) return "";
    const s = fixed(speed(n), o.digits == null ? 1 : o.digits, o.signed);
    return o.bare ? s : `${s} ${speedSymbol()}`;
  }

  function formatTemperature(c, opts) {
    const o = opts || {};
    const n = Number(c);
    if (!isFinite(n)) return "";
    return `${fixed(temperature(n), o.digits == null ? 1 : o.digits)} ${temperatureSymbol()}`;
  }

  /**
   * A distance that may be a few metres or many kilometres. Below the unit's
   * `shortBelow` it is printed in the length unit (a 300 m hop is not
   * "0.19 mi"), above it in the distance unit.
   *
   * `coarse` is the place search's reading: rounded to tens below the switch,
   * one decimal up to ten, whole numbers after. Otherwise the mission
   * summary's: whole length units, then two decimals.
   */
  function formatDistance(m, opts) {
    const o = opts || {};
    const n = Number(m);
    if (!isFinite(n)) return "";
    const d = unitOf("distance", prefs.distance);
    const switchAt = o.coarse ? d.shortBelow * 0.95 : d.shortBelow;
    if (n < switchAt) {
      const v = length(n);
      return `${o.coarse ? Math.round(v / 10) * 10 : Math.round(v)} ${lengthSymbol()}`;
    }
    const big = n * d.perSi;
    if (o.coarse) return `${big < 9.95 ? big.toFixed(1) : Math.round(big)} ${d.symbol}`;
    return `${big.toFixed(2)} ${d.symbol}`;
  }

  applySaved();

  return {
    QUANTITIES, SYSTEMS, DEFAULTS,
    get, set, setSystem, systemOf, normalize, applySaved, fromConfig,
    length, lengthToSi, distance, distanceToSi, speed, speedToSi, temperature,
    lengthSymbol, distanceSymbol, speedSymbol, temperatureSymbol,
    formatLength, formatSpeed, formatTemperature, formatDistance,
  };
})();
