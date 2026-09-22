"use strict";

/*
  Corvus.mapSearch — the place search, for every map in the application.

  The one thing a map cannot give you is a way to get somewhere by name.
  Panning from this site to the next one at planning zoom is minutes of
  dragging, and the operator knows the airfield's name, not its decimals.

  This lived inside mission.js and existed on the Mission map alone, which
  made "go to this place" a thing the planner could do and the Home map could
  not — on two screens showing the same ground, in the same corner, under the
  same rail. It is one component now, mounted by both, so the box behaves
  identically wherever it is reached and a change to it is a change to both.

  Two halves, and the split is the offline contract:

    * A COORDINATE PAIR is parsed here, in the browser, in every notation a
      briefing writes one in — decimal, degrees/minutes, degrees/minutes/
      seconds, hemisphere letters in front or behind, German decimal commas.
      It answers instantly and it answers on a laptop that has never seen the
      internet, which is the case this application is built for.
    * A NAME goes to /api/geocode, which is a thin proxy over Nominatim. That
      needs the network, and when there is none the box says so in a sentence
      rather than failing silently.

  Downloaded areas are matched locally too, and listed first. An operator who
  pulled tiles for "Manching" before leaving has already named the place they
  are going to, and that name is on the laptop.

  create(spec) returns a handle; spec is:

    container   the map wrapper the control is appended to
    map()       the MapLibre map, read fresh (the planner's dies with the page)
    regions()   the downloaded areas, for the offline name match
    onGo(row)   optional — the host's answer to "the operator chose a place".
                The Home map stops following the aircraft and records the move
                as an aim; the planner records the aim alone.
*/
window.Corvus = window.Corvus || {};
Corvus.mapSearch = (function () {

  const DEBOUNCE_MS = 350;
  const RESULT_ZOOM = 15;
  // A named place is a point plus a box; framing the box of a whole country
  // would leave the planner looking at nothing it can draw on.
  const MAX_ZOOM = 16;
  const MAX_ROWS = 7;
  const EARTH_R_M = 6371008.8;

  // What each sort of row is drawn with. A coordinate pair is a position, a
  // downloaded area is a box of tiles, and anything else came off the
  // geocoder and is a place on the ground.
  const KIND_ICON = {
    coordinates: "crosshair",
    "downloaded area": "frame",
  };

  // =====================================================================
  // Pure: the offline half
  // =====================================================================

  /** Degrees from one group of numbers: [d], [d, m] or [d, m, s]. */
  function dmsToDegrees(parts) {
    const degrees = Math.abs(parts[0]);
    const minutes = parts.length > 1 ? parts[1] : 0;
    const seconds = parts.length > 2 ? parts[2] : 0;
    if (minutes < 0 || minutes >= 60 || seconds < 0 || seconds >= 60) return null;
    const value = degrees + minutes / 60 + seconds / 3600;
    return parts[0] < 0 || Object.is(parts[0], -0) ? -value : value;
  }

  /** One side of a pair, tokenised: [{nums, hemi}]. A hemisphere letter ends
   *  the group it belongs to, which is what lets "N48 E11" and "48N 11E" be
   *  the same two numbers. */
  function coordinateGroups(text) {
    const groups = [];
    let current = { nums: [], hemi: "" };
    const token = /([NSEWnsew])|([-+]?\d+(?:\.\d+)?)/g;
    let match;
    while ((match = token.exec(text)) !== null) {
      if (current.hemi && (match[1] || current.nums.length)) {
        groups.push(current);
        current = { nums: [], hemi: "" };
      }
      if (match[1]) current.hemi = match[1].toUpperCase();
      else current.nums.push(Number(match[2]));
    }
    if (current.nums.length || current.hemi) groups.push(current);
    return groups;
  }

  /** A typed coordinate pair as {lat, lon}, or null if it is not one.
   *
   *  Pure, and the offline half of the search box — so it is a test hook and
   *  it never touches the map. Deliberately strict about what it accepts: a
   *  place NAME that half-parsed as numbers would fly the operator to the
   *  Atlantic instead of saying "no network", which is the one failure this
   *  box must not have. */
  function parseCoordinates(text) {
    let raw = String(text == null ? "" : text).trim();
    if (!raw) return null;
    // "48,080217 11,640969" — a German keyboard writing two decimals. Only
    // when the whole string is exactly that, so a comma stays a SEPARATOR in
    // "48.08, 11.64", which is the same pair under the other convention.
    if (/^[-+]?\d+,\d+\s+[-+]?\d+,\d+$/.test(raw)) raw = raw.replace(/,/g, ".");
    // Anything that is not a number, a hemisphere letter or a coordinate mark
    // means this is a name, not a position.
    if (/[^0-9\s.,\-+°º'′"″NSEWnsew/]/.test(raw)) return null;

    // A comma or a slash is the operator saying where one side ends. Without
    // one, a hemisphere letter says it — and failing both, an even run of
    // numbers splits down the middle, which is what "48.08 11.64" and
    // "48 04 48.8 11 38 27.5" both mean.
    const sides = raw.split(/[,/]/).map((part) => part.trim()).filter(Boolean);
    let groups;
    if (sides.length === 2) {
      const left = coordinateGroups(sides[0]);
      const right = coordinateGroups(sides[1]);
      if (left.length !== 1 || right.length !== 1) return null;
      groups = [left[0], right[0]];
    } else if (sides.length === 1) {
      groups = coordinateGroups(sides[0]);
      if (groups.length === 1 && !groups[0].hemi) {
        const nums = groups[0].nums;
        if (nums.length < 2 || nums.length > 6 || nums.length % 2 !== 0) return null;
        const half = nums.length / 2;
        groups = [
          { nums: nums.slice(0, half), hemi: "" },
          { nums: nums.slice(half), hemi: "" },
        ];
      }
    } else {
      return null;
    }
    if (groups.length !== 2) return null;
    if (groups.some((group) => !group.nums.length || group.nums.length > 3 ||
                               group.nums.some((n) => !isFinite(n)))) return null;

    const values = groups.map((group) => {
      const degrees = dmsToDegrees(group.nums);
      if (degrees == null) return null;
      // S and W are the same statement as a minus sign; both together are
      // still south, never south-of-south.
      return (group.hemi === "S" || group.hemi === "W") ? -Math.abs(degrees) : degrees;
    });
    if (values.some((value) => value == null)) return null;

    const hemis = groups.map((group) => group.hemi);
    let lat, lon;
    if (hemis.some((h) => h === "N" || h === "S") && hemis.some((h) => h === "E" || h === "W")) {
      // Lettered, so the order they were typed in does not matter.
      const latIndex = (hemis[0] === "N" || hemis[0] === "S") ? 0 : 1;
      lat = values[latIndex];
      lon = values[1 - latIndex];
    } else {
      // Unlettered is latitude first, which is what every briefing, every GPS
      // and every MAVLink message means by a pair of numbers…
      lat = values[0];
      lon = values[1];
      // …unless it cannot be. A first number past 90 is not a latitude, and
      // reading it as one would plan the mission on another continent.
      if (Math.abs(lat) > 90 && Math.abs(lon) <= 90) { lat = values[1]; lon = values[0]; }
    }
    if (!isFinite(lat) || !isFinite(lon)) return null;
    if (Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
    return { lat, lon };
  }

  /** How a coordinate pair reads back to the operator, so the row they are
   *  about to click says what it understood. */
  function formatCoordinates(point) {
    return `${point.lat.toFixed(6)}°, ${point.lon.toFixed(6)}°`;
  }

  /** A Nominatim display_name split into the two lines a row is drawn as.
   *
   *  Upstream hands back a full postal address — "Neubiberg, Landkreis
   *  München, Bayern, 85579, Deutschland" — and the list used to show one
   *  ellipsized run of it per row, which puts the one word the operator is
   *  looking for in front of 40 characters they are not. The first comma is
   *  where the place stops and its address starts, in every language
   *  Nominatim answers in, because the field is ordered smallest-first.
   *
   *  Pure, so the rule is assertable without a network or a DOM. */
  function splitLabel(label) {
    const text = String(label == null ? "" : label).trim();
    const comma = text.indexOf(",");
    if (comma <= 0) return { name: text, context: "" };
    return {
      name: text.slice(0, comma).trim(),
      context: text.slice(comma + 1).trim(),
    };
  }

  /** Metres between two {lat, lon}, on a sphere. Good to a few parts in a
   *  thousand at any range a row is worth showing, which is what a distance
   *  rounded to two significant figures needs. */
  function distanceM(a, b) {
    const toRad = Math.PI / 180;
    const dLat = (b.lat - a.lat) * toRad;
    const dLon = (b.lon - a.lon) * toRad;
    const lat1 = a.lat * toRad;
    const lat2 = b.lat * toRad;
    const h = Math.sin(dLat / 2) ** 2 +
              Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLon / 2) ** 2;
    return 2 * EARTH_R_M * Math.asin(Math.min(1, Math.sqrt(h)));
  }

  /** How far a result is from where the map is looking, as the row says it.
   *
   *  The reason the column exists: a search for a common name answers with
   *  several of it, and "which one" is a question the names cannot settle and
   *  the distance can. Empty rather than "0 m" when there is no camera to
   *  measure from. */
  function formatDistance(metres) {
    if (!isFinite(metres)) return "";
    if (metres < 950) return `${Math.round(metres / 10) * 10} m`;
    if (metres < 9950) return `${(metres / 1000).toFixed(1)} km`;
    return `${Math.round(metres / 1000)} km`;
  }

  /** Which row is selected after the list is redrawn. Pure, so the rule the
   *  operator feels under their fingers is assertable without a DOM.
   *
   *  Matched on the label rather than the index: the second draw PREPENDS
   *  nothing but can append (local rows, then the network's), and a row that
   *  moved is still the row they chose. Anything not found falls back to the
   *  first row, which is what an operator who has not arrowed anywhere
   *  expects Enter to take.
   */
  function keepSelection(previous, rows) {
    if (!rows.length) return -1;
    const kept = previous
      ? rows.findIndex((row) => row && row.label === previous.label)
      : -1;
    return kept >= 0 ? kept : 0;
  }

  /** One corner of a downloaded area's box, or null if it is not a number.
   *
   *  Not `isFinite(Number(v))`, which is what this was: Number(null) is 0 and
   *  Number("") is 0, so a region the backend handed back with a missing
   *  corner passed the check and became a box anchored on the prime meridian.
   *  The row looked ordinary, said the right name, and flew the operator into
   *  the sea off Africa. */
  function corner(value) {
    if (value === null || value === undefined || value === "" ||
        typeof value === "boolean") return null;
    const number = Number(value);
    return isFinite(number) ? number : null;
  }

  /** Downloaded areas whose name matches, as search rows. The offline half of
   *  a NAME search: these are on the laptop because the operator put them
   *  there, and they are named after exactly the places missions get flown. */
  function regionMatches(regions, query) {
    const needle = String(query || "").trim().toLowerCase();
    if (!needle) return [];
    const seen = new Set();
    const rows = [];
    (regions || []).forEach((region) => {
      const name = String((region && region.name) || "").trim();
      if (!name || seen.has(name.toLowerCase())) return;
      if (name.toLowerCase().indexOf(needle) === -1) return;
      const b = region.bounds || {};
      const box = { w: corner(b.w), s: corner(b.s), e: corner(b.e), n: corner(b.n) };
      if (box.w === null || box.s === null || box.e === null || box.n === null) return;
      seen.add(name.toLowerCase());
      rows.push({
        label: name,
        kind: "downloaded area",
        // This laptop's own answer, not the geocoder's — so the label is a
        // name the operator typed, never an address to be split.
        local: true,
        lat: (box.s + box.n) / 2,
        lon: (box.w + box.e) / 2,
        bounds: box,
      });
    });
    return rows;
  }

  // =====================================================================
  // The control
  // =====================================================================

  function create(spec) {
    const options = spec || {};
    const mapOf = typeof options.map === "function" ? options.map : () => options.map || null;
    const regionsOf = typeof options.regions === "function"
      ? options.regions : () => options.regions || [];

    let destroyed = false;
    let open = false;
    let timer = null;
    let token = 0;          // guards a slow lookup against a newer keystroke
    let rows = [];
    let index = -1;
    let pin = null;

    const el = document.createElement("div");
    el.className = "map-search";

    const trigger = Corvus.ui.iconButton("search", {
      className: "map-search-trigger",
      title: "Search for a place",
      ariaLabel: "Search for a place",
      size: 17,
      onClick: () => setOpen(!open),
    });
    trigger.setAttribute("aria-expanded", "false");

    // The field and what sits inside it. A box that grows out of a magnifier
    // loses the magnifier the moment it is open, so the field carries its own
    // — and a clear button, because emptying a search by holding backspace is
    // not a gesture, it is a chore.
    const fieldEl = document.createElement("div");
    fieldEl.className = "map-search-field";
    const fieldIcon = Corvus.ui.icon("search", "auto");
    fieldIcon.classList.add("map-search-field-icon");

    const input = Corvus.ui.input({
      className: "map-search-input",
      placeholder: "Place or coordinates",
      ariaLabel: "Search for a place or type coordinates",
      autocomplete: false,
      spellcheck: false,
      onInput: () => { markEmpty(); schedule(); },
    });
    input.addEventListener("keydown", onKey);

    const clearBtn = Corvus.ui.iconButton("x", {
      className: "map-search-reset",
      title: "Clear the search",
      ariaLabel: "Clear the search",
      size: 13,
      onClick: () => {
        input.value = "";
        markEmpty();
        schedule();
        input.focus();
      },
    });
    fieldEl.append(fieldIcon, input, clearBtn);

    const resultsEl = document.createElement("div");
    resultsEl.className = "map-search-results";
    resultsEl.setAttribute("role", "listbox");
    resultsEl.hidden = true;

    // Field first, button second: the control is anchored by its right edge,
    // so the field grows out to the LEFT and the button never moves.
    el.append(fieldEl, trigger, resultsEl);
    if (options.container) options.container.appendChild(el);

    /** The clear button is only a control while there is something to clear. */
    function markEmpty() {
      el.classList.toggle("has-text", !!input.value);
    }

    function setOpen(next) {
      open = !!next;
      el.classList.toggle("is-open", open);
      trigger.classList.toggle("active", open);
      trigger.setAttribute("aria-expanded", open ? "true" : "false");
      if (open) {
        // The box is no use to somebody who has to click it a second time to
        // type into it.
        input.focus();
        input.select();
        document.addEventListener("pointerdown", onOutside, true);
        schedule();
      } else {
        document.removeEventListener("pointerdown", onOutside, true);
        clearTimer();
        show([], "");
        input.blur();
      }
    }

    /** Close the search when the operator presses somewhere else.
     *
     *  Every other popover in this UI does — the layer menu, the map's
     *  context menu, the flight bar's mode list all go through
     *  Corvus.ui.menu, which has closed on an outside press from the start.
     *  This box was the one that did not, and it is the one that most needs
     *  to: `.map-search.is-open` deliberately raises its z-index above the
     *  tool bar, so a search left open after a press on the map sits on top
     *  of the controls the operator just reached for.
     *
     *  pointerdown in the capture phase, the same as Corvus.ui.menu: by the
     *  time the press lands the decision is made, and capturing means a
     *  handler further down (the map's own drag start) cannot swallow it
     *  first. A press inside the box is not an outside press — that includes
     *  the trigger button, which lives inside `el` and does its own toggling
     *  on click; closing here would make it reopen on every press.
     */
    function onOutside(event) {
      if (!open) return;
      const target = event && event.target;
      if (target && typeof el.contains === "function" && el.contains(target)) return;
      setOpen(false);
    }

    function clearTimer() {
      if (timer) { clearTimeout(timer); timer = null; }
    }

    /** Every keystroke shows what can be answered without the network — a
     *  coordinate pair, a downloaded area — immediately, and queues the
     *  online lookup behind a debounce so typing a town does not cost a
     *  request a letter (which is also what Nominatim's usage policy
     *  forbids). */
    function schedule() {
      clearTimer();
      const query = input.value;
      const local = localRows(query);
      show(local, "");
      const text = query.trim();
      if (!text || parseCoordinates(text)) return;   // nothing left to look up
      timer = setTimeout(() => {
        timer = null;
        run(text, local);
      }, DEBOUNCE_MS);
    }

    /** What this laptop can answer by itself. */
    function localRows(query) {
      const text = String(query || "").trim();
      if (!text) return [];
      const point = parseCoordinates(text);
      const found = point ? [{
        label: formatCoordinates(point),
        kind: "coordinates",
        local: true,
        lat: point.lat, lon: point.lon, bounds: null,
      }] : [];
      return found.concat(regionMatches(regionsOf(), text)).slice(0, MAX_ROWS);
    }

    function run(text, localForQuery) {
      const mine = ++token;
      resultsEl.classList.add("is-busy");
      const map = mapOf();
      const centre = map ? map.getCenter() : null;
      const near = centre ? `&near=${centre.lng.toFixed(5)},${centre.lat.toFixed(5)}` : "";
      Corvus.telemetry.requestJson(
        `/api/geocode?q=${encodeURIComponent(text)}&limit=${MAX_ROWS}${near}`,
      ).then((res) => {
        if (destroyed || mine !== token) return;
        const found = (res && Array.isArray(res.results)) ? res.results : [];
        const all = localForQuery.concat(found).slice(0, MAX_ROWS);
        const note = all.length ? "" : ((res && res.error) || "Nothing found.");
        show(all, note);
      }).catch((error) => {
        if (destroyed || mine !== token) return;
        show(localForQuery,
          localForQuery.length ? "" : (error.message || "The place search failed."));
      });
    }

    /** Where the map is looking, as a plain point, or null before it exists.
     *  Read per draw rather than per row — it is the same answer for all of
     *  them and getCenter() is not free. */
    function cameraPoint() {
      const map = mapOf();
      const centre = map && typeof map.getCenter === "function" ? map.getCenter() : null;
      if (!centre || !isFinite(centre.lng) || !isFinite(centre.lat)) return null;
      return { lat: centre.lat, lon: centre.lng };
    }

    /** Draw the result list. `note` is the line shown INSTEAD of rows when
     *  there are none — "no network" and "nothing there" are different
     *  answers and the operator has to be able to tell them apart. */
    function show(next, note) {
      resultsEl.classList.remove("is-busy");
      // The list is drawn twice per search: once with what this laptop can
      // answer on its own (a coordinate pair, a downloaded area) and again
      // ~350 ms later when the network results land. Resetting the selection
      // both times threw an operator who was already arrowing down back to
      // row 0 under their fingers, mid-keystroke. So the row they had is
      // looked up again by label and kept when it is still there.
      const previous = index >= 0 ? rows[index] : null;
      rows = Array.isArray(next) ? next : [];
      index = keepSelection(previous, rows);
      Corvus.ui.clear(resultsEl);
      const from = cameraPoint();
      rows.forEach((row, i) => resultsEl.appendChild(buildRow(row, i, from)));
      if (!rows.length && note) {
        const empty = document.createElement("div");
        empty.className = "map-search-note";
        empty.textContent = note;
        resultsEl.appendChild(empty);
      }
      resultsEl.hidden = !rows.length && !note;
      highlight();
      Corvus.ui.refreshIcons();
    }

    /** One result: what it is, what it is called, where it is.
     *
     *  Three columns and two lines, rather than the single ellipsized run of
     *  a postal address this used to be. The glyph says which of the three
     *  kinds of answer the row is without reading a word of it, the name is
     *  the word the operator typed, and the distance settles a list of
     *  identically named villages. */
    function buildRow(row, i, from) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "map-search-row";
      button.setAttribute("role", "option");

      const glyph = Corvus.ui.icon(KIND_ICON[row.kind] || "map-pin", "auto");
      glyph.classList.add("map-search-row-icon");
      button.appendChild(glyph);

      const text = document.createElement("span");
      text.className = "map-search-row-text";
      // Only a geocoded label is an address with a place at the front of it.
      // A coordinate pair is ONE thing containing a comma, and splitting it
      // put the latitude on the first line and the longitude on the second —
      // two numbers that mean nothing apart, drawn as if they were a name and
      // its county.
      const parts = row.local ? { name: row.label, context: "" } : splitLabel(row.label);
      const name = document.createElement("span");
      name.className = "map-search-row-name";
      // textContent, never innerHTML: this string came off the network.
      name.textContent = parts.name;
      text.appendChild(name);
      // The address and the sort of place are one quiet line, and either half
      // may be missing — a coordinate pair has no address, a downloaded area
      // has no category.
      const detail = [parts.context, row.kind].filter(Boolean).join(" · ");
      if (detail) {
        const sub = document.createElement("span");
        sub.className = "map-search-row-detail";
        sub.textContent = detail;
        text.appendChild(sub);
      }
      button.appendChild(text);

      if (from && isFinite(row.lat) && isFinite(row.lon)) {
        const away = document.createElement("span");
        away.className = "map-search-row-dist";
        away.textContent = formatDistance(distanceM(from, { lat: row.lat, lon: row.lon }));
        button.appendChild(away);
      }

      button.addEventListener("click", () => goTo(i));
      return button;
    }

    function highlight() {
      const buttons = resultsEl.querySelectorAll(".map-search-row");
      buttons.forEach((button, i) => {
        button.classList.toggle("is-active", i === index);
        button.setAttribute("aria-selected", i === index ? "true" : "false");
      });
    }

    function onKey(event) {
      if (event.key === "Escape") {
        // Stops here: the page's own Escape disarms the planner's placement
        // tool, and an operator closing a search box did not ask for that as
        // well.
        event.preventDefault();
        event.stopPropagation();
        setOpen(false);
        return;
      }
      if (event.key === "Enter") {
        event.preventDefault();
        // Enter before the debounce has fired means "I have finished
        // typing", so look it up now rather than making them wait out the
        // timer.
        if (timer) {
          const text = input.value.trim();
          clearTimer();
          if (text && !parseCoordinates(text)) { run(text, localRows(text)); return; }
        }
        goTo(index < 0 ? 0 : index);
        return;
      }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        if (!rows.length) return;
        event.preventDefault();
        const step = event.key === "ArrowDown" ? 1 : -1;
        index = (index + step + rows.length) % rows.length;
        highlight();
      }
    }

    /** Fly to one result and mark it.
     *
     *  Typing a place IS the operator aiming the map — more deliberately
     *  than the wheel nudge that counts as one — so the host is told, and
     *  the two maps open on the ground the operator last asked for whichever
     *  of them they asked it on. The fly-to itself is programmatic and
     *  carries no originalEvent, so neither map's own movestart handler can
     *  see it; onGo is how it is said out loud instead. */
    function goTo(i) {
      const row = rows[i];
      const map = mapOf();
      if (!row || !map) return;
      const bounds = row.bounds;
      if (bounds && (bounds.e - bounds.w > 1e-6 || bounds.n - bounds.s > 1e-6)) {
        map.fitBounds([[bounds.w, bounds.s], [bounds.e, bounds.n]],
                      { padding: 80, duration: 700, maxZoom: MAX_ZOOM });
      } else {
        map.easeTo({
          center: [row.lon, row.lat],
          zoom: Math.max(map.getZoom(), RESULT_ZOOM),
          duration: 700,
        });
      }
      dropPin(row);
      setOpen(false);
      if (typeof options.onGo === "function") options.onGo(row);
    }

    /** A pin on the place that was found, so a village in a forest is
     *  somewhere rather than just a view. Cleared by the next search and by
     *  leaving. */
    function dropPin(row) {
      clearPin();
      const map = mapOf();
      if (!map || typeof maplibregl === "undefined") return;
      const element = document.createElement("div");
      element.className = "map-search-pin";
      element.title = row.label;
      // Press it to take it off again — the pin is a note to self, and a mark
      // that can only be removed by searching for something else is litter.
      element.addEventListener("click", (event) => {
        event.stopPropagation();
        clearPin();
      });
      pin = new maplibregl.Marker({ element, anchor: "center" })
        .setLngLat([row.lon, row.lat]).addTo(map);
    }

    function clearPin() {
      if (pin) { pin.remove(); pin = null; }
    }

    Corvus.ui.refreshIcons();

    return {
      el,
      isOpen: () => open,
      setOpen,
      clearPin,
      /** Everything this control put outside its own element: the capture
       *  listener it registers on the document while open, the pending
       *  lookup, and the marker it added to somebody else's map. */
      destroy() {
        destroyed = true;
        clearTimer();
        clearPin();
        document.removeEventListener("pointerdown", onOutside, true);
        open = false;
        rows = [];
        index = -1;
        if (el.parentNode) el.parentNode.removeChild(el);
      },
    };
  }

  return {
    create,
    // Test hooks: the pure halves. All of them are wrong in ways nothing
    // throws over — a name that half-parses as numbers flies the operator to
    // the Atlantic, a label split at the wrong place hides the word they
    // searched for, a dropped selection moves the row Enter takes.
    _parseCoordinates: parseCoordinates,
    _formatCoordinates: formatCoordinates,
    _splitLabel: splitLabel,
    _formatDistance: formatDistance,
    _distanceM: distanceM,
    _keepSelection: keepSelection,
    _regionMatches: regionMatches,
  };
})();
