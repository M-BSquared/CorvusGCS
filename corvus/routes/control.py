"""Setup pages that drive the vehicle's actuators: RC and motors.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
ControlRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
from typing import Any
from .. import ardupilot_rc, rc_config
from ..http_routes import route

logger = logging.getLogger("corvus.server")


class ControlRoutes:
    @route("GET", "/api/motors")
    def _api_motors(self) -> None:
        """Return the vehicle's motor configuration as a renderable description.

        Reads only the ~100 parameters the Motors page needs (a named batch
        read, not the full ~1300-parameter download the editor uses) and hands
        them to :mod:`corvus.motor_config`, which owns the PX4 schema. The
        response lists only the fields the connected firmware actually answered
        for, so a parameter absent on v1.16 is one field fewer rather than an
        error (AGENTS.md: graceful fallback across v1.16/v1.17/v1.18).

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is.
        """
        schema = self._schema("motors")
        # The flight controller and the GPS antenna are placed relative to the
        # same centre of gravity as the motors, so they are drawn on the same
        # airframe and ride the same batched read.
        mounting = self._schema("mounting")
        if self.mavlink is None:
            payload = schema.build({})
            payload["sensors"] = mounting.positions({})
            payload["connected"] = False
            self._send_json(payload)
            return
        try:
            values = self._fetch_page_params(
                schema.param_names() + mounting.position_param_names())
            # A second, smaller read once the first has said which pins drive
            # a motor: their per-channel limits. A schema without per-channel
            # limits has no such function.
            followup = getattr(schema, "output_param_names", None)
            names = followup(values) if values and followup is not None else []
            if names:
                values = dict(values, **self._fetch_page_params(names))
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("motor parameter fetch failed")
            payload = schema.build({})
            payload["sensors"] = mounting.positions({})
            payload["connected"] = False
            payload["error"] = str(exc)
            self._send_json(payload)
            return
        payload = schema.build(values)
        payload["sensors"] = mounting.positions(values)
        payload["position_hint"] = mounting.POSITION_HINT
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)

    @route("GET", "/api/rc")
    def _api_rc(self) -> None:
        """Return the vehicle's transmitter configuration as a description.

        Same shape and same contract as ``/api/safety``, ``/api/motors`` and
        ``/api/tuning``: one batched read of the parameters
        :mod:`corvus.rc_config` knows about, handed to that module to turn into
        sections — the input mode and its failsafe, the stick channels, the
        flight-mode switch and its six slots, the remaining switches, the AUX
        passthroughs, and the per-channel calibration table.

        The channel pickers are capped at what the receiver actually delivers
        (``RC_CHANNELS.chancount``, via the telemetry store) rather than at
        PX4's eighteen, so an eight-channel radio does not present ten empty
        rows. When no RC has arrived the cap falls back to the full eighteen —
        an unknown receiver must not narrow the choices.

        A parameter the connected firmware does not answer for is one field
        fewer and an empty section disappears, so one schema serves v1.16,
        v1.17 and v1.18 without a version switch (AGENTS.md).

        Always 200 so the page can render a "not connected" state instead of an
        error banner; ``connected`` says which it is.
        """
        schema = self._schema("rc")
        empty = {"connected": False, "sections": [], "assignments": {},
                 "channel_limit": schema.MAX_CHANNELS, "received": 0}
        if self.mavlink is None:
            self._send_json(empty)
            return
        channels = schema.MAX_CHANNELS
        if self.store is not None:
            reported = self.store.get_snapshot().get("rc_channel_count") or 0
            if isinstance(reported, int) and 0 < reported <= schema.MAX_CHANNELS:
                channels = reported
        try:
            values = self._fetch_page_params(schema.param_names())
        except Exception as exc:  # noqa: BLE001 - a read must never 500 the page
            logger.exception("RC parameter fetch failed")
            self._send_json(dict(empty, error=str(exc)))
            return
        payload = schema.build(
            values, channels,
            **({"vehicle_type": self._vehicle_type_id()}
               if schema is ardupilot_rc else {}),
        )
        payload["connected"] = bool(values)
        if not values:
            payload["error"] = self.mavlink.get_last_command_error() or "no parameters received"
        self._send_json(payload)

    @route("POST", "/api/motors/assign")
    def _api_motors_assign(self, payload: dict) -> None:
        """Wire one motor to one output pin (Setup -> Motors, click-to-assign).

        PX4 stores the mapping pin-first (``PWM_MAIN_FUNC3 = 103``), so moving
        *Motor 3* to a different pin is two writes, not one: clear the pin it is
        on, then claim the new one. Doing that here rather than in the browser
        keeps the pair together and lets the outcome be judged as a whole.

        Three cases for a target pin that is already taken:
        free (``Disabled``) is a plain move; another *motor* is a swap, so the
        displaced motor lands on the pin this one vacated; anything else — a
        servo, a gimbal, a parachute — is refused by name, because silently
        moving a servo off its pin is how a control surface stops working.

        Body: ``{motor: 1-16, bank: "MAIN"|"AUX"|"CAN"|"SIM", pin: n}`` to assign, or
        ``{motor: n, output: null}`` to unassign.
        """
        schema = self._schema("motors")
        motor = payload.get("motor")
        if isinstance(motor, bool) or not isinstance(motor, int) or not (
                1 <= motor <= schema.MAX_MOTOR_FUNCTIONS):
            self._send_json({
                "ok": False,
                "error": f"motor must be between 1 and {schema.MAX_MOTOR_FUNCTIONS}",
            }, 400)
            return

        unassign = payload.get("output", False) is None
        target_param = None
        target_label = ""
        if not unassign:
            bank = payload.get("bank")
            pin = payload.get("pin")
            if not isinstance(bank, str) or isinstance(pin, bool) or not isinstance(pin, int):
                self._send_json({"ok": False, "error": "bank and pin are required"}, 400)
                return
            target_param = schema.function_param(bank, pin)
            if target_param is None:
                self._send_json({"ok": False, "error": f"unknown output {bank} {pin}"}, 400)
                return
            target_label = f"{bank} {pin}"

        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return

        values = self.mavlink.fetch_params(
            [e for e in schema.param_names() if "_FUNC" in e])
        if not values:
            error = self.mavlink.get_last_command_error() or "no output parameters received"
            self._send_json({"ok": False, "error": error}, 503)
            return
        if target_param is not None and target_param not in values:
            self._send_json(
                {"ok": False, "error": f"this board has no output {target_label}"}, 400)
            return

        entries = schema.outputs(values)
        source = next((e for e in entries if e["motor"] == motor), None)
        if target_param is not None and source is not None and source["param"] == target_param:
            self._send_json({"ok": True, "writes": [], "note": "already assigned"})
            return

        writes: list[tuple[str, float]] = []
        if target_param is None:
            if source is None:
                self._send_json({"ok": True, "writes": [], "note": "already unassigned"})
                return
            writes.append((source["param"], 0.0))
        else:
            occupant = next(e for e in entries if e["param"] == target_param)
            displaced = occupant["motor"]
            if displaced is None and int(round(occupant["value"])) != 0:
                self._send_json({
                    "ok": False,
                    "error": f"{target_label} drives {occupant['function']}. "
                             "Free it in Parameters before assigning a motor to it",
                }, 409)
                return
            if displaced is not None and source is None:
                self._send_json({
                    "ok": False,
                    "error": f"{target_label} already drives Motor {displaced}, and "
                             f"Motor {motor} has no output to swap it onto",
                }, 409)
                return
            # Source first: a motor briefly on no pin is a safer transient than
            # one briefly on two.
            if source is not None:
                vacated = (0.0 if displaced is None
                           else schema.motor_function_value(displaced))
                writes.append((source["param"], float(vacated or 0.0)))
            claimed = schema.motor_function_value(motor)
            if claimed is None:
                self._send_json({
                    "ok": False,
                    "error": f"this autopilot has no output function for Motor {motor}",
                }, 400)
                return
            writes.append((target_param, float(claimed)))

        applied: list[dict[str, Any]] = []
        for name, value in writes:
            if not self.mavlink.set_param(name, value):
                error = self.mavlink.get_last_command_error() or "parameter write failed"
                status = 503 if "not connected" in error else 409
                self._send_json({
                    "ok": False, "error": error, "applied": applied, "failed": name,
                }, status)
                return
            applied.append({"param": name, "value": value})
        logger.info("assigned motor %d -> %s", motor, target_label or "unassigned")
        self._send_json({"ok": True, "writes": applied})

    @route("POST", "/api/motors/test")
    def _api_motors_test(self, payload: dict) -> None:
        """Spin one motor on the bench so the operator can identify it.

        PROPELLERS MUST BE OFF — the UI will not enable this until the operator
        confirms that, and the bridge refuses while armed and bounds the
        duration so the *vehicle* stops the motor even if the link dies.
        """
        motor = payload.get("motor")
        if isinstance(motor, bool) or not isinstance(motor, int):
            self._send_json({"ok": False, "error": "motor must be an integer"}, 400)
            return
        throttle = payload.get("throttle", 15)
        if isinstance(throttle, bool) or not isinstance(throttle, (int, float)):
            self._send_json({"ok": False, "error": "throttle must be a number"}, 400)
            return
        duration = payload.get("duration", 2)
        if isinstance(duration, bool) or not isinstance(duration, (int, float)):
            self._send_json({"ok": False, "error": "duration must be a number"}, 400)
            return
        if not 0 <= float(throttle) <= 100:
            self._send_json(
                {"ok": False, "error": "throttle must be between 0 and 100 percent"}, 400)
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.motor_test(motor, float(throttle), float(duration)):
            self._send_json({"ok": True, "motor": motor, "throttle": float(throttle)})
            return
        error = self.mavlink.get_last_command_error() or "motor test failed"
        status = 503 if "not connected" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/motors/test/stop")
    def _api_motors_test_stop(self, payload: dict) -> None:
        """Stop every running motor test.

        Never gated on the armed state: this only ever stops a motor, so
        refusing it would be a safety regression (mirrors /api/calibrate/cancel).
        """
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.stop_motor_test():
            self._send_json({"ok": True})
            return
        error = self.mavlink.get_last_command_error() or "motor stop failed"
        self._send_json({"ok": False, "error": error}, 503)

    @route("POST", "/api/rc/stream")
    def _api_rc_stream(self, payload: dict) -> None:
        """Enable/disable the high-rate RC_CHANNELS stream on demand.

        Read-only stream-rate control, and safe while armed for the same reason
        the tuning one is: no command and no parameter write leaves the GCS.
        The Radio Control page toggles it on open and close, so the rest of the
        time PX4 streams channels at its own default rate.
        """
        enabled = payload.get("enabled")
        if not isinstance(enabled, bool):
            self._send_json({"ok": False, "error": "enabled must be boolean"}, 400)
            return
        raw_rate = payload.get("rate_hz", 20)
        # bool is a subclass of int — reject it so True is never coerced to 1 Hz
        if isinstance(raw_rate, bool) or not isinstance(raw_rate, (int, float)):
            self._send_json({"ok": False, "error": "rate_hz must be a number"}, 400)
            return
        rate_hz = int(raw_rate)
        if raw_rate != rate_hz or not 1 <= rate_hz <= 50:
            self._send_json(
                {"ok": False, "error": "rate_hz must be between 1 and 50 Hz"}, 400
            )
            return
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.mavlink.set_rc_stream(enabled, rate_hz):
            self._send_json({"ok": True, "enabled": enabled, "rate_hz": rate_hz})
            return
        error = self.mavlink.get_last_command_error() or "RC stream request failed"
        status = 503 if "not connected" in error else 409
        self._send_json({"ok": False, "error": error}, status)

    @route("POST", "/api/rc/calibrate")
    def _api_rc_calibrate(self, payload: dict) -> None:
        """Write a measured RC calibration to the vehicle.

        Neither PX4 nor ArduPilot has an autopilot-side RC calibration: unlike
        the accelerometer, the whole procedure belongs to the ground station,
        which watches RC_CHANNELS while the operator sweeps every control and
        then writes the endpoints it saw. This is the write half. The measuring
        half is the wizard in ``setup-control.js``; what arrives here is its
        result:

        ``{channels: [{channel, min, max, trim, reversed}], count, mapping}``,
        where ``mapping`` is the stick assignment the wizard learned by asking
        for one named stick at a time and watching which channel answered.

        Validation lives in the connected stack's ``calibration_writes`` — the
        two differ on where the reversal goes (``RC<n>_REV`` on PX4, a 0/1
        ``RC<n>_REVERSED`` on ArduPilot) and on whether there is a channel-count
        parameter at all — and is all-or-nothing on purpose: a rejected
        measurement leaves the vehicle exactly as it was, because a half-written
        endpoint set looks calibrated and is not. A write that fails part-way is reported with the parameters
        that did land, so the operator knows the radio is now inconsistent and
        must run the wizard again rather than fly it.

        Refused while armed by the bridge's own parameter gate; refused here
        first so the reason names the page rather than a parameter.
        """
        schema = self._schema("rc")
        if self.mavlink is None:
            self._send_json({"ok": False, "error": "not connected"}, 503)
            return
        if self.store is not None and self.store.get_snapshot().get("armed"):
            self._send_json(
                {"ok": False, "error": "cannot calibrate the radio while armed"}, 409)
            return

        count = payload.get("count")
        if count is not None and (isinstance(count, bool) or not isinstance(count, int)):
            self._send_json({"ok": False, "error": "count must be an integer"}, 400)
            return

        # The known-parameter set is what makes this version-tolerant: a write
        # to an RC<n>_REV a firmware does not have is dropped rather than sent
        # and reported as a failure the operator cannot act on.
        known = set(self.mavlink.fetch_params(schema.param_names()))
        if not known:
            error = self.mavlink.get_last_command_error() or "no parameters received"
            self._send_json({"ok": False, "error": error}, 503)
            return

        try:
            writes = schema.calibration_writes(
                payload.get("channels"), count, known, payload.get("mapping"))
        except rc_config.CalibrationError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return

        applied: list[dict[str, Any]] = []
        for write in writes:
            if not self.mavlink.set_param(write["name"], write["value"]):
                error = self.mavlink.get_last_command_error() or "parameter write failed"
                status = 503 if "not connected" in error else 409
                self._send_json({
                    "ok": False, "error": error, "applied": applied,
                    "failed": write["name"],
                }, status)
                return
            applied.append(write)
        logger.info("RC calibration written: %d parameters", len(applied))
        self._send_json({"ok": True, "writes": applied})
