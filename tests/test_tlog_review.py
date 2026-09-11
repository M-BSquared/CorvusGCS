"""Telemetry Review tests — a recorded tlog read as plots.

The tlog is the log that always exists: Corvus records one for every session,
on this laptop, from the first frame — including the flights where the ULog
never came off the aircraft. What matters here is that reading one is
*honest*: the timeline is reconstructed from the frames rather than assumed,
a recording that cannot support a plot says so instead of drawing an empty
one, and two aircraft on one link are never interleaved into one flight.

Recordings are synthesised frame by frame with pymavlink, so the tests
exercise the same bytes the bridge writes.
"""
from __future__ import annotations

import json
import math
import pathlib

import pytest

from corvus.tlog_review import (
    MAX_TLOG_BYTES, TlogError, clear_cache, review_bytes, review_file,
)

mavutil = pytest.importorskip("pymavlink.mavutil")

_MAGIC = b"#CORVUS-TLOG "
_PX4 = 12                     # MAV_AUTOPILOT_PX4
_ARMED = 128                  # MAV_MODE_FLAG_SAFETY_ARMED
_QUAD = 2                     # MAV_TYPE_QUADROTOR
# px4_custom_mode: (main << 16) | (sub << 24). Main 4 is AUTO, sub 4 MISSION.
_MODE_MISSION = (4 << 16) | (4 << 24)
_MODE_POSITION = 3 << 16


@pytest.fixture(autouse=True)
def _no_cache() -> None:
    """Each test reads its own recording; a review held from the last one
    would answer for a file it never saw."""
    clear_cache()
    yield
    clear_cache()


class _Recorder:
    """Builds a Corvus tlog the way TlogWriter does: header line, raw frames."""

    def __init__(self, system: int = 1) -> None:
        self.link = mavutil.mavlink.MAVLink(None, srcSystem=system, srcComponent=1)
        self.frames: list[bytes] = []

    def send(self, msg) -> None:
        self.frames.append(msg.pack(self.link))

    def heartbeat(self, custom_mode: int = _MODE_MISSION, armed: bool = True,
                  autopilot: int = _PX4, kind: int = _QUAD) -> None:
        self.send(mavutil.mavlink.MAVLink_heartbeat_message(
            kind, autopilot, _ARMED if armed else 0, custom_mode, 4, 3))

    def position(self, boot_ms: int, lat: float, lon: float, alt_m: float,
                 vx: float = 5.0, vy: float = 0.0, vz: float = 0.0) -> None:
        self.send(mavutil.mavlink.MAVLink_global_position_int_message(
            boot_ms, int(lat * 1e7), int(lon * 1e7), int(alt_m * 1000),
            int(alt_m * 1000), int(vx * 100), int(vy * 100), int(vz * 100), 0))

    def attitude(self, boot_ms: int, roll: float = 0.0, pitch: float = 0.0,
                 yaw: float = 0.0) -> None:
        self.send(mavutil.mavlink.MAVLink_attitude_message(
            boot_ms, roll, pitch, yaw, 0.0, 0.0, 0.0))

    def sys_status(self, volts_mv: int = 16000, load: int = 250,
                   drop: int = 0) -> None:
        self.send(mavutil.mavlink.MAVLink_sys_status_message(
            0, 0, 0, load, volts_mv, 100, 80, drop, 0, 0, 0, 0, 0))

    def vibration(self, boot_us: int, vibe: float, clipping: int = 0) -> None:
        self.send(mavutil.mavlink.MAVLink_vibration_message(
            boot_us, vibe, vibe, vibe, clipping, clipping, clipping))

    def radio(self, rssi: int, remrssi: int) -> None:
        self.send(mavutil.mavlink.MAVLink_radio_status_message(
            rssi, remrssi, 100, 0, 0, 0, 0))

    def gps(self, fix: int = 3, sats: int = 14, eph: int = 90) -> None:
        self.send(mavutil.mavlink.MAVLink_gps_raw_int_message(
            0, fix, 0, 0, 0, eph, 65535, 0, 0, sats))

    def statustext(self, severity: int, text: str) -> None:
        self.send(mavutil.mavlink.MAVLink_statustext_message(
            severity, text.encode("utf-8")[:50]))

    def system_time(self, unix_us: int, boot_ms: int) -> None:
        self.send(mavutil.mavlink.MAVLink_system_time_message(unix_us, boot_ms))

    def blob(self, meta: dict | None = None) -> bytes:
        header = _MAGIC + json.dumps(meta or {
            "product": "Corvus GCS", "version": "9.9.9-test",
            "conn": "udp:0.0.0.0:14540", "format": "mavlink-raw",
        }).encode("utf-8") + b"\n"
        return header + b"".join(self.frames)


def _a_flight(seconds: int = 30, hz: int = 5) -> _Recorder:
    """A short, healthy mission: position, attitude, battery, GPS, heartbeats."""
    rec = _Recorder()
    rec.system_time(1_700_000_000_000_000, 1000)
    for step in range(seconds * hz):
        boot = 1000 + int(step * 1000 / hz)
        rec.position(boot, 48.1 + step * 1e-5, 11.5 + step * 1e-5, 100.0 + step)
        rec.attitude(boot, roll=math.radians(5.0), pitch=math.radians(-2.0))
        if step % hz == 0:
            rec.heartbeat()
            rec.sys_status()
            rec.gps()
    return rec


# ---------------------------------------------------------------------------
# The payload
# ---------------------------------------------------------------------------

def test_a_recording_becomes_the_same_payload_shape_a_ulog_does() -> None:
    """The Analysis page draws both with one renderer, so both must arrive in
    one shape. A drifting second shape is a second renderer by the back door."""
    data = review_bytes(_a_flight().blob(), "session.tlog")
    assert set(data) >= {"summary", "groups", "modes", "armed", "findings",
                         "plots", "messages"}
    assert data["kind"] == "tlog"
    for plot in data["plots"]:
        assert set(plot) >= {"id", "title", "unit", "group", "series"}
        for series in plot["series"]:
            assert len(series["x"]) == len(series["y"])
            assert series["name"]
    # Every group named is a group something was actually filed under.
    for group in data["groups"]:
        assert any(p["group"] == group for p in data["plots"])


def test_the_timeline_is_reconstructed_from_the_frames() -> None:
    """Corvus writes raw frames with no receive timestamp of its own, so time
    has to come out of the frames. 30 s of samples must read as 30 s."""
    data = review_bytes(_a_flight(seconds=30).blob(), "s.tlog")
    assert 29.0 <= data["summary"]["duration_s"] <= 30.5
    altitude = next(p for p in data["plots"] if p["id"] == "altitude")
    x = altitude["series"][0]["x"]
    assert x[0] == 0.0, "the recording starts at zero, not at the boot clock"
    assert x == sorted(x), "and the axis only ever runs forwards"


def test_a_reboot_mid_recording_does_not_fold_the_flight_in_half() -> None:
    """The autopilot's boot clock restarts; the recording's does not. Without
    re-anchoring, the second half would be drawn on top of the first."""
    rec = _Recorder()
    for step in range(50):
        rec.position(10_000 + step * 100, 48.1, 11.5, 100.0)
    # Rebooted: boot_ms starts again from near zero.
    for step in range(50):
        rec.position(200 + step * 100, 48.1, 11.5, 200.0)
    data = review_bytes(rec.blob(), "reboot.tlog")
    x = next(p for p in data["plots"] if p["id"] == "altitude")["series"][0]["x"]
    assert x == sorted(x), "the axis stays monotonic across the reboot"
    assert data["summary"]["duration_s"] > 9.0


def test_the_flight_modes_come_off_the_heartbeats() -> None:
    """PX4 packs the mode into custom_mode. The names match the ULog review's,
    so a band is the same word and the same colour whichever log it came from."""
    rec = _Recorder()
    for step in range(40):
        boot = 1000 + step * 250
        rec.position(boot, 48.1, 11.5, 100.0)
        rec.heartbeat(_MODE_POSITION if step < 20 else _MODE_MISSION)
    data = review_bytes(rec.blob(), "modes.tlog")
    assert [m["mode"] for m in data["modes"]] == ["Position", "Mission"]
    assert data["modes"][0]["end"] == pytest.approx(data["modes"][1]["start"])


def test_a_non_px4_heartbeat_is_not_given_a_px4_mode_name() -> None:
    """Every autopilot packs custom_mode differently. A confidently
    mislabelled band is worse than a number: the strip has to be trustable."""
    rec = _Recorder()
    for step in range(40):
        rec.position(1000 + step * 250, 48.1, 11.5, 100.0)
        rec.heartbeat(custom_mode=7, autopilot=3)   # MAV_AUTOPILOT_ARDUPILOTMEGA
    data = review_bytes(rec.blob(), "ardu.tlog")
    assert [m["mode"] for m in data["modes"]] == ["Mode 7"]


def test_armed_time_is_measured_not_assumed() -> None:
    rec = _Recorder()
    for step in range(40):
        boot = 1000 + step * 250
        rec.position(boot, 48.1, 11.5, 100.0)
        rec.heartbeat(armed=5 <= step < 25)
    data = review_bytes(rec.blob(), "armed.tlog")
    assert len(data["armed"]) == 1
    assert data["summary"]["armed_s"] == pytest.approx(5.0, abs=0.6)


# ---------------------------------------------------------------------------
# Honesty about what a tlog cannot show
# ---------------------------------------------------------------------------

def test_a_recording_with_no_timeline_says_so_rather_than_looking_clean() -> None:
    """A session that only ever exchanged heartbeats has nothing timestamped in
    it. A page of no plots reading "nothing stands out" would say the flight
    was fine; the truth is that nothing could be read at all."""
    rec = _Recorder()
    for _ in range(50):
        rec.heartbeat()
        rec.sys_status()
    data = review_bytes(rec.blob(), "quiet.tlog")
    assert data["plots"] == []
    assert data["summary"]["frames"] == 100
    text = data["findings"][0]["text"]
    assert "no timeline" in text and "100 frames" in text
    assert not any(f["level"] == "ok" for f in data["findings"])


def test_a_plot_the_link_never_carried_is_absent_not_empty() -> None:
    """Which messages a link carries is a stream-rate setting. A page of empty
    axes would suggest the aircraft was missing them."""
    data = review_bytes(_a_flight().blob(), "s.tlog")
    ids = {p["id"] for p in data["plots"]}
    assert "altitude" in ids
    assert "vibration" not in ids, "no VIBRATION was streamed"
    assert "radio" not in ids, "and no RADIO_STATUS either"


def test_the_link_group_is_the_one_a_ulog_cannot_have() -> None:
    """A log written on the aircraft cannot know the ground station stopped
    hearing it. This is the reason to read a tlog even when the ULog exists."""
    rec = _a_flight()
    for _ in range(20):
        rec.radio(rssi=140, remrssi=50)
    data = review_bytes(rec.blob(), "radio.tlog")
    assert "Link" in data["groups"]
    radio = next(p for p in data["plots"] if p["id"] == "radio")
    assert {s["name"] for s in radio["series"]} >= {"Local RSSI", "Remote RSSI"}
    assert any("RSSI fell to 50" in f["text"] for f in data["findings"])


# ---------------------------------------------------------------------------
# Findings
# ---------------------------------------------------------------------------

def test_clipping_and_vibration_are_called_out() -> None:
    rec = _a_flight()
    for step in range(20):
        rec.vibration(1_000_000 + step * 100_000, vibe=42.0, clipping=step)
    data = review_bytes(rec.blob(), "vibe.tlog")
    text = " ".join(f["text"] for f in data["findings"])
    assert "clipping" in text.lower()
    assert "42 m/s²" in text
    assert any(f["level"] == "critical" for f in data["findings"])


def test_a_sagging_battery_is_reported_per_cell() -> None:
    """Volts alone mean nothing without the cell count — 12 V is healthy on a
    3S and destroyed on a 4S."""
    rec = _a_flight()
    rec.sys_status(volts_mv=12_400)     # 4S at 3.10 V/cell
    data = review_bytes(rec.blob(), "batt.tlog")
    assert any("3.10 V per cell" in f["text"] and f["level"] == "critical"
               for f in data["findings"])


def test_the_aircrafts_own_error_messages_are_surfaced() -> None:
    rec = _a_flight()
    rec.statustext(3, "Preflight Fail: Compass")
    rec.statustext(6, "Armed")
    data = review_bytes(rec.blob(), "msgs.tlog")
    levels = {m["level"] for m in data["messages"]}
    assert levels == {"error", "info"}
    assert any("Preflight Fail: Compass" in f["text"] for f in data["findings"])


def test_a_clean_recording_says_what_it_cannot_tell_you() -> None:
    """The one thing a reader must not take away is that a clean tlog means a
    clean flight — the signals that ground an aircraft are not in it."""
    data = review_bytes(_a_flight().blob(), "clean.tlog")
    assert data["findings"][0]["level"] == "ok"
    assert "ULog" in data["findings"][0]["text"]


# ---------------------------------------------------------------------------
# Two aircraft, one file
# ---------------------------------------------------------------------------

def test_a_second_aircraft_on_the_link_is_not_folded_into_the_flight() -> None:
    """A mavlink-router with two vehicles puts both in one recording.
    Interleaved, their attitude would be one impossible trace."""
    rec = _Recorder(system=1)
    other = _Recorder(system=7)
    for step in range(40):
        boot = 1000 + step * 250
        rec.position(boot, 48.1, 11.5, 100.0)
        rec.heartbeat()
        other.position(boot, 10.0, 20.0, 900.0)
        other.heartbeat()
    # Whichever system wins, only one aircraft may end up on the axes.
    merged = rec.blob() + b"".join(other.frames)
    data = review_bytes(merged, "two.tlog")
    assert data["summary"]["sys_name"] == "System 1"
    altitudes = next(p for p in data["plots"]
                     if p["id"] == "altitude")["series"][0]["y"]
    assert max(altitudes) < 500, "the other aircraft's 900 m is not in here"


# ---------------------------------------------------------------------------
# Refusals and the file on disk
# ---------------------------------------------------------------------------

def test_an_empty_or_frameless_recording_is_refused_with_a_reason() -> None:
    with pytest.raises(TlogError, match="empty"):
        review_bytes(_MAGIC + b"{}\n", "empty.tlog")
    with pytest.raises(TlogError, match="no MAVLink frames"):
        review_bytes(_MAGIC + b'{}\n' + b"\x00" * 400, "junk.tlog")


def test_a_recording_without_the_corvus_header_is_still_read() -> None:
    """A tlog from another station is the same frames. Only the recording
    metadata is lost, and that is not a reason to refuse the flight."""
    rec = _a_flight()
    data = review_bytes(b"".join(rec.frames), "foreign.tlog")
    assert data["summary"]["recorded_by"] == ""
    assert any(p["id"] == "altitude" for p in data["plots"])


def test_a_truncated_tail_still_yields_the_flight() -> None:
    """The last frame of a tlog is routinely a fragment — it is whatever was on
    the wire when the link or the process died. Strict parsing would throw the
    whole flight away over it."""
    blob = _a_flight().blob()
    data = review_bytes(blob[:-7], "cut.tlog")
    assert data["summary"]["duration_s"] > 25


def test_an_oversized_recording_is_refused_rather_than_read(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Reading a tlog means reading all of it; refusing with a reason beats an
    out-of-memory kill of the whole GCS."""
    path = tmp_path / "huge.tlog"
    path.write_bytes(_a_flight(seconds=2).blob())
    monkeypatch.setattr("corvus.tlog_review.MAX_TLOG_BYTES", 16)
    with pytest.raises(TlogError, match="too large"):
        review_file(str(path))


def test_review_file_reads_from_disk_and_names_the_recording(
    tmp_path: pathlib.Path,
) -> None:
    path = tmp_path / "20260910-143205-000000.tlog"
    path.write_bytes(_a_flight().blob())
    data = review_file(str(path))
    assert data["summary"]["name"] == "20260910-143205-000000.tlog"
    assert data["summary"]["recorded_by"] == "Corvus GCS"
    assert data["summary"]["conn"] == "udp:0.0.0.0:14540"


def test_a_recording_still_being_written_is_re_read_not_answered_from_cache(
    tmp_path: pathlib.Path,
) -> None:
    """The session in the folder may be the one recording right now. A cache
    that answered from the first read would freeze the flight mid-air."""
    path = tmp_path / "live.tlog"
    path.write_bytes(_a_flight(seconds=10).blob())
    first = review_file(str(path))["summary"]["duration_s"]
    path.write_bytes(_a_flight(seconds=30).blob())
    second = review_file(str(path))["summary"]["duration_s"]
    assert second > first


def test_a_missing_file_is_refused_with_a_reason(tmp_path: pathlib.Path) -> None:
    with pytest.raises(TlogError, match="could not read"):
        review_file(str(tmp_path / "gone.tlog"))


def test_the_size_ceiling_is_a_real_number() -> None:
    assert MAX_TLOG_BYTES >= 64 * 1024 * 1024


def test_the_frames_are_streamed_not_held_all_at_once() -> None:
    """The parsed frames must never be materialised in one list.

    MAX_TLOG_BYTES is documented as the thing that keeps a huge recording from
    OOM-killing the whole GCS, and it only does that if the frames are walked
    as a stream: at the accepted size a tlog is tens of millions of pymavlink
    objects, which is gigabytes of Python that no cap on the FILE size bounds.
    So the reader is pinned to a generator here rather than trusted to stay one.
    """
    import inspect

    from corvus import tlog_review

    assert inspect.isgeneratorfunction(tlog_review._messages)
    body = inspect.getsource(tlog_review.review_bytes)
    assert "list(_messages" not in body, "review_bytes must stream, not collect"

    # And it still reads the same flight it did when it collected.
    data = review_bytes(_a_flight().blob(), "streamed.tlog")
    assert data["summary"]["frames"] > 0
    assert data["plots"]
