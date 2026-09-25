"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.setupRemoteId — the Remote ID sub-page of the Setup page.
 *
 * The one setup page whose subject is not the aircraft.
 *
 * Every other page here reads a parameter, writes a parameter, and the
 * autopilot owns the answer. Remote ID does not work that way. The serial
 * number, the registration number the authority issued, the class mark on the
 * airframe — none of it is stored on the vehicle, and no parameter holds it.
 * It is held by the *ground station* and pushed down the link once a second as
 * OPEN_DRONE_ID_BASIC_ID, OPEN_DRONE_ID_OPERATOR_ID, OPEN_DRONE_ID_SELF_ID and
 * OPEN_DRONE_ID_SYSTEM. Both PX4 and ArduPilot will refuse to arm when those
 * stop arriving, which makes this page part of the pre-flight rather than part
 * of the initial setup: it is what the aircraft is saying about you, right now,
 * to anybody within receiver range.
 *
 * That is why the page is shaped the way it is.
 *
 * *The broadcast has a switch, and it starts off.* An aircraft broadcasting a
 * blank or half-filled identity is not a smaller version of a correct filing,
 * it is a false one. Nothing goes out until the operator says so.
 *
 * *The checks are advisory, and the page always saves.* What the FAA's Part 89
 * broadcast and the EU's EN 4709-002 ask for are two different lists, and
 * neither is the whole of what a given operator is flying under — an exemption,
 * a national rule, an aircraft registered abroad. So the backend reports what
 * is missing for the chosen region and the page shows it as a list to work
 * through, never as a refusal. Refusing to store a registration number because
 * this build did not recognise its shape would be Corvus being wrong about the
 * operator's own paperwork.
 *
 * *The identity stays editable while armed.* The vehicle's own parameters do
 * not — the autopilot refuses those writes and the page greys them with the
 * usual banner — but the identity is Corvus's, and an operator who has just
 * noticed a wrong serial number on a live aircraft needs to fix it rather than
 * be told to disarm first.
 *
 * *Nothing here is legal advice.* The page names the two published broadcast
 * formats and says what each one asks for. The filing is the operator's.
 *
 * Backend contract:
 *   GET  /api/remoteid                  {connected,sections,identity,configured,
 *                                        schema,findings,status,suggested_ua_type}
 *   GET  /api/remoteid/status           {identity,findings,status} — the poll
 *   POST /api/config {remote_id:{...}}  save the identity (merged one level deep)
 *   POST /api/params/set {name,value}   write one vehicle field (refused while armed)
 *
 * Exposes render(container, navigateBack) -> destroy(). The caller (setup.js)
 * owns the lifecycle and calls destroy() on back / left-nav re-entry, which
 * stops the status poll, releases the telemetry subscription, and disowns any
 * in-flight fetch so a late answer never writes to a DOM this page has left.
 */
Corvus.setupRemoteId = (function () {
  const S = Corvus.setupShared;
  const { applyArmed, setFieldStatus, notify } = S;

  // How often the live broadcast status is re-read while the page is open.
  // The identity half needs no polling at all — it only changes when this page
  // changes it — but "is it actually going out" and the aircraft's arm verdict
  // are the two things an operator stands there waiting for.
  //
  // The poll goes to /api/remoteid/status rather than to /api/remoteid, and
  // that is not a size optimisation. The full endpoint reads the vehicle's
  // parameters, and a parameter read on a reconnecting link can sit for tens of
  // seconds behind the bridge's operation lock; polled every three seconds,
  // those stack up against the browser's six-connection-per-origin limit and
  // starve the map and every other fetch on the page. `polling` guards the
  // remaining case — one slow answer must not be followed by a second request.
  const POLL_MS = 3000;

  // -1000 m is the Open Drone ID sentinel for "not known" on every altitude
  // field. It is a real number on the wire, so an empty input has to become
  // this rather than 0, which would claim sea level.
  const ALTITUDE_UNKNOWN = -1000;

  /** What a page that has never been saved holds, so the form can draw first. */
  const EMPTY_IDENTITY = {
    enabled: false, region: "eu",
    basic_id: { id_type: 1, ua_type: 0, uas_id: "" },
    operator_id: { operator_id_type: 0, operator_id: "" },
    self_id: { description_type: 0, description: "" },
    system: {
      operator_location_type: 0, operator_latitude: 0, operator_longitude: 0,
      operator_altitude_geo: ALTITUDE_UNKNOWN, classification_type: 0,
      category_eu: 0, class_eu: 0, area_count: 1, area_radius: 0,
      area_ceiling: ALTITUDE_UNKNOWN, area_floor: ALTITUDE_UNKNOWN,
    },
  };

  function render(container, navigateBack) {
    const state = {
      container, navigateBack,
      doc: null,
      identity: cloneIdentity(EMPTY_IDENTITY),
      armed: false, loading: false, polling: false, destroyed: false, controls: [],
      status: { cls: "", text: "" },
      live: null,
      poll: null,
      // Check values (setupShared), for the vehicle's own parameters only:
      // what the operator set until a check confirms it, and the last
      // check's outcome until the next one.
      wanted: {}, check: null, checking: false,
    };

    const cur = Corvus.telemetry && Corvus.telemetry.getState();
    state.armed = !!(cur && cur.armed);
    state.live = cur || null;

    paint(state);

    if (Corvus.telemetry && typeof Corvus.telemetry.subscribe === "function") {
      state.unsub = Corvus.telemetry.subscribe((s) => {
        if (state.destroyed || !s) return;
        state.live = s;
        applyArmed(state, !!s.armed);
      });
    }

    load(state);
    startPoll(state);

    return function destroy() {
      state.destroyed = true;
      stopPoll(state);
      if (state.unsub) { try { state.unsub(); } catch (_e) {} state.unsub = null; }
    };
  }

  /** A deep-enough copy: the identity is two levels and all leaves are scalars. */
  function cloneIdentity(src) {
    const source = src || {};
    const out = {
      enabled: !!source.enabled,
      region: source.region || "eu",
    };
    ["basic_id", "operator_id", "self_id", "system"].forEach((key) => {
      out[key] = Object.assign({}, EMPTY_IDENTITY[key], source[key] || {});
    });
    return out;
  }

  // -------------------------------------------------------------------------
  // Loading and polling
  // -------------------------------------------------------------------------

  // Check values: the shared read back, redrawn by a fresh load(). Only the
  // vehicle sections: the identity above them is Corvus's own, saved to the
  // config file, and there is nothing on the vehicle to read it back from.
  const CHECK = {
    prefix: "rid",
    names: (state) => S.fieldParams((state.doc && state.doc.sections) || []),
    reload: (state) => load(state, true),
    setStatus: (state, cls, text) => setStatus(state, cls, text),
  };

  /** `fresh` reads the vehicle's parameters from it rather than the cache. */
  function load(state, fresh) {
    if (state.loading) return Promise.resolve();
    state.loading = true;
    if (state.reloadBtn) state.reloadBtn.disabled = true;
    if (state.checkBtn) state.checkBtn.disabled = true;
    setStatus(state, "pending", "Reading Remote ID configuration…");
    return Corvus.telemetry.requestJson(fresh ? "/api/remoteid?fresh=1" : "/api/remoteid").then((doc) => {
      if (state.destroyed) return;
      state.doc = doc || {};
      state.identity = cloneIdentity((doc && doc.identity) || EMPTY_IDENTITY);
      if (!doc || !doc.connected) {
        // Not an error the way it is on the other pages. The identity is
        // configured on the ground, often with the aircraft still in its case,
        // and the whole top half of this page works with nothing connected.
        state.status = {
          cls: "", text: (doc && doc.error)
            ? `Vehicle settings unavailable: ${doc.error}`
            : "No vehicle connected. The identity below is still editable",
        };
      } else {
        state.status = { cls: "ok", text: `${doc.received} vehicle parameters read` };
      }
    }).catch((err) => {
      if (state.destroyed) return;
      state.doc = {};
      state.status = {
        cls: "err", text: (err && err.message) || "Could not read the Remote ID configuration",
      };
    }).finally(() => {
      if (state.destroyed) return;
      state.loading = false;
      paint(state);
    });
  }

  /**
   * Re-read only what changes on its own.
   *
   * A full load() would rebuild the form under the operator's cursor, so this
   * takes the same document and repaints two cards from it: whether the
   * broadcast is going out, and what is still missing from the filing. Both
   * answers move without anybody touching the page — the first when the link
   * comes up, the second when a save lands.
   */
  function refreshChecks(state) {
    if (state.destroyed || state.loading || state.polling) return Promise.resolve();
    state.polling = true;
    return Corvus.telemetry.requestJson("/api/remoteid/status").then((doc) => {
      if (state.destroyed || state.loading || !doc) return;
      state.doc = Object.assign({}, state.doc, {
        status: doc.status, findings: doc.findings,
      });
      repaintCard(state, ".rid-status-card", statusCard);
      repaintCard(state, ".rid-checks-card", checksCard);
    }).catch(() => { /* a poll that fails is not worth a banner */ })
      .finally(() => { state.polling = false; });
  }

  function startPoll(state) {
    stopPoll(state);
    const timer = window.setInterval(() => refreshChecks(state), POLL_MS);
    state.poll = timer;
  }

  function stopPoll(state) {
    if (state.poll != null) {
      try { window.clearInterval(state.poll); } catch (_e) {}
      state.poll = null;
    }
  }

  function setStatus(state, cls, text) {
    state.status = { cls, text };
    S.setActionsStatus(state.actionsStatus, cls, text);
  }

  // -------------------------------------------------------------------------
  // Page chrome
  // -------------------------------------------------------------------------

  function paint(state) {
    if (state.destroyed) return;
    state.container.innerHTML = "";
    state.controls = [];

    const page = S.el("div", "setup-page rid-page");
    page.appendChild(S.backButton(state.navigateBack));
    page.appendChild(S.pageHeader("Remote ID",
      "The identity this station broadcasts for the aircraft"));

    const actions = S.el("div", "params-actions");
    const reloadBtn = Corvus.ui.button({
      variant: "primary", size: "sm", icon: "refresh-cw", label: "Reload",
      className: "rid-reload",
    });
    reloadBtn.disabled = state.loading || state.checking;
    reloadBtn.addEventListener("click", () => {
      state.check = null;
      load(state);
    });
    const checkBtn = S.checkButton(state, CHECK);
    const actionsStatus = S.el("div", "params-actions-status");
    actions.appendChild(reloadBtn);
    actions.appendChild(checkBtn);
    actions.appendChild(actionsStatus);
    page.appendChild(actions);

    // Only the vehicle's own parameters are refused while armed. Everything
    // above them is Corvus's own identity and stays live — see the module
    // comment for why that is deliberate rather than an oversight.
    const banner = S.el("div", "params-banner");
    banner.hidden = true;
    banner.textContent = "Vehicle Remote ID parameters are read-only while armed";
    page.appendChild(banner);

    state.reloadBtn = reloadBtn;
    state.actionsStatus = actionsStatus;
    state.banner = banner;

    const host = S.el("div", "rid-stack");
    state.host = host;
    page.appendChild(host);
    state.container.appendChild(page);
    S.setActionsStatus(actionsStatus, state.status.cls, state.status.text);

    host.appendChild(broadcastCard(state));
    host.appendChild(statusCard(state));
    host.appendChild(checksCard(state));

    const grid = S.el("div", "rid-grid");
    grid.appendChild(basicIdCard(state));
    grid.appendChild(operatorIdCard(state));
    grid.appendChild(selfIdCard(state));
    grid.appendChild(operatorLocationCard(state));
    host.appendChild(grid);

    host.appendChild(classificationCard(state));
    if (state.check) host.appendChild(S.checkCard(state.check, "rid"));
    renderVehicleSections(state, host);

    applyArmed(state, state.armed);
    S.refreshIcons();
  }

  /** Swap one card for a freshly built one, in place, keeping the rest still. */
  function repaintCard(state, selector, builder) {
    if (!state.host) return;
    const old = state.host.querySelector(selector);
    const next = builder(state);
    if (old && old.parentNode) {
      S.dropControls(state, old);
      old.parentNode.replaceChild(next, old);
    } else {
      state.host.appendChild(next);
    }
    applyArmed(state, state.armed);
    S.refreshIcons();
  }

  function schema(state) {
    return (state.doc && state.doc.schema) || {};
  }

  function options(state, key, fallback) {
    const list = schema(state)[key];
    return Array.isArray(list) && list.length ? list : (fallback || []);
  }

  // -------------------------------------------------------------------------
  // The switch, and what the broadcast is doing
  // -------------------------------------------------------------------------

  function broadcastCard(state) {
    const card = S.el("div", "page-card rid-card rid-broadcast-card");
    card.dataset.section = "broadcast";
    card.appendChild(S.sectionTitle("Broadcast"));
    card.appendChild(S.el("div", "field-hint",
      "With this on, Corvus sends the identity below to the aircraft once a "
      + "second for as long as it is connected. The aircraft passes it to its "
      + "Remote ID transmitter, and stops arming when the stream stops. Off, "
      + "nothing is sent at all, which is the right setting until the details "
      + "below are the ones on your registration."));

    const head = S.el("div", "rid-switch-row");
    const label = S.el("div", "rid-switch-label");
    label.appendChild(S.el("span", "rid-switch-title", "Broadcast this identity"));
    label.appendChild(S.el("span", "rid-switch-sub",
      "Sent to the connected aircraft, not over the air by this laptop."));
    head.appendChild(label);

    const status = S.el("span", "params-row-status");
    const toggle = Corvus.ui.toggle({
      className: "rid-enable-toggle",
      value: !!state.identity.enabled,
      ariaLabel: "Broadcast this Remote ID identity",
      onChange: (next) => saveIdentity(state, { enabled: next }, status),
    });
    toggle.el.dataset.role = "enable";
    head.appendChild(toggle.el);
    card.appendChild(head);
    card.appendChild(status);

    const grid = S.el("div", "pform-grid rid-grid-fields");
    grid.appendChild(selectRow(state, {
      key: "region", path: [], label: "Check against",
      options: options(state, "regions", [{ value: "eu", label: "European Union" }]),
      stringValue: true,
      info: "Which published broadcast format the list below is measured "
            + "against. It changes what Corvus checks, never what it sends.",
    }));
    card.appendChild(grid);
    return card;
  }

  function statusCard(state) {
    const card = S.el("div", "page-card rid-card rid-status-card");
    card.dataset.section = "status";
    card.appendChild(S.sectionTitle("Right now"));

    const live = (state.doc && state.doc.status) || {};
    const list = S.el("div", "rid-status-rows");

    list.appendChild(statusRow("link", "Link",
      live.supported
        ? "MAVLink 2: the Remote ID messages can be carried"
        : "MAVLink 1, or no vehicle. Remote ID needs MAVLink 2",
      live.supported ? "healthy" : "off"));

    let sendingText;
    let sendingLevel;
    if (!live.enabled) {
      sendingText = "Off, nothing is being sent";
      sendingLevel = "off";
    } else if (live.broadcasting) {
      sendingText = live.last_sent_age != null
        ? `Sending, last message ${live.last_sent_age} s ago`
        : "Sending";
      sendingLevel = "healthy";
    } else {
      sendingText = live.error || "On, but nothing has gone out yet";
      sendingLevel = "warning";
    }
    list.appendChild(statusRow("sending", "Broadcast", sendingText, sendingLevel));

    const arm = live.arm_status;
    let armText;
    let armLevel;
    if (!arm) {
      // Not a fault. PX4 does not send this message at all, and an ArduPilot
      // aircraft with Remote ID disabled has nothing to report — so the row
      // says "nothing said" rather than inventing a clearance or a failure.
      armText = "The aircraft has not reported a Remote ID arm status";
      armLevel = "off";
    } else if (arm.ok) {
      armText = "The aircraft's Remote ID system is ready to arm";
      armLevel = "healthy";
    } else {
      armText = arm.error || "The aircraft's Remote ID system would refuse the arm";
      armLevel = "critical";
    }
    list.appendChild(statusRow("arm", "Aircraft", armText, armLevel));

    card.appendChild(list);
    return card;
  }

  function statusRow(key, label, text, level) {
    const row = S.el("div", "rid-status-row");
    row.dataset.status = key;
    row.appendChild(Corvus.ui.statusDot(level));
    row.appendChild(S.el("span", "rid-status-label", label));
    row.appendChild(S.el("span", "rid-status-text", text));
    return row;
  }

  // -------------------------------------------------------------------------
  // What is still missing
  // -------------------------------------------------------------------------

  function checksCard(state) {
    const card = S.el("div", "page-card rid-card rid-checks-card");
    card.dataset.section = "checks";
    card.appendChild(S.sectionTitle("What the rules ask for"));

    const regions = options(state, "regions", []);
    const picked = regions.filter((r) => r.value === state.identity.region)[0];
    card.appendChild(S.el("div", "field-hint",
      "Measured against " + ((picked && picked.label) || "the selected region")
      + ". This is a reading of the published broadcast format, not legal "
      + "advice and not a certification. The filing is yours."));

    const findings = (state.doc && state.doc.findings) || [];
    if (!findings.length) {
      const ok = S.el("div", "rid-check rid-check-ok");
      ok.dataset.level = "ok";
      ok.appendChild(S.icon("check"));
      ok.appendChild(S.el("span", "rid-check-text",
        "Everything this region's broadcast format asks for is filled in."));
      card.appendChild(ok);
      return card;
    }

    const list = S.el("div", "rid-checks");
    findings.forEach((finding) => {
      const row = S.el("div", "rid-check rid-check-" + (finding.level || "warning"));
      row.dataset.level = finding.level || "warning";
      row.dataset.field = finding.field || "";
      row.appendChild(S.icon(finding.level === "error" ? "alert-triangle" : "info"));
      row.appendChild(S.el("span", "rid-check-text", finding.text || ""));
      list.appendChild(row);
    });
    card.appendChild(list);
    return card;
  }

  // -------------------------------------------------------------------------
  // The identity itself
  // -------------------------------------------------------------------------

  function basicIdCard(state) {
    const card = S.el("div", "page-card rid-card rid-basic-card");
    card.dataset.section = "basic_id";
    card.appendChild(S.sectionTitle("Basic ID: the aircraft"));
    card.appendChild(S.el("div", "field-hint",
      "What identifies the airframe. Under both the FAA's and the EU's rules "
      + "this is normally the manufacturer's serial number in ANSI/CTA-2063-A "
      + "form: four characters of manufacturer code, one character saying how "
      + "long the rest is, then the serial. It is on the airframe's label, not "
      + "in its parameters."));

    const grid = S.el("div", "pform-grid rid-grid-fields");
    grid.appendChild(selectRow(state, {
      key: "id_type", path: ["basic_id"], label: "ID type",
      options: options(state, "id_types", []),
      info: "What kind of identifier the field below is. Getting this wrong "
            + "broadcasts a correct number under the wrong heading.",
    }));
    grid.appendChild(textRow(state, {
      key: "uas_id", path: ["basic_id"], label: "Aircraft ID",
      maxLength: (schema(state).limits || {}).uas_id || 20,
      placeholder: "e.g. 1596F483658SK8U6PNJ1",
      info: "Exactly as it appears on the aircraft. Capitals and digits only, "
            + "without the letters I and O.",
      uppercase: true,
      validate: (value) => serialProblem(state, value),
    }));
    grid.appendChild(selectRow(state, {
      key: "ua_type", path: ["basic_id"], label: "Aircraft type",
      options: options(state, "ua_types", []),
      info: "What the aircraft is, as it was registered.",
      suggestion: suggestedUaType(state),
    }));
    card.appendChild(grid);
    return card;
  }

  /**
   * The serial-number check, run in the browser as the operator types.
   *
   * The backend runs the same rules and its answer is what the checks card
   * shows; this is the one place the page duplicates a backend rule, and only
   * the shape of it — a serial that does not fit the format at all should say
   * so under the field, while the character is still under the cursor, rather
   * than after a round trip. The value is saved either way.
   */
  function serialProblem(state, value) {
    if (state.identity.basic_id.id_type !== 1) return "";
    const text = String(value || "").trim();
    if (!text) return "";
    if (/[^0-9A-Z]/.test(text)) return "capitals and digits only";
    if (/[IO]/.test(text)) return "the letters I and O are not allowed";
    if (text.length < 6) return "too short for a CTA-2063-A serial";
    const declared = parseInt(text[4], 16);
    if (!(declared >= 1 && declared <= 15)) {
      return "the 5th character is the length code and must be 1-9 or A-F";
    }
    if (text.length - 5 !== declared) {
      return `the length character says ${declared} characters follow it, `
        + `but ${text.length - 5} do`;
    }
    return "";
  }

  /** The aircraft type the connected vehicle looks like, or null. */
  function suggestedUaType(state) {
    const suggested = Number((state.doc || {}).suggested_ua_type) || 0;
    if (!suggested || suggested === state.identity.basic_id.ua_type) return null;
    const match = options(state, "ua_types", []).filter((o) => o.value === suggested)[0];
    if (!match) return null;
    return {
      value: suggested,
      // Deliberately phrased as what the airframe *is*, not what it should be
      // filed as: the two differ often enough that this must never read as an
      // instruction.
      label: `The connected aircraft is a ${String(match.label).toLowerCase()}`,
    };
  }

  function operatorIdCard(state) {
    const card = S.el("div", "page-card rid-card rid-operator-card");
    card.dataset.section = "operator_id";
    card.appendChild(S.sectionTitle("Operator ID: you"));
    card.appendChild(S.el("div", "field-hint",
      "The registration number your authority issued to you as an operator, "
      + "which is not the same as the aircraft's. In the EU it is the 16 "
      + "characters that start with your country code. The three characters "
      + "after them are the secret half of your registration and are never "
      + "broadcast. Do not type them here."));

    const grid = S.el("div", "pform-grid rid-grid-fields");
    grid.appendChild(selectRow(state, {
      key: "operator_id_type", path: ["operator_id"], label: "Type",
      options: options(state, "operator_id_types", []),
    }));
    grid.appendChild(textRow(state, {
      key: "operator_id", path: ["operator_id"], label: "Operator registration",
      maxLength: (schema(state).limits || {}).operator_id || 20,
      placeholder: "e.g. FIN87astrdge12k8",
      info: "Under the FAA's rule this field is optional; under the EU's it is "
            + "what the broadcast is for.",
    }));
    card.appendChild(grid);
    return card;
  }

  function selfIdCard(state) {
    const card = S.el("div", "page-card rid-card rid-self-card");
    card.dataset.section = "self_id";
    card.appendChild(S.sectionTitle("Self ID: the flight"));
    card.appendChild(S.el("div", "field-hint",
      "A free line about what this flight is, broadcast alongside the two IDs. "
      + "It is what somebody on the ground reads when they wonder why an "
      + "aircraft is over their field, so it is worth filling in even where "
      + "nothing requires it. Switching the type to Emergency is how an "
      + "operator says so while the aircraft is still in the air."));

    const grid = S.el("div", "pform-grid rid-grid-fields");
    grid.appendChild(selectRow(state, {
      key: "description_type", path: ["self_id"], label: "Type",
      options: options(state, "description_types", []),
    }));
    grid.appendChild(textRow(state, {
      key: "description", path: ["self_id"], label: "Description",
      maxLength: (schema(state).limits || {}).description || 23,
      placeholder: "e.g. Survey flight, Hall 7",
      info: "Short enough to read off a phone screen.",
    }));
    card.appendChild(grid);
    return card;
  }

  function operatorLocationCard(state) {
    const card = S.el("div", "page-card rid-card rid-location-card");
    card.dataset.section = "location";
    card.appendChild(S.sectionTitle("Operator position"));
    card.appendChild(S.el("div", "field-hint",
      "Where the broadcast says the person flying is standing. The take-off "
      + "point is what the aircraft fills in for itself; a fixed position is "
      + "one you type. The FAA's rule wants the control station's own "
      + "position, so the take-off point does not satisfy it."));

    const isFixed = state.identity.system.operator_location_type === 2;
    const grid = S.el("div", "pform-grid rid-grid-fields");
    grid.appendChild(selectRow(state, {
      key: "operator_location_type", path: ["system"], label: "Position source",
      options: options(state, "location_types", []),
    }));
    if (isFixed) {
      grid.appendChild(numberRow(state, {
        key: "operator_latitude", path: ["system"], label: "Latitude",
        unit: "°", step: 0.0000001, min: -90, max: 90,
      }));
      grid.appendChild(numberRow(state, {
        key: "operator_longitude", path: ["system"], label: "Longitude",
        unit: "°", step: 0.0000001, min: -180, max: 180,
      }));
      grid.appendChild(numberRow(state, {
        key: "operator_altitude_geo", path: ["system"], label: "Height above the ellipsoid",
        unit: "m", step: 1, min: -1000, max: 31767, unknown: ALTITUDE_UNKNOWN,
        placeholder: "not declared",
        info: "WGS-84 height, not height above ground. Leave it empty when you "
              + "do not know it. The broadcast has a value that means exactly "
              + "that, and a guessed number does not.",
      }));
    }
    card.appendChild(grid);

    if (isFixed) {
      const actions = S.el("div", "rid-card-actions");
      const status = S.el("span", "params-row-status");
      const take = Corvus.ui.button({
        variant: "ghost", size: "sm", icon: "map-pin",
        label: "Take the aircraft's position",
        className: "rid-take-position",
      });
      take.dataset.action = "take-position";
      take.disabled = !vehiclePosition(state);
      take.addEventListener("click", () => {
        const pos = vehiclePosition(state);
        if (!pos) {
          setFieldStatus(status, "err", "the aircraft has no position yet");
          return;
        }
        saveIdentity(state, { system: { operator_latitude: pos[0],
                                        operator_longitude: pos[1] } }, status)
          .then(() => { if (!state.destroyed) repaintIdentity(state); });
      });
      actions.appendChild(take);
      actions.appendChild(status);
      actions.appendChild(S.el("span", "field-hint",
        "An operator normally stands within a few metres of the aircraft "
        + "before take-off, which makes its position the fastest honest way to "
        + "fill these in."));
      card.appendChild(actions);
    }
    return card;
  }

  /** The vehicle's position, or null when nothing real has arrived. */
  function vehiclePosition(state) {
    const pos = (state.live || {}).position;
    if (!Array.isArray(pos) || pos.length < 2) return null;
    const lat = Number(pos[0]);
    const lon = Number(pos[1]);
    if (!isFinite(lat) || !isFinite(lon)) return null;
    // 0,0 is the store's "nothing yet", and it is also a real point in the
    // Atlantic. Treating it as a position would put an operator there.
    if (lat === 0 && lon === 0) return null;
    return [lat, lon];
  }

  function classificationCard(state) {
    const card = S.el("div", "page-card rid-card rid-class-card");
    card.dataset.section = "classification";
    card.appendChild(S.sectionTitle("EU vehicle info"));
    card.appendChild(S.el("div", "field-hint",
      "The European classification: which operational category the flight is "
      + "in, and which class mark the airframe carries. Declare it and both "
      + "are broadcast; leave it undeclared and neither is sent, which is the "
      + "right setting outside the EU. The operational volume underneath is "
      + "for the Specific category, where the flight is flown inside an "
      + "approved area rather than within sight."));

    const declared = state.identity.system.classification_type === 1;
    const grid = S.el("div", "pform-grid rid-grid-fields");
    grid.appendChild(selectRow(state, {
      key: "classification_type", path: ["system"], label: "Classification",
      options: options(state, "classification_types", []),
    }));
    if (declared) {
      grid.appendChild(selectRow(state, {
        key: "category_eu", path: ["system"], label: "Operational category",
        options: options(state, "categories_eu", []),
        info: "Open, Specific or Certified: the category the flight is "
              + "authorised under, not the aircraft's own class.",
      }));
      grid.appendChild(selectRow(state, {
        key: "class_eu", path: ["system"], label: "Class mark",
        options: options(state, "classes_eu", []),
        info: "The C0-C6 label on the airframe or in its declaration of "
              + "conformity.",
      }));
      grid.appendChild(numberRow(state, {
        key: "area_count", path: ["system"], label: "Operational areas",
        step: 1, min: 1, max: 65535,
        info: "How many volumes the flight is authorised in. 1 unless the "
              + "authorisation says otherwise.",
      }));
      grid.appendChild(numberRow(state, {
        key: "area_radius", path: ["system"], label: "Area radius",
        unit: "m", step: 1, min: 0, max: 65535,
        info: "Radius of the authorised volume around the aircraft. 0 declares "
              + "no radius.",
      }));
      grid.appendChild(numberRow(state, {
        key: "area_ceiling", path: ["system"], label: "Area ceiling",
        unit: "m", step: 1, min: -1000, max: 31767, unknown: ALTITUDE_UNKNOWN,
        placeholder: "not declared",
        info: "Upper limit of the authorised volume, above the ellipsoid.",
      }));
      grid.appendChild(numberRow(state, {
        key: "area_floor", path: ["system"], label: "Area floor",
        unit: "m", step: 1, min: -1000, max: 31767, unknown: ALTITUDE_UNKNOWN,
        placeholder: "not declared",
        info: "Lower limit of the authorised volume, above the ellipsoid.",
      }));
    }
    card.appendChild(grid);
    return card;
  }

  // -------------------------------------------------------------------------
  // Form rows
  // -------------------------------------------------------------------------

  /** Where a row's value lives: state.identity[...path][key]. */
  function readValue(state, path, key) {
    let node = state.identity;
    (path || []).forEach((step) => { node = (node && node[step]) || {}; });
    return node[key];
  }

  /** The nested patch a row saves: {system: {area_floor: 12}}. */
  function patchFor(path, key, value) {
    const leaf = {};
    leaf[key] = value;
    let out = leaf;
    for (let i = (path || []).length - 1; i >= 0; i -= 1) {
      const wrapper = {};
      wrapper[path[i]] = out;
      out = wrapper;
    }
    return out;
  }

  function writeLocal(state, path, key, value) {
    let node = state.identity;
    (path || []).forEach((step) => {
      if (!node[step]) node[step] = {};
      node = node[step];
    });
    node[key] = value;
  }

  function rowShell(state, spec) {
    const row = S.el("div", "pform-field rid-field");
    row.dataset.field = spec.key;
    row.appendChild(S.rowLabel("pform-field-label rid-field-label", spec.label, spec.info));
    const cell = S.el("div", "pform-field-control rid-field-control");
    row.appendChild(cell);
    return { row, cell };
  }

  function selectRow(state, spec) {
    const { row, cell } = rowShell(state, spec);
    const status = S.el("span", "params-row-status");
    const current = readValue(state, spec.path, spec.key);
    const select = Corvus.ui.select({
      className: "pform-select rid-select",
      ariaLabel: spec.label,
      options: (spec.options || []).map((o) => ({
        value: o.value,
        label: o.disabled && o.reason ? `${o.label} (${o.reason})` : o.label,
        disabled: !!o.disabled,
      })),
      value: spec.stringValue ? String(current) : Number(current),
    });
    select.addEventListener("change", () => {
      const next = spec.stringValue ? select.value : Number(select.value);
      writeLocal(state, spec.path, spec.key, next);
      saveIdentity(state, patchFor(spec.path, spec.key, next), status)
        // Several of these selects decide which fields exist at all — a fixed
        // operator position adds three, an undeclared EU classification removes
        // six — so the identity half is rebuilt once the vehicle-independent
        // save has landed.
        .then(() => { if (!state.destroyed) repaintIdentity(state); });
    });
    cell.appendChild(select);
    cell.appendChild(status);

    if (spec.suggestion) {
      const suggest = Corvus.ui.button({
        variant: "ghost", size: "sm", label: spec.suggestion.label,
        className: "rid-suggest",
      });
      suggest.dataset.action = "suggest-" + spec.key;
      suggest.addEventListener("click", () => {
        const next = spec.suggestion.value;
        writeLocal(state, spec.path, spec.key, next);
        saveIdentity(state, patchFor(spec.path, spec.key, next), status)
          .then(() => { if (!state.destroyed) repaintIdentity(state); });
      });
      row.appendChild(suggest);
    }
    return row;
  }

  function textRow(state, spec) {
    const { row, cell } = rowShell(state, spec);
    const status = S.el("span", "params-row-status");
    const current = String(readValue(state, spec.path, spec.key) || "");
    const input = Corvus.ui.input({
      className: "pform-input rid-input",
      mono: true,
      value: current,
      placeholder: spec.placeholder || "",
      ariaLabel: spec.label,
      autocomplete: false,
      spellcheck: false,
    });
    input.maxLength = spec.maxLength;

    const counter = S.el("span", "rid-counter", `${current.length}/${spec.maxLength}`);
    const repaintCounter = () => {
      counter.textContent = `${String(input.value || "").length}/${spec.maxLength}`;
    };

    input.addEventListener("input", () => {
      if (spec.uppercase) {
        const upper = String(input.value || "").toUpperCase();
        if (upper !== input.value) input.value = upper;
      }
      repaintCounter();
      const problem = spec.validate ? spec.validate(input.value) : "";
      if (problem) {
        input.classList.add("invalid");
        setFieldStatus(status, "err", problem);
      } else {
        input.classList.remove("invalid");
        setFieldStatus(status, "", "");
      }
    });
    input.addEventListener("change", () => {
      const value = String(input.value || "").trim();
      writeLocal(state, spec.path, spec.key, value);
      // Saved even when the live check is unhappy. The check is about the shape
      // of a filing, and a page that refused to remember a half-typed serial
      // number would lose it the moment the operator went to look at the label.
      saveIdentity(state, patchFor(spec.path, spec.key, value), status);
    });

    cell.appendChild(input);
    cell.appendChild(counter);
    cell.appendChild(status);
    return row;
  }

  function numberRow(state, spec) {
    const { row, cell } = rowShell(state, spec);
    const status = S.el("span", "params-row-status");
    const raw = Number(readValue(state, spec.path, spec.key));
    const isUnknown = spec.unknown != null && raw === spec.unknown;
    const input = Corvus.ui.input({
      className: "pform-input rid-input",
      mono: true,
      value: isUnknown ? "" : S.formatNumber(raw),
      placeholder: spec.placeholder || "",
      ariaLabel: spec.label,
      autocomplete: false,
    });
    input.step = String(spec.step);

    input.addEventListener("change", () => {
      const text = String(input.value || "").trim();
      if (text === "" && spec.unknown != null) {
        input.classList.remove("invalid");
        writeLocal(state, spec.path, spec.key, spec.unknown);
        saveIdentity(state, patchFor(spec.path, spec.key, spec.unknown), status);
        return;
      }
      const problem = S.rangeProblem({ min: spec.min, max: spec.max }, text);
      if (problem) {
        input.classList.add("invalid");
        setFieldStatus(status, "err", problem);
        return;
      }
      input.classList.remove("invalid");
      const value = Number(text);
      writeLocal(state, spec.path, spec.key, value);
      saveIdentity(state, patchFor(spec.path, spec.key, value), status);
    });

    cell.appendChild(input);
    if (spec.unit) cell.appendChild(S.el("span", "pform-unit", spec.unit));
    cell.appendChild(status);
    return row;
  }

  // -------------------------------------------------------------------------
  // Saving
  // -------------------------------------------------------------------------

  /**
   * Persist part of the identity.
   *
   * POST /api/config merges the remote_id key one level deep, so a patch
   * naming only `basic_id` leaves the operator registration alone. The backend
   * hands the result straight to the bridge, so the next 1 Hz cycle already
   * carries it — which is why the checks are re-read afterwards rather than at
   * the next reload.
   */
  function saveIdentity(state, patch, status) {
    setFieldStatus(status, "pending", "saving");
    return Corvus.telemetry.requestJson("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ remote_id: patch }),
    }).then((data) => {
      if (state.destroyed) return;
      const saved = (data && data.config && data.config.remote_id) || null;
      if (saved) state.identity = cloneIdentity(saved);
      setFieldStatus(status, "ok", "saved");
      return refreshChecks(state);
    }).catch((err) => {
      if (state.destroyed) return;
      const message = (err && err.message) || "save failed";
      setFieldStatus(status, "err", message);
      notify("critical", `Could not save the Remote ID identity: ${message}`);
    });
  }

  /**
   * Rebuild every card that renders the identity, leaving the rest alone.
   *
   * Called after a save that changes which fields exist — a position source,
   * an EU classification — rather than after every save, because a rebuild
   * takes the cursor out of whatever the operator was typing in.
   */
  function repaintIdentity(state) {
    if (state.destroyed || !state.host) return;
    repaintCard(state, ".rid-broadcast-card", broadcastCard);
    repaintCard(state, ".rid-basic-card", basicIdCard);
    repaintCard(state, ".rid-operator-card", operatorIdCard);
    repaintCard(state, ".rid-self-card", selfIdCard);
    repaintCard(state, ".rid-location-card", operatorLocationCard);
    repaintCard(state, ".rid-class-card", classificationCard);
  }

  // -------------------------------------------------------------------------
  // The vehicle's own parameters
  // -------------------------------------------------------------------------

  /**
   * One card per section the backend sent, rendered by the shared
   * schema-driven form. Nothing here knows a parameter name: PX4 answers with
   * COM_ARM_ODID and ArduPilot with the DID_ family, and this renders whichever
   * arrived.
   */
  function renderVehicleSections(state, host) {
    const sections = (state.doc && state.doc.sections) || [];
    if (!sections.length) return;
    sections.forEach((section) => {
      const card = S.el("div", "page-card rid-card rid-vehicle-card");
      card.dataset.section = section.id;
      card.appendChild(S.sectionTitle(section.title));
      if (section.hint) card.appendChild(S.el("div", "field-hint", section.hint));
      card.appendChild(S.paramFieldGrid(state, section.fields, { prefix: "rid" }));
      host.appendChild(card);
    });
  }

  return { render };
})();
