"""corvus_link against pymavlink: the same tables, the same bytes.

corvus_link replaces pymavlink in the running app. pymavlink stays a test
dependency so these tests can hold the Rust implementation to it: every
message definition and constant of the ``all`` dialect in both wire
versions, every message encoded and decoded both ways, signing, and the
parser's handling of a damaged stream.
"""
from __future__ import annotations

import random
import struct

import pytest

corvus_link = pytest.importorskip("corvus_link")
pv1 = pytest.importorskip("pymavlink.dialects.v10.all")
pv2 = pytest.importorskip("pymavlink.dialects.v20.all")

RV1 = corvus_link.dialect_v1
RV2 = corvus_link.dialect_v2
PAIRS = [(pv1, RV1), (pv2, RV2)]
IDS = ["v1", "v2"]

CLASS_ATTRS = (
    "id", "msgname", "fieldnames", "ordered_fieldnames", "fieldtypes",
    "fielddisplays_by_name", "fieldenums_by_name", "fieldunits_by_name",
    "orders", "lengths", "array_lengths", "crc_extra", "instance_field",
    "instance_offset",
)


class Sink:
    """A file object that keeps what was written."""

    def __init__(self) -> None:
        self.data = b""

    def write(self, buf: bytes) -> None:
        self.data += bytes(buf)


@pytest.mark.parametrize(("py", "rs"), PAIRS, ids=IDS)
def test_message_tables_match(py, rs) -> None:
    assert set(py.mavlink_map) == set(rs.mavlink_map)
    for msg_id, py_cls in py.mavlink_map.items():
        rs_cls = rs.mavlink_map[msg_id]
        for attr in CLASS_ATTRS:
            assert getattr(rs_cls, attr) == getattr(py_cls, attr), (py_cls.msgname, attr)
        assert rs_cls.format == py_cls.unpacker.format, py_cls.msgname
        name = f"MAVLink_{py_cls.msgname.lower()}_message"
        assert getattr(rs, name) is rs_cls


@pytest.mark.parametrize(("py", "rs"), PAIRS, ids=IDS)
def test_constants_match(py, rs) -> None:
    names = [n for n, v in vars(py).items() if n.isupper() and isinstance(v, int) and not isinstance(v, bool)]
    assert len(names) > 2000
    for name in names:
        assert getattr(rs, name) == getattr(py, name), name


def test_enums_match() -> None:
    assert set(pv2.enums) == set(RV2.enums)
    for name, py_enum in pv2.enums.items():
        rs_enum = RV2.enums[name]
        assert rs_enum.bitmask == py_enum.bitmask, name
        assert set(rs_enum) == set(py_enum), name
        for value, py_entry in py_enum.items():
            rs_entry = rs_enum[value]
            assert rs_entry.name == py_entry.name
            assert rs_entry.description == py_entry.description, py_entry.name
            assert rs_entry.has_location == py_entry.has_location, py_entry.name
            assert dict(rs_entry.param) == dict(py_entry.param), py_entry.name
            assert dict(rs_entry.label) == dict(py_entry.label), py_entry.name


def _value(rng: random.Random, ftype: str, length: int):
    if ftype == "char":
        alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789 \x7f\x80\xff"
        n = rng.randint(0, length) if length else 1
        return bytes(rng.choice(alphabet) for _ in range(n))
    if ftype in ("float", "double"):
        v = rng.choice([0.0, 1.5, -2.25, rng.uniform(-1e6, 1e6)])
        return struct.unpack("<f", struct.pack("<f", v))[0] if ftype == "float" else v
    bits = int(ftype.replace("uint", "").replace("int", "").replace("_t", ""))
    if ftype.startswith("u"):
        return rng.choice([0, 1, (1 << bits) - 1, rng.randrange(0, 1 << bits)])
    lo = -(1 << (bits - 1))
    return rng.choice([0, -1, lo, -lo - 1, rng.randrange(lo, -lo)])


def _args(rng: random.Random, cls) -> list:
    args = []
    for ftype, alen in zip(cls.fieldtypes, _decl_array_lengths(cls)):
        if ftype == "char":
            args.append(_value(rng, "char", alen))
        elif alen:
            args.append([_value(rng, ftype, alen) for _ in range(alen)])
        else:
            args.append(_value(rng, ftype, 0))
    return args


def _decl_array_lengths(cls) -> list[int]:
    """Array length per field in declaration order, from the struct format."""
    fmt = cls.unpacker.format if hasattr(cls, "unpacker") else cls.format
    import re
    items = re.findall(r"(\d*)([a-zA-Z])", fmt[1:])
    by_wire = [int(n) if n else 0 for n, _ in items]
    lengths = []
    for name in cls.fieldnames:
        lengths.append(by_wire[cls.ordered_fieldnames.index(name)])
    return lengths


def _pair(py, rs, src=(42, 7)):
    pf, rf = Sink(), Sink()
    pm = py.MAVLink(pf, srcSystem=src[0], srcComponent=src[1])
    rm = rs.MAVLink(rf, srcSystem=src[0], srcComponent=src[1])
    return pm, pf, rm, rf


@pytest.mark.parametrize(("py", "rs"), PAIRS, ids=IDS)
def test_every_message_encodes_identically(py, rs) -> None:
    rng = random.Random(1234)
    pm, pf, rm, rf = _pair(py, rs)
    for _msg_id, py_cls in sorted(py.mavlink_map.items()):
        for _ in range(3):
            args = _args(rng, py_cls)
            name = py_cls.msgname.lower()
            getattr(pm, name + "_send")(*args)
            getattr(rm, name + "_send")(*args)
            assert rf.data == pf.data, py_cls.msgname
            pf.data = rf.data = b""
    assert rm.seq == pm.seq
    assert rm.total_packets_sent == pm.total_packets_sent


@pytest.mark.parametrize(("py", "rs"), PAIRS, ids=IDS)
def test_every_message_decodes_identically(py, rs) -> None:
    rng = random.Random(99)
    pm, pf, _rm, _rf = _pair(py, rs)
    parser_py = py.MAVLink(None)
    parser_rs = rs.MAVLink(None)
    for _msg_id, py_cls in sorted(py.mavlink_map.items()):
        getattr(pm, py_cls.msgname.lower() + "_send")(*_args(rng, py_cls))
        frame, pf.data = pf.data, b""
        a = parser_py.parse_char(frame)
        b = parser_rs.parse_char(frame)
        assert a is not None and b is not None, py_cls.msgname
        assert b.get_type() == a.get_type()
        assert b.to_dict() == a.to_dict(), py_cls.msgname
        for field in py_cls.fieldnames:
            assert getattr(b, field) == getattr(a, field), (py_cls.msgname, field)
        assert bytes(b.get_msgbuf()) == bytes(a.get_msgbuf())
        assert bytes(b.get_payload()) == bytes(a.get_payload())
        assert b.get_crc() == a.get_crc()
        for getter in ("get_msgId", "get_srcSystem", "get_srcComponent", "get_seq", "get_signed", "get_link_id"):
            assert getattr(b, getter)() == getattr(a, getter)(), getter
        assert str(b) == str(a)


def test_v2_frames_through_a_v1_parser_match() -> None:
    """A MAVLink 1 parser meeting MAVLink 2 frames: unknown ids, no extensions."""
    rng = random.Random(5)
    pm, pf, _rm, _rf = _pair(pv2, RV2)
    a_parser, b_parser = pv1.MAVLink(None), RV1.MAVLink(None)
    for py_cls in list(pv2.mavlink_map.values())[::7]:
        getattr(pm, py_cls.msgname.lower() + "_send")(*_args(rng, py_cls))
    stream, pf.data = pf.data, b""
    a = a_parser.parse_buffer(stream)
    b = b_parser.parse_buffer(stream)
    assert [m.get_type() for m in b] == [m.get_type() for m in a]
    for x, y in zip(a, b):
        if x.get_type().startswith(("UNKNOWN", "BAD")):
            assert bytes(y.get_msgbuf()) == bytes(x.get_msgbuf())
        else:
            assert y.to_dict() == x.to_dict()


@pytest.mark.parametrize("robust", [True, False])
def test_damaged_stream_parses_identically(robust: bool) -> None:
    rng = random.Random(7)
    pm, pf, _rm, _rf = _pair(pv2, RV2)
    for py_cls in list(pv2.mavlink_map.values())[::5]:
        getattr(pm, py_cls.msgname.lower() + "_send")(*_args(rng, py_cls))
        if rng.random() < 0.3:
            pf.data += bytes(rng.randrange(256) for _ in range(rng.randint(1, 9)))
    stream = bytearray(pf.data)
    for _ in range(40):
        stream[rng.randrange(len(stream))] = rng.randrange(256)
    a_parser, b_parser = pv2.MAVLink(None), RV2.MAVLink(None)
    a_parser.robust_parsing = b_parser.robust_parsing = robust
    out_a, out_b = [], []
    for parser, out in ((a_parser, out_a), (b_parser, out_b)):
        chunks = random.Random(11)
        pos = 0
        while pos < len(stream):
            step = chunks.randint(1, 40)
            chunk = bytes(stream[pos:pos + step])
            pos += step
            while True:
                try:
                    m = parser.parse_char(chunk)
                except Exception as exc:  # noqa: BLE001 - compared below
                    out.append(("error", type(exc).__name__, str(exc)))
                    m = None
                chunk = b""
                if m is None:
                    break
                out.append((m.get_type(), bytes(m.get_msgbuf()), getattr(m, "reason", None)))
    assert out_b == out_a
    assert b_parser.total_receive_errors == a_parser.total_receive_errors
    assert b_parser.total_packets_received == a_parser.total_packets_received


def test_signed_frames_match_and_verify() -> None:
    key = bytes(range(32))
    pm, pf, rm, rf = _pair(pv2, RV2)
    for m in (pm, rm):
        m.signing.secret_key = key
        m.signing.sign_outgoing = True
        m.signing.link_id = 3
        m.signing.timestamp = 123456789
    pm.command_long_send(1, 1, 400, 0, 1, 0, 0, 0, 0, 0, 0)
    rm.command_long_send(1, 1, 400, 0, 1, 0, 0, 0, 0, 0, 0)
    assert rf.data == pf.data
    assert rm.signing.timestamp == pm.signing.timestamp == 123456790

    verifier = RV2.MAVLink(None)
    verifier.signing.secret_key = key
    msg = verifier.parse_char(pf.data)
    assert msg.get_signed() and msg.get_link_id() == 3
    assert verifier.signing.goodsig_count == 1

    tampered = bytearray(pf.data)
    tampered[-1] ^= 1
    reject = RV2.MAVLink(None)
    reject.signing.secret_key = key
    reject.robust_parsing = True
    bad = reject.parse_char(bytes(tampered))
    assert bad.get_type() == "BAD_DATA" and bad.reason == "Invalid signature"

    unsigned_pf = Sink()
    plain = pv2.MAVLink(unsigned_pf, 1, 1)
    plain.radio_status_send(1, 2, 3, 4, 5, 6, 7)
    plain.heartbeat_send(2, 12, 0, 0, 0)
    gate = RV2.MAVLink(None)
    gate.signing.secret_key = key
    gate.signing.allow_unsigned_callback = lambda mav, msg_id: msg_id == 109
    gate.robust_parsing = True
    got = gate.parse_buffer(unsigned_pf.data)
    assert [m.get_type() for m in got] == ["RADIO_STATUS", "BAD_DATA"]
    assert gate.signing.unsigned_count == 1 and gate.signing.reject_count == 1


def test_force_mavlink1_and_header_fields() -> None:
    pm, pf, rm, rf = _pair(pv2, RV2, src=(254, 190))
    for m in (pm, rm):
        m.seq = 250
        for _ in range(8):
            m.param_request_read_send(1, 1, b"MPC_XY_VEL_MAX", -1, force_mavlink1=True)
    assert rf.data == pf.data and rm.seq == pm.seq


def test_wrong_argument_types_fail_like_pymavlink() -> None:
    import struct as st
    _pm, _pf, rm, _rf = _pair(pv2, RV2)
    with pytest.raises(st.error):
        rm.heartbeat_send(300, 0, 0, 0, 0)
    with pytest.raises(st.error):
        rm.heartbeat_send(1.5, 0, 0, 0, 0)
    with pytest.raises(TypeError):
        rm.param_set_send(1, 1, "TEXT_NOT_BYTES", 1.0, 9)
    with pytest.raises(TypeError):
        rm.heartbeat_send(1, 2)
    assert not hasattr(RV1.MAVLink(None), "open_drone_id_basic_id_send")
    assert hasattr(RV2.MAVLink(None), "open_drone_id_basic_id_send")


def test_x25crc_matches() -> None:
    from pymavlink.generator.mavcrc import x25crc as py_crc
    data = bytes(range(256)) * 3
    a, b = py_crc(data), corvus_link.x25crc(data)
    a.accumulate(b"xyz")
    b.accumulate(b"xyz")
    a.accumulate_str("abc")
    b.accumulate_str("abc")
    assert a.crc == b.crc


def test_mode_tables_and_decoding_match() -> None:
    from pymavlink import mavutil as pmu
    from corvus_link import mavutil as rmu
    assert rmu.px4_map == pmu.px4_map
    for t in range(0, 45):
        assert rmu.mode_mapping_bynumber(t) == pmu.mode_mapping_bynumber(t), t
        assert rmu.mode_mapping_byname(t) == pmu.mode_mapping_byname(t), t
    rng = random.Random(3)
    for _ in range(500):
        autopilot = rng.choice([3, 12, 0, 8])
        mav_type = rng.choice([1, 2, 10, 12, 13, 19, 20, 7, 5])
        base = rng.choice([0, 1, 81, 129, 217])
        custom = rng.choice([0, 3, 4, 6, 9, 24, 0x040000, 0x04040000, 0x03050000, rng.randrange(1 << 32)])
        a = pv2.MAVLink_heartbeat_message(mav_type, autopilot, base, custom, 4, 3)
        b = RV2.MAVLink_heartbeat_message(mav_type, autopilot, base, custom, 4, 3)
        assert rmu.mode_string_v10(b) == pmu.mode_string_v10(a)
