"""Flight Review: what the plots say, and what they must never say.

A review that invents a verdict is worse than one that points at a plot, so
these tests are mostly about restraint — no findings that the log does not
support, no plot silently missing when the data is simply flat, and no
decimation that drops the one spike the review exists to catch.
"""
from __future__ import annotations

import math
import os
import struct

import pytest

from corvus.flight_review import MAX_POINTS, _decimate, review
from corvus.ulog import MAGIC, read


def _msg(msg_type: str, body: bytes) -> bytes:
    return struct.pack("<HB", len(body), ord(msg_type)) + body


def _build(*parts: bytes) -> bytes:
    return MAGIC + b"\x01" + struct.pack("<Q", 0) + b"".join(parts)


def _fmt(text: str) -> bytes:
    return _msg("F", text.encode())


def _add(msg_id: int, name: str, multi_id: int = 0) -> bytes:
    return _msg("A", bytes([multi_id]) + struct.pack("<H", msg_id) + name.encode())


def _row(msg_id: int, payload: bytes) -> bytes:
    return _msg("D", struct.pack("<H", msg_id) + payload)


def _plot(result: dict, plot_id: str) -> dict | None:
    return next((p for p in result["plots"] if p["id"] == plot_id), None)


def _says(finding: dict) -> str:
    """Everything a finding says — the headline and the reasoning behind it.

    The two are separate fields because the page shows the headline and keeps
    the reasoning a click away; a test asking whether the review *said*
    something should not care which half it landed in.
    """
    return finding["text"] + " " + finding.get("detail", "")


# ---------------------------------------------------------------------------
# Decimation
# ---------------------------------------------------------------------------

def test_decimation_keeps_the_spike_stride_sampling_would_drop() -> None:
    """The whole job is finding the one frame where something saturated.

    Taking every n-th sample is precisely the filter that loses it.
    """
    count = MAX_POINTS * 10
    values = [0.0] * count
    values[count // 2 + 3] = 99.0          # a spike between stride samples
    times = [float(i) for i in range(count)]

    _, out = _decimate(times, values)
    assert len(out) <= MAX_POINTS
    assert max(out) == 99.0, "the spike survived the reduction"


def test_short_series_are_passed_through_untouched() -> None:
    times, values = [1.0, 2.0], [3.0, 4.0]
    assert _decimate(times, values) == (times, values)


# ---------------------------------------------------------------------------
# Plots
# ---------------------------------------------------------------------------

def test_ekf_test_ratios_are_plotted_with_the_rejection_threshold() -> None:
    blob = _build(
        _fmt("estimator_status:uint64_t timestamp;float mag_test_ratio;float vel_test_ratio;"),
        _add(1, "estimator_status"),
        _row(1, struct.pack("<Qff", 0, 0.2, 0.3)),
        _row(1, struct.pack("<Qff", 1_000_000, 1.8, 0.4)),
    )
    result = review(read(blob), "x.ulg")
    plot = _plot(result, "ekf")
    assert plot is not None
    assert [s["name"] for s in plot["series"]] == ["Magnetometer", "Velocity"]
    # 1.0 is the line between "normal" and "the estimator stopped believing a
    # sensor", and a plot of test ratios without it is decoration.
    assert plot["threshold"] == 1.0
    assert any("innovations reached 1.80" in f["text"] for f in result["findings"])


def test_clipping_is_reported_as_critical() -> None:
    """A saturated IMU lies to the estimator, so everything downstream of it in
    the same log is suspect — that has to be said, not left to the plot."""
    blob = _build(
        _fmt("vehicle_imu_status:uint64_t timestamp;uint32_t[3] accel_clipping;"),
        _add(1, "vehicle_imu_status"),
        _row(1, struct.pack("<Q3I", 0, 0, 0, 0)),
        _row(1, struct.pack("<Q3I", 1_000_000, 5, 2, 0)),
    )
    result = review(read(blob), "x.ulg")
    assert _plot(result, "clipping") is not None
    finding = next(f for f in result["findings"] if "clipping" in f["text"])
    assert finding["level"] == "critical"


def test_a_clean_imu_produces_no_clipping_plot_and_no_alarm() -> None:
    blob = _build(
        _fmt("vehicle_imu_status:uint64_t timestamp;uint32_t[3] accel_clipping;"),
        _add(1, "vehicle_imu_status"),
        _row(1, struct.pack("<Q3I", 0, 0, 0, 0)),
    )
    result = review(read(blob), "x.ulg")
    assert _plot(result, "clipping") is None
    # Checked by level, not by keyword: the all-clear line mentions clipping
    # too, and a test that greps for the word passes on the wrong sentence.
    assert [f["level"] for f in result["findings"]] == ["ok"]


def test_attitude_is_converted_from_the_logged_quaternion() -> None:
    # 90° roll: q = (cos45, sin45, 0, 0)
    half = 0.7071067811865476
    blob = _build(
        _fmt("vehicle_attitude:uint64_t timestamp;float[4] q;"),
        _add(1, "vehicle_attitude"),
        _row(1, struct.pack("<Q4f", 0, half, half, 0.0, 0.0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "att_roll")
    assert plot is not None
    roll = next(s for s in plot["series"] if s["name"] == "Roll estimate")
    assert roll["y"][0] == pytest.approx(90.0, abs=0.01)


def test_unassigned_actuator_channels_are_left_out() -> None:
    """`noutputs` says how many the mixer drives; the rest is array padding and
    plotting it buries the four that matter under four flat lines."""
    blob = _build(
        _fmt("actuator_outputs:uint64_t timestamp;uint32_t noutputs;float[8] output;"),
        _add(1, "actuator_outputs"),
        _row(1, struct.pack("<QI8f", 0, 4, 1000, 1100, 1200, 1300, 0, 0, 0, 0)),
        _row(1, struct.pack("<QI8f", 1_000_000, 4, 1500, 1400, 1600, 1450, 0, 0, 0, 0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "actuators")
    assert plot is not None
    assert [s["name"] for s in plot["series"]] == [
        "Output 1", "Output 2", "Output 3", "Output 4"]


def test_flat_outputs_still_plot_and_say_the_motors_never_ran() -> None:
    """A missing plot reads as "not implemented"; a flat one is an answer."""
    blob = _build(
        _fmt("actuator_outputs:uint64_t timestamp;uint32_t noutputs;float[8] output;"),
        _add(1, "actuator_outputs"),
        _row(1, struct.pack("<QI8f", 0, 4, 900, 900, 900, 900, 0, 0, 0, 0)),
        _row(1, struct.pack("<QI8f", 1_000_000, 4, 900, 900, 900, 900, 0, 0, 0, 0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "actuators")
    assert plot is not None
    assert "did not run" in plot["note"]


def test_altitude_is_flipped_out_of_ned() -> None:
    """Local position is NED — down is positive, and nobody reads it that way."""
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float z;"),
        _add(1, "vehicle_local_position"),
        _row(1, struct.pack("<Qf", 0, -12.0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "altitude")
    assert plot["series"][0]["y"][0] == pytest.approx(12.0)


def test_angular_rates_are_converted_to_degrees_and_paired_with_setpoints() -> None:
    """The tuning plot: rate against demand, in the unit gains are reasoned in."""
    import math
    blob = _build(
        _fmt("vehicle_angular_velocity:uint64_t timestamp;float[3] xyz;"),
        _fmt("vehicle_rates_setpoint:uint64_t timestamp;float roll;float pitch;float yaw;"),
        _add(1, "vehicle_angular_velocity"), _add(2, "vehicle_rates_setpoint"),
        _row(1, struct.pack("<Q3f", 0, math.pi, 0.0, 0.0)),
        _row(2, struct.pack("<Q3f", 0, math.pi / 2, 0.0, 0.0)),
    )
    # One plot per axis, not three axes on one: the question is asked per
    # axis, and six lines on one pair of axes is unreadable.
    plot = _plot(review(read(blob), "x.ulg"), "rate_roll")
    assert plot["group"] == "Control"
    roll = next(s for s in plot["series"] if s["name"] == "Roll rate")
    assert roll["y"][0] == pytest.approx(180.0, abs=0.01)
    setpoint = next(s for s in plot["series"] if s["name"] == "Roll rate setpoint")
    assert setpoint["y"][0] == pytest.approx(90.0, abs=0.01)


def test_the_ground_track_is_north_over_east_with_equal_axes() -> None:
    """The one plot that is not against time — and a track drawn on unequal
    axes is a track of a different shape."""
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;float y;"),
        _add(1, "vehicle_local_position"),
        _row(1, struct.pack("<Qff", 0, 0.0, 0.0)),
        _row(1, struct.pack("<Qff", 1_000_000, 10.0, 5.0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "track")
    assert plot is not None
    assert plot["equal"] is True
    assert plot["xlabel"] == "East (m)"
    path = next(s for s in plot["series"] if s["name"] == "Estimated")
    assert path["x"] == [0.0, 5.0], "east on x"
    assert path["y"] == [0.0, 10.0], "north on y"


def test_a_stationary_log_gets_no_ground_track() -> None:
    """A dot is not a track, and an empty plot invites the wrong conclusion."""
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;float y;"),
        _add(1, "vehicle_local_position"),
        _row(1, struct.pack("<Qff", 0, 0.0, 0.0)),
        _row(1, struct.pack("<Qff", 1_000_000, 0.05, 0.02)),
    )
    assert _plot(review(read(blob), "x.ulg"), "track") is None


REF_LAT, REF_LON = 47.397742, 8.545594


def _flown(gps: bool = True, waypoints: bool = True, global_ref: bool = True,
           fix_type: int = 3) -> bytes:
    """A short flight: estimate, its setpoint, GPS fixes and one waypoint."""
    from corvus.flight_review import _Projection

    projection = _Projection(REF_LAT, REF_LON)
    parts = [
        _fmt("vehicle_local_position:uint64_t timestamp;float x;float y;"
             "double ref_lat;double ref_lon;bool xy_global;"),
        _add(1, "vehicle_local_position"),
        _fmt("vehicle_local_position_setpoint:uint64_t timestamp;float x;float y;"),
        _add(2, "vehicle_local_position_setpoint"),
    ]
    if gps:
        parts += [_fmt("vehicle_gps_position:uint64_t timestamp;int32_t lat;"
                       "int32_t lon;uint8_t fix_type;"),
                  _add(3, "vehicle_gps_position")]
    if waypoints:
        parts += [_fmt("position_setpoint_triplet:uint64_t timestamp;"
                       "double current.lat;double current.lon;bool current.valid;"),
                  _add(4, "position_setpoint_triplet")]
    for step in range(8):
        stamp = step * 500_000
        lat = REF_LAT + step * 0.0001
        lon = REF_LON + step * 0.0001
        north, east = projection.project(lat, lon)
        parts.append(_row(1, struct.pack(
            "<QffddB", stamp, north, east,
            REF_LAT if global_ref else 0.0, REF_LON if global_ref else 0.0,
            global_ref)))
        parts.append(_row(2, struct.pack("<Qff", stamp, north + 1.0, east + 1.0)))
        if gps:
            parts.append(_row(3, struct.pack(
                "<QiiB", stamp, int(lat * 1e7), int(lon * 1e7), fix_type)))
        if waypoints:
            # Republished every cycle, as PX4 does: the same waypoint over and
            # over until the mission advances.
            parts.append(_row(4, struct.pack(
                "<QddB", stamp, REF_LAT + 0.001, REF_LON + 0.001, True)))
    return _build(*parts)


def _track_series(blob: bytes) -> dict[str, dict]:
    plot = _plot(review(read(blob), "x.ulg"), "track")
    assert plot is not None
    return {s["name"]: s for s in plot["series"]}


def test_the_projection_is_px4s_own_not_a_flat_earth_stand_in() -> None:
    """The estimator produced ``vehicle_local_position`` with this transform.
    Any other one puts the projected fixes metres away from the estimate at
    range, and that gap would be read as estimator error."""
    from corvus.flight_review import _Projection

    projection = _Projection(REF_LAT, REF_LON)
    assert projection.project(REF_LAT, REF_LON) == (0.0, 0.0)
    north, east = projection.project(REF_LAT + 0.001, REF_LON)
    assert north == pytest.approx(111.19, abs=0.05)
    assert east == pytest.approx(0.0, abs=1e-6)
    # East shrinks with the cosine of the latitude; at 47° it is about 0.68.
    _, east = projection.project(REF_LAT, REF_LON + 0.001)
    assert east == pytest.approx(111.19 * math.cos(math.radians(REF_LAT)), abs=0.1)


def test_the_track_overlays_estimate_setpoint_gps_and_waypoints() -> None:
    series = _track_series(_flown())
    assert set(series) == {"Estimated", "Setpoint", "GPS (projected)",
                           "Commanded position"}
    # Projected through the estimator's own origin, so the fixes land on the
    # estimate rather than beside it.
    assert series["GPS (projected)"]["x"] == pytest.approx(
        series["Estimated"]["x"], abs=0.02)
    assert series["GPS (projected)"]["y"] == pytest.approx(
        series["Estimated"]["y"], abs=0.02)


def test_the_estimate_is_drawn_over_its_references_not_under_them() -> None:
    """Plotly draws later traces on top. A noisy GPS trace laid over the
    estimate hides the one line the plot is about."""
    plot = _plot(review(read(_flown()), "x.ulg"), "track")
    assert [s["name"] for s in plot["series"]] == [
        "GPS (projected)", "Setpoint", "Estimated", "Commanded position"]


def test_repeated_waypoints_collapse_to_the_points_actually_commanded() -> None:
    """The triplet is republished every cycle. Drawn as a path, it would show
    legs the aircraft was never asked to fly."""
    series = _track_series(_flown())
    commanded = series["Commanded position"]
    assert commanded["draw"] == "markers", "points, not a route"
    assert len(commanded["x"]) == 1, "eight republished rows, one waypoint"


def test_a_flight_without_a_global_reference_gets_no_gps_track() -> None:
    """Indoors, ref_lat/ref_lon are zero. Projecting against them would draw a
    track in the Gulf of Guinea and overlay it as if it disagreed."""
    series = _track_series(_flown(global_ref=False))
    assert "GPS (projected)" not in series
    assert "Estimated" in series
    plot = _plot(review(read(_flown(global_ref=False)), "x.ulg"), "track")
    assert "no global reference" in plot["note"]


def test_fixes_without_a_3d_lock_are_left_out() -> None:
    """A 2D or dead-reckoned fix has no business anchoring a track."""
    assert "GPS (projected)" not in _track_series(_flown(fix_type=2))


def test_magnetometer_norm_is_computed_from_the_vector() -> None:
    blob = _build(
        _fmt("vehicle_magnetometer:uint64_t timestamp;float[3] magnetometer_ga;"),
        _add(1, "vehicle_magnetometer"),
        _row(1, struct.pack("<Q3f", 0, 3.0, 4.0, 0.0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "mag")
    assert plot["series"][0]["y"][0] == pytest.approx(5.0)


def test_climb_rate_is_flipped_out_of_ned() -> None:
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;float y;"
             "float vx;float vy;float vz;"),
        _add(1, "vehicle_local_position"),
        _row(1, struct.pack("<Q5f", 0, 0, 0, 3.0, 4.0, -2.0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "velocity")
    ground = next(s for s in plot["series"] if s["name"] == "Ground speed")
    climb = next(s for s in plot["series"] if s["name"] == "Climb rate")
    assert ground["y"][0] == pytest.approx(5.0)
    assert climb["y"][0] == pytest.approx(2.0), "climbing reads as a climb"


def test_plots_are_grouped_and_the_group_list_matches() -> None:
    """A review with two dozen plots is a scroll unless it is sectioned."""
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;float y;"),
        _fmt("cpuload:uint64_t timestamp;float load;float ram_usage;"),
        _add(1, "vehicle_local_position"), _add(2, "cpuload"),
        _row(1, struct.pack("<Qff", 0, 0.0, 0.0)),
        _row(1, struct.pack("<Qff", 1_000_000, 30.0, 20.0)),
        _row(2, struct.pack("<Qff", 0, 0.4, 0.5)),
    )
    result = review(read(blob), "x.ulg")
    assert result["groups"] == ["Flight", "System"], "declared in reading order"
    for plot in result["plots"]:
        assert plot["group"] in result["groups"], "no plot lands in a group nobody lists"


def test_an_airspeed_plot_is_absent_on_a_multirotor_log() -> None:
    """Version and airframe tolerance: a missing topic is not an error."""
    blob = _build(
        _fmt("cpuload:uint64_t timestamp;float load;float ram_usage;"),
        _add(1, "cpuload"), _row(1, struct.pack("<Qff", 0, 0.1, 0.2)),
    )
    result = review(read(blob), "x.ulg")
    assert _plot(result, "airspeed") is None
    assert _plot(result, "cpu") is not None


# ---------------------------------------------------------------------------
# Flight modes
# ---------------------------------------------------------------------------

def test_flight_modes_become_intervals() -> None:
    """Every plot is read against the mode it was flown in — the same
    oscillation in Position and in Manual are different findings."""
    blob = _build(
        _fmt("vehicle_status:uint64_t timestamp;uint8_t nav_state;uint8_t arming_state;"),
        _add(1, "vehicle_status"),
        _row(1, struct.pack("<QBB", 0, 0, 1)),              # Manual, disarmed
        _row(1, struct.pack("<QBB", 2_000_000, 0, 2)),      # Manual, armed
        _row(1, struct.pack("<QBB", 4_000_000, 2, 2)),      # Position, armed
        _row(1, struct.pack("<QBB", 8_000_000, 5, 2)),      # Return, armed
    )
    result = review(read(blob), "x.ulg")
    # Manual 0–4 s, Position 4–8 s. The final Return span is zero-length (the
    # log ends on the transition) and is dropped by the minimum-duration rule.
    assert [(m["mode"], m["start"], m["end"]) for m in result["modes"]] == [
        ("Manual", 0.0, 4.0), ("Position", 4.0, 8.0),
    ]
    assert result["summary"]["modes_flown"] == ["Manual", "Position"]
    # Armed time is the part of the log that flew.
    assert result["armed"] == [{"start": 2.0, "end": 8.0}]
    assert result["summary"]["armed_s"] == 6.0


def test_the_two_mode_enums_are_not_confused() -> None:
    """`vehicle_status` carries NAVIGATION_STATE_*, the older `commander_state`
    carries MAIN_STATE_*, and 6 means Position-slow in one and Acro in the
    other. Reading one table for the other mislabels most of a flight."""
    def _modes(topic: str, field: str, value: int) -> list:
        blob = _build(
            _fmt(f"{topic}:uint64_t timestamp;uint8_t {field};"),
            _add(1, topic),
            _row(1, struct.pack("<QB", 0, value)),
            _row(1, struct.pack("<QB", 2_000_000, value)),
        )
        return review(read(blob), "x.ulg")["modes"]

    assert _modes("vehicle_status", "nav_state", 6)[0]["mode"] == "Position slow"
    assert _modes("commander_state", "main_state", 6)[0]["mode"] == "Acro"


def test_a_frozen_clock_produces_no_bands_rather_than_wrong_ones() -> None:
    """Some PX4 versions publish `commander_state` without updating its
    timestamp. Bands placed from a stopped clock would relabel the whole
    flight, which is worse than having none."""
    blob = _build(
        _fmt("commander_state:uint64_t timestamp;uint8_t main_state;"),
        _add(1, "commander_state"),
        _row(1, struct.pack("<QB", 2_069_758, 0)),
        _row(1, struct.pack("<QB", 2_069_758, 2)),
        _row(1, struct.pack("<QB", 2_069_758, 0)),
    )
    assert review(read(blob), "x.ulg")["modes"] == []


def test_a_frozen_clock_does_not_become_the_time_base() -> None:
    """The bug this guards: one topic with a stuck timestamp shifted every
    plot in the review by the distance between its frozen value and the real
    start of the log."""
    blob = _build(
        _fmt("commander_state:uint64_t timestamp;uint8_t main_state;"),
        _fmt("cpuload:uint64_t timestamp;float load;float ram_usage;"),
        _add(1, "commander_state"), _add(2, "cpuload"),
        _row(1, struct.pack("<QB", 2_000_000, 0)),
        _row(1, struct.pack("<QB", 2_000_000, 0)),
        _row(2, struct.pack("<Qff", 112_000_000, 0.4, 0.5)),
        _row(2, struct.pack("<Qff", 113_000_000, 0.5, 0.5)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "cpu")
    assert plot["series"][0]["x"][0] == 0.0, "the running clock sets t=0"
    assert plot["series"][0]["x"][-1] == pytest.approx(1.0)


def test_momentary_mode_flickers_are_not_drawn() -> None:
    """A mode held for milliseconds during a transition is a sliver of colour
    that says nothing."""
    blob = _build(
        _fmt("vehicle_status:uint64_t timestamp;uint8_t nav_state;uint8_t arming_state;"),
        _add(1, "vehicle_status"),
        _row(1, struct.pack("<QBB", 0, 0, 1)),
        _row(1, struct.pack("<QBB", 5_000_000, 4, 1)),        # 10 ms of Loiter
        _row(1, struct.pack("<QBB", 5_010_000, 2, 1)),
        _row(1, struct.pack("<QBB", 9_000_000, 2, 1)),
    )
    modes = [m["mode"] for m in review(read(blob), "x.ulg")["modes"]]
    assert "Loiter" not in modes
    assert modes == ["Manual", "Position"]


# ---------------------------------------------------------------------------
# Estimation sources
# ---------------------------------------------------------------------------

def test_altitude_sources_are_compared_on_one_axis() -> None:
    """Each source fails its own way; the only way to see which one is lying is
    to put them side by side."""
    blob = _build(
        _fmt("vehicle_global_position:uint64_t timestamp;float alt;"),
        _fmt("vehicle_gps_position:uint64_t timestamp;int32_t alt;"),
        _fmt("vehicle_air_data:uint64_t timestamp;float baro_alt_meter;"),
        _add(1, "vehicle_global_position"), _add(2, "vehicle_gps_position"),
        _add(3, "vehicle_air_data"),
        _row(1, struct.pack("<Qf", 0, 512.0)),
        _row(2, struct.pack("<Qi", 0, 513_000)),          # millimetres
        _row(3, struct.pack("<Qf", 0, 510.5)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "alt_sources")
    assert plot["group"] == "Estimator"
    by_name = {s["name"]: s["y"][0] for s in plot["series"]}
    assert by_name["Fused estimate"] == pytest.approx(512.0)
    # GPS altitude is logged in millimetres; getting that wrong puts one trace
    # a thousand times off and makes the comparison meaningless.
    assert by_name["GPS altitude (MSL)"] == pytest.approx(513.0)
    assert by_name["Barometer altitude"] == pytest.approx(510.5)


def test_a_single_altitude_source_is_not_called_a_comparison() -> None:
    blob = _build(
        _fmt("vehicle_air_data:uint64_t timestamp;float baro_alt_meter;"),
        _add(1, "vehicle_air_data"),
        _row(1, struct.pack("<Qf", 0, 510.5)),
    )
    result = review(read(blob), "x.ulg")
    assert _plot(result, "alt_sources") is None
    assert _plot(result, "baro") is not None, "but the barometer still gets its own plot"


def test_gps_and_estimator_velocity_are_compared() -> None:
    blob = _build(
        _fmt("vehicle_gps_position:uint64_t timestamp;float vel_n_m_s;float vel_e_m_s;"),
        _fmt("vehicle_local_position:uint64_t timestamp;float x;float y;float vx;float vy;"),
        _add(1, "vehicle_gps_position"), _add(2, "vehicle_local_position"),
        _row(1, struct.pack("<Qff", 0, 3.0, 4.0)),
        _row(2, struct.pack("<Q4f", 0, 0.0, 0.0, 6.0, 8.0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "gps_velocity")
    by_name = {s["name"]: s["y"][0] for s in plot["series"]}
    assert by_name["GPS"] == pytest.approx(5.0)
    assert by_name["Estimator"] == pytest.approx(10.0)


def test_each_axis_gets_its_own_angle_and_rate_plot() -> None:
    """Six plots, not two. "Is roll tracking?" is asked per axis, and three
    estimates plus three setpoints on one pair of axes is unreadable."""
    import math
    half = 0.7071067811865476
    blob = _build(
        _fmt("vehicle_attitude:uint64_t timestamp;float[4] q;"),
        _fmt("vehicle_angular_velocity:uint64_t timestamp;float[3] xyz;"),
        _add(1, "vehicle_attitude"), _add(2, "vehicle_angular_velocity"),
        _row(1, struct.pack("<Q4f", 0, half, half, 0.0, 0.0)),
        _row(2, struct.pack("<Q3f", 0, 0.0, 0.0, math.pi)),
    )
    result = review(read(blob), "x.ulg")
    ids = {p["id"] for p in result["plots"]}
    assert {"att_roll", "att_pitch", "att_yaw",
            "rate_roll", "rate_pitch", "rate_yaw"} <= ids
    yaw_rate = _plot(result, "rate_yaw")["series"][0]
    assert yaw_rate["y"][0] == pytest.approx(180.0, abs=0.01)


def test_local_position_and_velocity_carry_their_setpoints() -> None:
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;float y;float z;"
             "float vx;float vy;float vz;"),
        _fmt("vehicle_local_position_setpoint:uint64_t timestamp;float x;float y;"
             "float z;float vx;float vy;float vz;"),
        _add(1, "vehicle_local_position"), _add(2, "vehicle_local_position_setpoint"),
        _row(1, struct.pack("<Q6f", 0, 10.0, 20.0, -30.0, 1.0, 2.0, -3.0)),
        _row(2, struct.pack("<Q6f", 0, 11.0, 21.0, -31.0, 1.5, 2.5, -3.5)),
    )
    result = review(read(blob), "x.ulg")
    x = _plot(result, "pos_x")
    assert [s["name"] for s in x["series"]] == ["Estimate", "Setpoint"]
    assert x["series"][0]["y"][0] == pytest.approx(10.0)
    # Z and vertical velocity are flipped out of NED so up reads as up.
    assert _plot(result, "pos_z")["series"][0]["y"][0] == pytest.approx(30.0)
    assert _plot(result, "vel_z")["series"][0]["y"][0] == pytest.approx(3.0)


def test_manual_control_reads_both_field_spellings() -> None:
    """PX4 renamed the stick axes from x/y/z/r to pitch/roll/throttle/yaw, and
    both spellings appear across the supported firmware range."""
    def _sticks(fields: str, values: tuple) -> dict:
        blob = _build(
            _fmt("manual_control_setpoint:uint64_t timestamp;" + fields),
            _add(1, "manual_control_setpoint"),
            _row(1, struct.pack("<Q4f", 0, *values)),
        )
        plot = _plot(review(read(blob), "x.ulg"), "manual")
        return {s["name"]: s["y"][0] for s in plot["series"]}

    modern = _sticks("float roll;float pitch;float yaw;float throttle;",
                     (0.1, 0.2, 0.3, 0.4))
    legacy = _sticks("float y;float x;float r;float z;", (0.1, 0.2, 0.3, 0.4))
    assert modern["Roll stick"] == pytest.approx(0.1)
    assert legacy["Roll stick"] == pytest.approx(0.1)
    assert modern["Throttle"] == pytest.approx(0.4)
    assert legacy["Throttle"] == pytest.approx(0.4)


def test_the_altitude_setpoint_is_drawn_as_markers_in_absolute_altitude() -> None:
    """Sparse and stepped: a line through it would imply values that were
    never commanded. And it has to share the axis with the other three, which
    means lifting the NED setpoint onto the local frame's AMSL origin."""
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float z;float ref_alt;"),
        _fmt("vehicle_local_position_setpoint:uint64_t timestamp;float z;"),
        _fmt("vehicle_air_data:uint64_t timestamp;float baro_alt_meter;"),
        _add(1, "vehicle_local_position"), _add(2, "vehicle_local_position_setpoint"),
        _add(3, "vehicle_air_data"),
        _row(1, struct.pack("<Qff", 0, -20.0, 500.0)),
        _row(2, struct.pack("<Qf", 0, -25.0)),
        _row(3, struct.pack("<Qf", 0, 519.0)),
    )
    plot = _plot(review(read(blob), "x.ulg"), "alt_sources")
    setpoint = next(s for s in plot["series"] if s["name"] == "Altitude setpoint")
    assert setpoint["draw"] == "markers"
    assert setpoint["y"][0] == pytest.approx(525.0), "ref_alt + 25 m up"


def test_the_modes_get_no_plot_of_their_own() -> None:
    """The bands behind every plot name their mode in place, so a separate
    timeline said the same thing a third time and cost a screen doing it."""
    blob = _build(
        _fmt("vehicle_status:uint64_t timestamp;uint8_t nav_state;uint8_t arming_state;"),
        _add(1, "vehicle_status"),
        _row(1, struct.pack("<QBB", 0, 0, 1)),
        _row(1, struct.pack("<QBB", 4_000_000, 2, 2)),
        _row(1, struct.pack("<QBB", 9_000_000, 2, 2)),
    )
    result = review(read(blob), "x.ulg")
    assert _plot(result, "modes") is None
    # Still reported: the strip and the bands are built from this.
    assert [span["mode"] for span in result["modes"]] == ["Manual", "Position"]
    # And still counted, though no plot draws the topic it came from.
    assert result["summary"]["duration_s"] == 9.0


def test_gps_uncertainty_and_noise_are_separate_plots() -> None:
    blob = _build(
        _fmt("vehicle_gps_position:uint64_t timestamp;float eph;float epv;"
             "uint16_t noise_per_ms;uint16_t jamming_indicator;uint8_t fix_type;"),
        _add(1, "vehicle_gps_position"),
        _row(1, struct.pack("<QffHHB", 0, 0.8, 1.2, 90, 40, 3)),
    )
    result = review(read(blob), "x.ulg")
    assert [s["name"] for s in _plot(result, "gps_accuracy")["series"]][:2] == [
        "Horizontal (eph)", "Vertical (epv)"]
    assert [s["name"] for s in _plot(result, "gps_noise")["series"]] == [
        "Noise per ms", "Jamming indicator"]
    assert [s["name"] for s in _plot(result, "gps_quality")["series"]] == ["Fix type"]


# ---------------------------------------------------------------------------
# Findings
# ---------------------------------------------------------------------------

def test_a_clean_log_says_so_rather_than_inventing_a_problem() -> None:
    blob = _build(
        _fmt("cpuload:uint64_t timestamp;float load;float ram_usage;"),
        _add(1, "cpuload"),
        _row(1, struct.pack("<Qff", 0, 0.3, 0.4)),
    )
    findings = review(read(blob), "x.ulg")["findings"]
    assert len(findings) == 1
    assert findings[0]["level"] == "ok"


def test_an_imbalanced_airframe_is_named_as_such() -> None:
    """One motor working harder than the others is a bent arm or a heavy
    corner — telling the operator to retune would send them the wrong way."""
    rows = []
    for i in range(20):
        rows.append(_row(1, struct.pack(
            "<QI8f", i * 100_000, 4, 1800, 1200, 1200, 1200, 0, 0, 0, 0)))
    blob = _build(
        _fmt("actuator_outputs:uint64_t timestamp;uint32_t noutputs;float[8] output;"),
        _add(1, "actuator_outputs"), *rows,
    )
    findings = review(read(blob), "x.ulg")["findings"]
    imbalance = next(f for f in findings if "uneven" in f["text"])
    assert "Output 1" in imbalance["text"]
    assert "airframe imbalance" in _says(imbalance)


def test_high_vibration_is_graded_not_just_reported() -> None:
    def _vibe(peak: float) -> list:
        return review(read(_build(
            _fmt("estimator_status:uint64_t timestamp;float[3] vibe;"),
            _add(1, "estimator_status"),
            _row(1, struct.pack("<Q3f", 0, 0.0, 0.0, 0.0)),
            _row(1, struct.pack("<Q3f", 1_000_000, peak, 0.0, 0.0)),
        )), "x.ulg")["findings"]

    assert all("Vibration" not in f["text"] for f in _vibe(5.0)), "quiet is not a finding"
    warn = next(f for f in _vibe(40.0) if "Vibration" in f["text"])
    assert warn["level"] == "warning"
    bad = next(f for f in _vibe(80.0) if "Vibration" in f["text"])
    assert bad["level"] == "critical"


def test_an_rc_signal_loss_is_flagged() -> None:
    blob = _build(
        _fmt("input_rc:uint64_t timestamp;float rssi;float rc_lost;"),
        _add(1, "input_rc"),
        _row(1, struct.pack("<Qff", 0, 100.0, 0.0)),
        _row(1, struct.pack("<Qff", 1_000_000, 10.0, 1.0)),
    )
    findings = review(read(blob), "x.ulg")["findings"]
    assert any("signal loss" in f["text"] for f in findings)


def test_a_sagging_pack_is_flagged() -> None:
    blob = _build(
        _fmt("battery_status:uint64_t timestamp;float voltage_filtered_v;float current_a;"),
        _add(1, "battery_status"),
        _row(1, struct.pack("<Qff", 0, 25.0, 1.0)),
        _row(1, struct.pack("<Qff", 1_000_000, 20.5, 40.0)),
    )
    findings = review(read(blob), "x.ulg")["findings"]
    assert any("sagged" in f["text"] for f in findings)


def test_dropouts_and_truncation_are_reported() -> None:
    blob = _build(
        _fmt("cpuload:uint64_t timestamp;float load;float ram_usage;"),
        _add(1, "cpuload"),
        _row(1, struct.pack("<Qff", 0, 0.3, 0.4)),
        _msg("O", struct.pack("<H", 30)),
    )
    result = review(read(blob), "x.ulg")
    assert any("dropout" in f["text"] for f in result["findings"])
    assert result["summary"]["dropouts"] == 1


def test_error_messages_are_surfaced_and_carried_through() -> None:
    blob = _build(
        _msg("L", bytes([3]) + struct.pack("<Q", 100) + b"Preflight Fail: Compass"),
        _msg("L", bytes([6]) + struct.pack("<Q", 200) + b"Armed"),
    )
    result = review(read(blob), "x.ulg")
    assert any("error-level message" in f["text"] for f in result["findings"])
    assert [m["text"] for m in result["messages"]] == [
        "Preflight Fail: Compass", "Armed"]


def test_an_empty_log_reviews_without_raising() -> None:
    """A log with nothing in it must produce an empty review, not a 500."""
    result = review(read(_build()), "empty.ulg")
    assert result["plots"] == []
    assert result["messages"] == []
    assert result["summary"]["duration_s"] == 0.0


def test_the_message_list_is_capped() -> None:
    """An aircraft that spent a flight complaining can log thousands."""
    parts = [
        _msg("L", bytes([6]) + struct.pack("<Q", i) + f"msg {i}".encode())
        for i in range(400)
    ]
    result = review(read(_build(*parts)), "x.ulg")
    assert len(result["messages"]) == 300
    assert result["messages"][-1]["text"] == "msg 399", "the tail is what is kept"


# ---------------------------------------------------------------------------
# review_file: reading the same log twice must not re-read it, and must not
# hand back a stale answer for a file that changed underneath.
# ---------------------------------------------------------------------------

def _log() -> bytes:
    """A minimal but reviewable flight: one attitude topic with two samples."""
    return _build(
        _fmt("vehicle_attitude:uint64_t timestamp;float[4] q;"),
        _add(1, "vehicle_attitude"),
        _row(1, struct.pack("<Q4f", 0, 1.0, 0.0, 0.0, 0.0)),
        _row(1, struct.pack("<Q4f", 1_000_000, 1.0, 0.0, 0.0, 0.0)),
    )


def test_the_same_file_is_only_read_once(tmp_path, monkeypatch) -> None:
    from corvus import flight_review

    flight_review.clear_cache()
    path = tmp_path / "log_1_flight.ulg"
    path.write_bytes(_log())
    reads = []
    real_read = flight_review.read
    monkeypatch.setattr(flight_review, "read", lambda *a, **k: (
        reads.append(1), real_read(*a, **k))[1])

    first = flight_review.review_file(str(path))
    second = flight_review.review_file(str(path))
    assert len(reads) == 1
    assert first is second
    assert first["summary"]["name"] == "log_1_flight.ulg"
    flight_review.clear_cache()


def test_a_rewritten_log_is_re_read_not_answered_from_the_cache(tmp_path) -> None:
    """The download folder reuses names: log_3 can be replaced by a different
    flight with the same id. Handing back the previous review would send an
    operator home with the wrong evidence."""
    from corvus import flight_review

    flight_review.clear_cache()
    path = tmp_path / "log_3_flight.ulg"
    path.write_bytes(_log())
    first = flight_review.review_file(str(path))
    os.utime(path, (0, 0))
    second = flight_review.review_file(str(path))
    assert first is not second
    flight_review.clear_cache()


def test_the_cache_does_not_grow_without_bound(tmp_path) -> None:
    from corvus import flight_review

    flight_review.clear_cache()
    for index in range(5):
        path = tmp_path / f"log_{index}_flight.ulg"
        path.write_bytes(_log())
        flight_review.review_file(str(path))
    assert len(flight_review._cache) <= flight_review._CACHE_ENTRIES
    flight_review.clear_cache()


# ---------------------------------------------------------------------------
# Measuring rather than eyeballing
# ---------------------------------------------------------------------------
# The helpers below are what separates "it got bad" from "it was bad for eight
# seconds, in Position, starting at 4:12". They read the raw columns, never the
# plotted ones, and each of these tests is about one way that distinction is
# easy to get wrong.

def test_duration_is_measured_in_seconds_not_in_samples() -> None:
    """The same event logged at 5 Hz and at 500 Hz has to report the same
    length. Counting samples would call one of them a hundred times worse."""
    from corvus.flight_review import _time_above

    slow_t = [i * 0.2 for i in range(50)]
    fast_t = [i * 0.002 for i in range(5000)]
    slow = [2.0 if 10 <= i < 20 else 0.0 for i in range(50)]
    fast = [2.0 if 1000 <= i < 2000 else 0.0 for i in range(5000)]

    slow_s, slow_first, slow_worst = _time_above(slow_t, slow, 1.0)
    fast_s, fast_first, _ = _time_above(fast_t, fast, 1.0)
    assert slow_s == pytest.approx(2.0, abs=0.25)
    assert fast_s == pytest.approx(2.0, abs=0.25)
    assert slow_first == pytest.approx(2.0)
    assert fast_first == pytest.approx(2.0)
    assert slow_worst == 2.0


def test_a_setpoint_is_held_between_its_own_samples_not_interpolated() -> None:
    """The controller ran at 250 Hz against a setpoint published at 50. What it
    was actually trying to achieve is the last value it was given, so lining
    the two up index by index would invent a tracking error that never was."""
    from corvus.flight_review import _align

    held = _align([0.0, 0.5, 1.0, 1.5, 2.0], [0.0, 1.0, 2.0], [10.0, 20.0, 30.0])
    assert held == [10.0, 10.0, 20.0, 20.0, 30.0]


def test_a_signal_before_its_first_setpoint_has_no_error_to_report() -> None:
    from corvus.flight_review import _align

    held = _align([0.0, 1.0], [0.5], [7.0])
    assert held[0] != held[0], "NaN, so the difference is skipped rather than faked"
    assert held[1] == 7.0


def test_oscillation_refuses_to_answer_from_telemetry_rate_data() -> None:
    """A 20 Hz ring sampled at 10 Hz aliases into something that looks like a
    slow wobble, and a confident frequency read off that is worse than none."""
    from corvus.flight_review import _oscillation

    times = [i * 0.1 for i in range(400)]
    values = [20.0 * math.sin(2 * math.pi * 20.0 * t) for t in times]
    assert _oscillation(times, values) is None


def test_oscillation_finds_the_fast_component_under_a_slow_manoeuvre() -> None:
    """The aircraft's own motion is slow and the ringing is not. Subtracting a
    short moving average is what tells a rolling turn from a D term chattering
    on top of one."""
    from corvus.flight_review import _oscillation

    times = [i * 0.004 for i in range(1000)]          # 250 Hz, 4 s
    values = [60.0 * math.sin(2 * math.pi * 0.25 * t)     # the manoeuvre
              + 20.0 * math.sin(2 * math.pi * 18.0 * t)   # the ringing
              for t in times]
    measured = _oscillation(times, values)
    assert measured is not None
    amplitude, frequency = measured
    assert amplitude == pytest.approx(20.0 / math.sqrt(2), rel=0.25)
    assert frequency == pytest.approx(18.0, rel=0.15)
    assert amplitude < 60.0, "the slow manoeuvre is not counted as ringing"


# ---------------------------------------------------------------------------
# Findings, anchored in the flight
# ---------------------------------------------------------------------------

def _timed_log(nav_state: int = 2) -> bytes:
    """A log with a mode to place findings in and a motor stuck at full."""
    rows = [_row(2, struct.pack("<QBB", 0, 2, nav_state))]
    for index in range(80):
        rows.append(_row(1, struct.pack(
            "<Q4f", index * 100_000, 0.99, 0.40, 0.41, 0.40)))
    return _build(
        _fmt("actuator_motors:uint64_t timestamp;float[4] control;"),
        _fmt("vehicle_status:uint64_t timestamp;uint8_t arming_state;uint8_t nav_state;"),
        _add(1, "actuator_motors"),
        _add(2, "vehicle_status"),
        *rows,
        _row(2, struct.pack("<QBB", 8_000_000, 2, nav_state)),
    )


def test_a_finding_says_where_in_the_flight_to_look() -> None:
    """A number is something to argue with; a number with a second and a mode
    on it is a place on the timeline to go and look."""
    result = review(read(_timed_log()), "x.ulg")
    finding = next(f for f in result["findings"] if "full output" in f["text"])
    assert finding["t"] == 0.0
    assert finding["mode"] == "Position", "the mode flown when it happened"


def test_a_motor_held_at_its_limit_is_named_with_how_long_it_was_there() -> None:
    """At the limit the mixer has nothing left to correct with. How long that
    lasted is the difference between a gust and an aircraft that cannot fly."""
    result = review(read(_timed_log()), "x.ulg")
    finding = next(f for f in result["findings"] if "full output" in f["text"])
    assert finding["level"] == "critical"
    assert "Motor 1" in finding["text"]
    assert "8.0 s" in finding["text"]


def test_a_brief_touch_of_the_limit_is_not_a_finding() -> None:
    """Every aircraft clips its outputs in a gust. Reporting that would bury
    the log where a motor really did run out."""
    rows = [_row(1, struct.pack("<Q4f", index * 100_000,
                                0.99 if index < 3 else 0.4, 0.4, 0.4, 0.4))
            for index in range(80)]
    blob = _build(
        _fmt("actuator_motors:uint64_t timestamp;float[4] control;"),
        _add(1, "actuator_motors"), *rows)
    findings = review(read(blob), "x.ulg")["findings"]
    assert all("full output" not in f["text"] for f in findings)


def test_an_innovation_finding_says_how_long_the_sensor_was_rejected() -> None:
    """A spike over 1.0 is a bump in the road. Three seconds over it is the
    estimator having stopped believing a sensor, and only the duration tells
    those two apart."""
    rows = [_row(1, struct.pack("<Qf", index * 100_000,
                                1.5 if 10 <= index < 40 else 0.2))
            for index in range(60)]
    blob = _build(
        _fmt("estimator_status:uint64_t timestamp;float hgt_test_ratio;"),
        _add(1, "estimator_status"), *rows)
    finding = next(f for f in review(read(blob), "x.ulg")["findings"]
                   if "innovations reached" in f["text"])
    assert "stayed above 1.0 for 3.0 s" in finding["text"]
    assert finding["t"] == pytest.approx(1.0), "and when it started"


def test_a_momentary_innovation_spike_is_reported_as_normal() -> None:
    rows = [_row(1, struct.pack("<Qf", index * 100_000,
                                1.5 if index == 10 else 0.2))
            for index in range(60)]
    blob = _build(
        _fmt("estimator_status:uint64_t timestamp;float hgt_test_ratio;"),
        _add(1, "estimator_status"), *rows)
    finding = next(f for f in review(read(blob), "x.ulg")["findings"]
                   if "innovations touched" in f["text"])
    # A note, not a warning: it is only a problem when it lasts.
    assert finding["level"] == "note"
    assert "only a problem when it lasts" in _says(finding)


def test_estimator_resets_are_counted_from_the_counter_not_its_value() -> None:
    """The counters are uint8 and wrap. A step of 200 is a wrap read backwards,
    not two hundred resets."""
    values = [0, 0, 1, 1, 2, 254, 255, 0]      # two real resets, then a wrap
    rows = [_row(1, struct.pack("<QfB", index * 500_000, 1.0, value))
            for index, value in enumerate(values)]
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;uint8_t xy_reset_counter;"),
        _add(1, "vehicle_local_position"), *rows)
    finding = next(f for f in review(read(blob), "x.ulg")["findings"]
                   if "reset its state" in f["text"])
    # 0->1 and 1->2 are resets; 2->254 is +252 and rejected as a wrap read
    # backwards; 254->255 and 255->0 are two more, the second across the wrap.
    assert "4 time(s)" in finding["text"]
    assert "horizontal position 4×" in _says(finding)
    assert finding["t"] == pytest.approx(1.0)


def test_px4s_own_failure_detector_is_reported_before_anything_inferred() -> None:
    """The autopilot deciding in flight that a propeller is imbalanced is a
    stronger statement than any threshold crossed in this file, and it belongs
    at the top of the page rather than buried under derived numbers."""
    blob = _build(
        _fmt("vehicle_status:uint64_t timestamp;uint8_t arming_state;uint8_t failure_detector_status;"),
        _add(1, "vehicle_status"),
        _row(1, struct.pack("<QBB", 0, 2, 0)),
        _row(1, struct.pack("<QBB", 2_000_000, 2, 64 | 128)),
    )
    findings = review(read(blob), "x.ulg")["findings"]
    assert findings[0]["level"] == "critical"
    assert "a propeller is imbalanced" in findings[0]["text"]
    assert "a motor stopped responding" in findings[0]["text"]
    assert "own verdict on the airframe" in _says(findings[0])
    assert findings[0]["t"] == pytest.approx(2.0)


def _mag_and_thrust(coupled: bool) -> bytes:
    rows = []
    for index in range(200):
        phase = math.sin(index * 0.1)
        thrust = 0.5 + 0.3 * phase
        field = 0.45 + (0.05 * phase if coupled else 0.05 * math.cos(index * 0.37))
        rows.append(_row(1, struct.pack("<Q3f", index * 50_000, field, 0.0, 0.0)))
        rows.append(_row(2, struct.pack("<Q3f", index * 50_000, 0.0, 0.0, -thrust)))
    return _build(
        _fmt("vehicle_magnetometer:uint64_t timestamp;float[3] magnetometer_ga;"),
        _fmt("vehicle_thrust_setpoint:uint64_t timestamp;float[3] xyz;"),
        _add(1, "vehicle_magnetometer"), _add(2, "vehicle_thrust_setpoint"),
        *rows,
    )


def test_a_compass_that_moves_with_the_throttle_is_named_as_interference() -> None:
    """Two plots side by side leave the reader to eyeball a correlation, and
    this is exactly the one they get wrong: the repair is wiring, not a
    recalibration, and nothing on either plot says so."""
    findings = review(read(_mag_and_thrust(coupled=True)), "x.ulg")["findings"]
    finding = next(f for f in findings if "Compass field strength" in f["text"])
    assert "correlation" in finding["text"]
    assert "power wiring" in _says(finding)


def test_a_compass_that_merely_wanders_is_not_blamed_on_the_wiring() -> None:
    findings = review(read(_mag_and_thrust(coupled=False)), "x.ulg")["findings"]
    assert all("Compass field strength" not in f["text"] for f in findings)


def test_a_ringing_rate_loop_is_named_with_its_frequency() -> None:
    """The plot shows a thick line. The finding says 18 Hz and 14 deg/s, which
    is the difference between "looks noisy" and a D gain to turn down."""
    rows = []
    for index in range(1000):
        t = index * 0.004                                   # 250 Hz
        ring = math.radians(20.0) * math.sin(2 * math.pi * 18.0 * t)
        rows.append(_row(1, struct.pack("<Q3f", int(t * 1e6), ring, 0.0, 0.0)))
    blob = _build(
        _fmt("vehicle_angular_velocity:uint64_t timestamp;float[3] xyz;"),
        _add(1, "vehicle_angular_velocity"), *rows)
    finding = next(f for f in review(read(blob), "x.ulg")["findings"]
                   if "rate rings" in f["text"])
    assert "roll" in finding["text"]
    assert "18 Hz" in finding["text"]


def test_a_processor_out_of_headroom_is_a_finding_a_busy_one_is_not() -> None:
    quiet = [_row(1, struct.pack("<Qff", i * 200_000, 0.55, 0.4)) for i in range(40)]
    busy = [_row(1, struct.pack("<Qff", i * 200_000,
                                0.95 if i >= 10 else 0.5, 0.4)) for i in range(40)]
    fmt = (_fmt("cpuload:uint64_t timestamp;float load;float ram_usage;"),
           _add(1, "cpuload"))
    assert all("CPU held above" not in f["text"]
               for f in review(read(_build(*fmt, *quiet)), "x.ulg")["findings"])
    finding = next(f for f in review(read(_build(*fmt, *busy)), "x.ulg")["findings"]
                   if "CPU held above" in f["text"])
    assert "peak 95%" in finding["text"]


def test_a_pack_taken_below_its_cell_floor_is_a_separate_finding_from_sag() -> None:
    """Sag retires a battery; a cell floor shortens the next flight. They have
    different answers, so they are different sentences."""
    rows = [_row(1, struct.pack("<Qf", i * 200_000, 16.6 - i * 0.2)) for i in range(20)]
    blob = _build(
        _fmt("battery_status:uint64_t timestamp;float voltage_filtered_v;"),
        _add(1, "battery_status"), *rows)
    findings = review(read(blob), "x.ulg")["findings"]
    floor = next(f for f in findings if "per cell" in f["text"])
    assert floor["level"] == "critical"
    assert "across 4" in floor["text"], "cell count from the peak, not the sag"
    assert any("sagged" in f["text"] for f in findings)


def test_gps_lost_while_armed_is_reported_and_on_the_ground_is_not() -> None:
    """A receiver hunting for satellites on the bench is not a finding. The
    same thing in the air is every position plot running on dead reckoning."""
    def log(bad: range) -> bytes:
        rows = []
        for index in range(40):
            rows.append(_row(1, struct.pack("<QB", index * 500_000,
                                            1 if index in bad else 3)))
            # Armed from index 10 onwards — five seconds into the recording.
            rows.append(_row(2, struct.pack("<QBB", index * 500_000,
                                            2 if index >= 10 else 1, 2)))
        return _build(
            _fmt("vehicle_gps_position:uint64_t timestamp;uint8_t fix_type;"),
            _fmt("vehicle_status:uint64_t timestamp;uint8_t arming_state;uint8_t nav_state;"),
            _add(1, "vehicle_gps_position"), _add(2, "vehicle_status"), *rows)

    airborne = review(read(log(range(20, 30))), "x.ulg")["findings"]
    lost = next(f for f in airborne if "below a 3-D fix" in f["text"])
    assert "5 s" in lost["text"]
    assert lost["t"] == pytest.approx(10.0)

    on_the_ground = review(read(log(range(0, 8))), "x.ulg")["findings"]
    grounded = [f for f in on_the_ground if "below a 3-D fix" in f["text"]]
    assert grounded == [], "before arming it is a receiver warming up"


def test_a_flight_message_is_placed_on_the_same_clock_as_the_plots() -> None:
    """PX4 writes these with the raw log timestamp. Left that way, the one line
    that explains a flight sits next to a number with nothing to do with the
    second it happened in."""
    blob = _build(
        _fmt("cpuload:uint64_t timestamp;float load;float ram_usage;"),
        _add(1, "cpuload"),
        _row(1, struct.pack("<Qff", 1_000_000, 0.3, 0.4)),
        _msg("L", bytes([3]) + struct.pack("<Q", 5_000_000) + b"Critical battery"),
        _row(1, struct.pack("<Qff", 9_000_000, 0.3, 0.4)),
    )
    messages = review(read(blob), "x.ulg")["messages"]
    assert messages[0]["t"] == pytest.approx(4.0)
    assert messages[0]["text"] == "Critical battery"


# ---------------------------------------------------------------------------
# The plots the new findings are read against
# ---------------------------------------------------------------------------

def test_the_rate_error_plot_subtracts_rather_than_overlaying() -> None:
    """Two traces that nearly overlap hide the only thing being asked about.
    Subtracted, the gap between them is the trace."""
    rows = []
    for index in range(40):
        stamp = index * 100_000
        rows.append(_row(1, struct.pack("<Q3f", stamp, math.radians(10.0), 0.0, 0.0)))
        rows.append(_row(2, struct.pack("<Qfff", stamp, math.radians(30.0), 0.0, 0.0)))
    blob = _build(
        _fmt("vehicle_angular_velocity:uint64_t timestamp;float[3] xyz;"),
        _fmt("vehicle_rates_setpoint:uint64_t timestamp;float roll;float pitch;float yaw;"),
        _add(1, "vehicle_angular_velocity"), _add(2, "vehicle_rates_setpoint"), *rows)
    plot = _plot(review(read(blob), "x.ulg"), "rate_error")
    assert plot is not None
    roll = next(s for s in plot["series"] if s["name"] == "Roll error")
    assert roll["y"][-1] == pytest.approx(20.0, abs=0.01), "demanded minus measured"


def test_the_rate_error_plot_is_absent_without_a_setpoint_to_subtract() -> None:
    rows = [_row(1, struct.pack("<Q3f", i * 100_000, 0.1, 0.0, 0.0)) for i in range(10)]
    blob = _build(
        _fmt("vehicle_angular_velocity:uint64_t timestamp;float[3] xyz;"),
        _add(1, "vehicle_angular_velocity"), *rows)
    assert _plot(review(read(blob), "x.ulg"), "rate_error") is None


def test_esc_rpm_is_drawn_for_the_channels_the_esc_count_declares() -> None:
    """Motor commands say what was asked for; RPM says what happened, and the
    gap between the two is where a failing motor shows up first."""
    rows = [
        _row(1, struct.pack("<QB2i", index * 100_000, 2, 5000, 5400))
        for index in range(20)
    ]
    blob = _build(
        _fmt("esc_report:int32_t esc_rpm;"),
        _fmt("esc_status:uint64_t timestamp;uint8_t esc_count;esc_report[2] esc;"),
        _add(1, "esc_status"), *rows)
    plot = _plot(review(read(blob), "x.ulg"), "esc")
    assert plot is not None
    assert [s["name"] for s in plot["series"]] == ["ESC 1", "ESC 2"]
    assert plot["group"] == "Airframe"


def test_a_mode_boundary_belongs_to_the_mode_being_entered() -> None:
    """One span's end is the next one's start. A finding about the moment the
    aircraft switched to Return reads as happening in Mission if the boundary
    belongs to both — which is the one second where it matters most."""
    from corvus.flight_review import _mode_at

    modes = [{"mode": "Mission", "start": 0.0, "end": 40.0},
             {"mode": "Return", "start": 40.0, "end": 60.0}]
    assert _mode_at(modes, 39.9) == "Mission"
    assert _mode_at(modes, 40.0) == "Return"
    assert _mode_at(modes, 60.0) == "Return", "the last span owns its own end"
    assert _mode_at(modes, None) == ""


# ---------------------------------------------------------------------------
# Restraint: what must NOT appear
# ---------------------------------------------------------------------------
# A page of findings is only worth reading if its reader believes every line on
# it. One alarm about an ordinary boot, or one impossible number, costs the
# other nine their credibility — so these are about the things the review is
# now careful enough not to say.

def _motors(rows: list[tuple[float, ...]]) -> bytes:
    body = [_row(1, struct.pack("<Q4f", index * 100_000, *row))
            for index, row in enumerate(rows)]
    return _build(_fmt("actuator_motors:uint64_t timestamp;float[4] control;"),
                  _add(1, "actuator_motors"), *body)


def test_every_motor_at_its_limit_together_is_a_climb_not_a_failure() -> None:
    """A pilot holding full throttle pins all four. Calling that a critical
    loss of control authority is how a review teaches its reader to stop
    reading findings."""
    blob = _motors([(0.99, 0.99, 0.99, 0.99)] * 80)
    findings = review(read(blob), "x.ulg")["findings"]
    assert all("pinned at full output" not in f["text"] for f in findings)


def test_one_motor_at_its_limit_while_the_others_are_not_is_the_finding() -> None:
    blob = _motors([(0.99, 0.5, 0.5, 0.5)] * 80)
    finding = next(f for f in review(read(blob), "x.ulg")["findings"]
                   if "pinned at full output" in f["text"])
    assert finding["level"] == "critical"
    assert "still had headroom" in finding["text"]


def test_an_estimator_settling_on_the_ground_is_not_four_alarms() -> None:
    """Every counter ticks when the first GPS fix arrives and the filter takes
    the measurement over its dead-reckoned guess. That is the system working,
    and it used to produce four near-identical warnings about a normal boot."""
    fields = ("xy_reset_counter", "z_reset_counter", "vxy_reset_counter",
              "heading_reset_counter")
    rows = []
    for index in range(40):
        # Armed from index 20; every counter bumps once at index 5, on the
        # ground, and nothing resets afterwards.
        bumped = 1 if index >= 5 else 0
        rows.append(_row(1, struct.pack("<Qf4B", index * 500_000, 1.0, *([bumped] * 4))))
        rows.append(_row(2, struct.pack("<QBB", index * 500_000,
                                        2 if index >= 20 else 1, 2)))
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;"
             + "".join(f"uint8_t {f};" for f in fields)),
        _fmt("vehicle_status:uint64_t timestamp;uint8_t arming_state;uint8_t nav_state;"),
        _add(1, "vehicle_local_position"), _add(2, "vehicle_status"), *rows)
    findings = review(read(blob), "x.ulg")["findings"]
    assert all("reset its state" not in f["text"] for f in findings)


def test_resets_in_flight_are_one_finding_not_one_per_counter() -> None:
    """The counters move together on a single estimator event. Four paragraphs
    about one hiccup is the clearest way this page became unreadable."""
    fields = ("xy_reset_counter", "z_reset_counter", "vxy_reset_counter",
              "heading_reset_counter")
    rows = []
    for index in range(40):
        bumped = 1 if index >= 30 else 0            # in flight
        rows.append(_row(1, struct.pack("<Qf4B", index * 500_000, 1.0, *([bumped] * 4))))
        rows.append(_row(2, struct.pack("<QBB", index * 500_000,
                                        2 if index >= 20 else 1, 2)))
    blob = _build(
        _fmt("vehicle_local_position:uint64_t timestamp;float x;"
             + "".join(f"uint8_t {f};" for f in fields)),
        _fmt("vehicle_status:uint64_t timestamp;uint8_t arming_state;uint8_t nav_state;"),
        _add(1, "vehicle_local_position"), _add(2, "vehicle_status"), *rows)
    resets = [f for f in review(read(blob), "x.ulg")["findings"]
              if "reset its state" in f["text"]]
    assert len(resets) == 1, "one line, with the breakdown behind it"
    assert "4 time(s)" in resets[0]["text"]
    assert "heading 1×" in _says(resets[0])


def test_an_impossible_compass_swing_is_not_blamed_on_the_wiring() -> None:
    """One dropped sample near zero turns a min/max spread into a claim that
    the field swung by twice its own value. No magnetometer on an aircraft does
    that, and a finding printing an impossible number costs every other finding
    on the page its credibility."""
    rows = []
    for index in range(200):
        phase = math.sin(index * 0.1)
        thrust = 0.5 + 0.3 * phase
        field = 0.45 + 0.05 * phase
        if index % 17 == 0:
            field = 0.0001                 # a dropout, not a measurement
        rows.append(_row(1, struct.pack("<Q3f", index * 50_000, field, 0.0, 0.0)))
        rows.append(_row(2, struct.pack("<Q3f", index * 50_000, 0.0, 0.0, -thrust)))
    blob = _build(
        _fmt("vehicle_magnetometer:uint64_t timestamp;float[3] magnetometer_ga;"),
        _fmt("vehicle_thrust_setpoint:uint64_t timestamp;float[3] xyz;"),
        _add(1, "vehicle_magnetometer"), _add(2, "vehicle_thrust_setpoint"), *rows)
    findings = review(read(blob), "x.ulg")["findings"]
    interference = [f for f in findings if "Compass field strength" in f["text"]]
    assert interference == [], "the dropouts are not a 200% swing"


def test_context_is_a_note_and_a_problem_is_a_warning() -> None:
    """Return is how most flights end. Ranking a pilot pressing a button with
    a motor failure is what makes a reader skim the whole list."""
    rows = []
    for index in range(40):
        nav = 5 if index >= 30 else 2          # Position, then Return
        rows.append(_row(1, struct.pack("<QBB", index * 500_000, 2, nav)))
    blob = _build(
        _fmt("vehicle_status:uint64_t timestamp;uint8_t arming_state;uint8_t nav_state;"),
        _add(1, "vehicle_status"), *rows)
    findings = review(read(blob), "x.ulg")["findings"]
    returned = next(f for f in findings if "flew Return" in f["text"])
    assert returned["level"] == "note"
    # A log whose only remark is context still reads as a clean flight.
    assert findings[0]["level"] == "ok"


def test_every_finding_is_a_headline_short_enough_to_scan() -> None:
    """The reasoning belongs behind the claim, not in front of it. This is the
    property that keeps a full page of findings readable, so it is asserted
    rather than left to review."""
    result = review(read(_timed_log()), "x.ulg")
    assert len(result["findings"]) >= 1
    for finding in result["findings"]:
        assert len(finding["text"]) <= 110, finding["text"]
        assert not finding["text"].endswith("."), "a headline, not a sentence"
        assert finding.get("detail"), "and the why is still there, behind it"
