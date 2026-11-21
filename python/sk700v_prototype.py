#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "hidapi>=0.15.0",
# ]
# ///
"""Standalone SK700V USB HID protocol prototype & diagnostic tool.

Run directly with ``uv`` (no virtualenv setup required)::

    uv run python/sk700v_prototype.py --self-test
    uv run python/sk700v_prototype.py --list
    uv run python/sk700v_prototype.py --dry-run --demo --count 3
    uv run python/sk700v_prototype.py --decode 106801090d010200281900423c00001013a10516
    uv run python/sk700v_prototype.py --send --demo
    uv run python/sk700v_prototype.py --off

Pure-protocol commands (``--self-test``, ``--dry-run``, ``--decode``) also work
with standard ``python3`` without ``hidapi`` installed because ``hid`` is
imported lazily only when accessing USB hardware.

SK700V USB HID Wire Protocol (VID = 0x381C, PID = 0x0003)
=========================================================

Every HID Output report sent to the cooler is **64 bytes**, zero-padded,
beginning with HID Report ID ``0x10``:

1. **Display Control Frame** (9 bytes + 55 zero bytes)::

       Offset  Size  Field           Value
       ------  ----  --------------  ------------------------------------------
       0       1     report_id       0x10
       1..=3   3     header          0x68, 0x01, 0x09
       4       1     payload_len     0x02 (bytes 5..=6)
       5       1     command         0x03 (display power control)
       6       1     state           0x01 = ON (init), 0x00 = OFF (shutdown)
       7       1     checksum        sum(frame[1:7]) & 0xFF (0x78 ON, 0x77 OFF)
       8       1     terminator      0x16
       9..=63  55    padding         0x00

2. **Telemetry Frame** (20 bytes + 44 zero bytes)::

       Offset  Size  Field           Encoding / Notes
       ------  ----  --------------  ------------------------------------------
       0       1     report_id       0x10
       1..=3   3     header          0x68, 0x01, 0x09
       4       1     payload_len     0x0D (13 bytes, covering bytes 5..=17)
       5       1     command         0x01 (telemetry update)
       6       1     display_mode    0x02 = active/on, 0x00 = instant off
       7..=8   2     cpu_power_w     uint16 big-endian (watts, clamped 0..=999)
       9       1     power_bar_pct   uint8 (0..=100): (power_w * 100) // max_w
       10      1     temp_unit       0x00 = Celsius, 0x01 = Fahrenheit
       11..=14 4     cpu_temp        IEEE-754 float32 big-endian
       15      1     cpu_usage_pct   uint8 (0..=100)
       16..=17 2     cpu_freq_mhz    uint16 big-endian (MHz)
       18      1     checksum        sum(frame[1:18]) & 0xFF (excludes report_id)
       19      1     terminator      0x16
       20..=63 44    padding         0x00
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import math
import struct
import sys
import time
from typing import Any

# USB identifiers (Sudokoo SK700V)
VENDOR_ID = 0x381C
PRODUCT_ID_SK700V = 0x0003

# HID framing constants
REPORT_ID = 0x10
REPORT_LENGTH = 64
FRAME_HEADER = bytes((0x68, 0x01, 0x09))
FRAME_TERMINATOR = 0x16

# Command / payload identifiers
CMD_TELEMETRY = 0x01
CMD_DISPLAY_POWER = 0x03

TELEMETRY_PAYLOAD_LEN = 0x0D  # 13 bytes: offsets 5..=17
TELEMETRY_FRAME_LEN = 20
CONTROL_PAYLOAD_LEN = 0x02    # 2 bytes: offsets 5..=6
CONTROL_FRAME_LEN = 9

# Display mode & temperature unit flags
MODE_INSTANT_OFF = 0x00
MODE_ACTIVE = 0x02
UNIT_CELSIUS = 0x00
UNIT_FAHRENHEIT = 0x01

DEFAULT_POWER_BAR_MAX_W = 160


@dataclass(frozen=True)
class Telemetry:
    """CPU telemetry sample encoded into a single SK700V HID report."""

    cpu_power_w: int
    cpu_temp_c: float
    cpu_usage_pct: int
    cpu_freq_mhz: int


# Real MasterCraft USB HID captures recorded on an AMD Ryzen 7 9800X3D
# (MasterCraft scaled the power bar using floor(power_w * 100 / 156)).
CAPTURED_VECTORS: tuple[tuple[Telemetry, str], ...] = (
    (Telemetry(40, 47.0, 16, 5025), "106801090d010200281900423c00001013a10516"),
    (Telemetry(51, 48.0, 20, 5025), "106801090d010200332000424000001413a11f16"),
    (Telemetry(50, 48.0, 20, 5025), "106801090d010200322000424000001413a11e16"),
    (Telemetry(52, 49.0, 19, 5225), "106801090d01020034210042440000131469ed16"),
    (Telemetry(55, 49.0, 22, 5225), "106801090d01020037230042440000161469f516"),
    (Telemetry(67, 53.0, 80, 5005), "106801090d010200432a004254000050138d7516"),
    (Telemetry(78, 56.0, 97, 4960), "106801090d0102004e3200426000006113607816"),
    (Telemetry(77, 65.0, 89, 4935), "106801090d0102004d3100428200005913477716"),
)


def compute_checksum(frame: bytes | bytearray, end: int) -> int:
    """Return the 8-bit wrapping sum over ``frame[1:end]`` (excluding Report ID byte 0)."""
    return sum(frame[1:end]) & 0xFF


def pad_report(packet: bytes | bytearray, length: int = REPORT_LENGTH) -> bytes:
    """Zero-pad ``packet`` up to ``length`` bytes for HID Interrupt/Output writes."""
    if len(packet) > length:
        raise ValueError(f"packet length {len(packet)} exceeds report length {length}")
    return bytes(packet) + bytes(length - len(packet))


def build_control_packet(display_on: bool) -> bytes:
    """Build a 64-byte display ON (``106801090203017816...``) or OFF (``...007716...``) report."""
    frame = bytearray(CONTROL_FRAME_LEN)
    frame[0] = REPORT_ID
    frame[1:4] = FRAME_HEADER
    frame[4] = CONTROL_PAYLOAD_LEN
    frame[5] = CMD_DISPLAY_POWER
    frame[6] = 0x01 if display_on else 0x00
    frame[7] = compute_checksum(frame, 7)
    frame[8] = FRAME_TERMINATOR
    return pad_report(frame)


def encode_telemetry(
    telemetry: Telemetry,
    *,
    power_bar_max_w: int = DEFAULT_POWER_BAR_MAX_W,
    fahrenheit: bool = False,
    display_mode: int = MODE_ACTIVE,
) -> bytes:
    """Encode a :class:`Telemetry` snapshot into a 64-byte SK700V USB HID report."""
    frame = bytearray(TELEMETRY_FRAME_LEN)
    frame[0] = REPORT_ID
    frame[1:4] = FRAME_HEADER
    frame[4] = TELEMETRY_PAYLOAD_LEN
    frame[5] = CMD_TELEMETRY
    frame[6] = display_mode & 0xFF

    power_w = max(0, min(999, int(telemetry.cpu_power_w)))
    struct.pack_into(">H", frame, 7, power_w)

    max_w = max(1, int(power_bar_max_w))
    frame[9] = min(100, (power_w * 100) // max_w)

    temp_c = telemetry.cpu_temp_c if math.isfinite(telemetry.cpu_temp_c) else 0.0
    if fahrenheit:
        frame[10] = UNIT_FAHRENHEIT
        temp_val = (temp_c * 9.0 / 5.0) + 32.0
    else:
        frame[10] = UNIT_CELSIUS
        temp_val = temp_c
    struct.pack_into(">f", frame, 11, float(temp_val))

    frame[15] = max(0, min(100, int(telemetry.cpu_usage_pct)))
    freq_mhz = max(0, min(0xFFFF, int(telemetry.cpu_freq_mhz)))
    struct.pack_into(">H", frame, 16, freq_mhz)

    frame[18] = compute_checksum(frame, 18)
    frame[19] = FRAME_TERMINATOR
    return pad_report(frame)


def decode_telemetry(packet: bytes) -> dict[str, Any]:
    """Decode and validate a raw 20..64-byte SK700V telemetry packet."""
    if len(packet) < TELEMETRY_FRAME_LEN:
        raise ValueError(
            f"telemetry packet too short ({len(packet)} bytes, need at least {TELEMETRY_FRAME_LEN})"
        )
    if packet[0] != REPORT_ID or packet[1:4] != FRAME_HEADER:
        raise ValueError(f"unexpected frame header: {packet[:4].hex()}")
    if packet[4] != TELEMETRY_PAYLOAD_LEN or packet[5] != CMD_TELEMETRY:
        raise ValueError(
            f"not a telemetry frame (len=0x{packet[4]:02x}, cmd=0x{packet[5]:02x})"
        )
    if packet[19] != FRAME_TERMINATOR:
        raise ValueError(f"unexpected terminator 0x{packet[19]:02x} (expected 0x16)")

    expected_csum = compute_checksum(packet, 18)
    actual_csum = packet[18]
    (power_w,) = struct.unpack_from(">H", packet, 7)
    (temp_val,) = struct.unpack_from(">f", packet, 11)
    (freq_mhz,) = struct.unpack_from(">H", packet, 16)

    return {
        "display_mode": packet[6],
        "power_w": power_w,
        "power_bar_pct": packet[9],
        "temp_unit": "F" if packet[10] == UNIT_FAHRENHEIT else "C",
        "temp_value": temp_val,
        "usage_pct": packet[15],
        "freq_mhz": freq_mhz,
        "checksum": actual_csum,
        "expected_checksum": expected_csum,
        "checksum_ok": actual_csum == expected_csum,
    }


def run_self_test() -> None:
    """Verify byte-for-byte parity against captured SK700V HID frames and edge cases."""
    init_pkt = build_control_packet(True)
    off_pkt = build_control_packet(False)
    assert len(init_pkt) == REPORT_LENGTH
    assert len(off_pkt) == REPORT_LENGTH
    assert init_pkt == pad_report(bytes.fromhex("106801090203017816"))
    assert off_pkt == pad_report(bytes.fromhex("106801090203007716"))

    # Captured telemetry frames (recorded with power_bar_max_w = 156)
    for sample, expected_short_hex in CAPTURED_VECTORS:
        actual = encode_telemetry(sample, power_bar_max_w=156)
        expected = pad_report(bytes.fromhex(expected_short_hex))
        assert actual == expected, (
            f"mismatch for {sample}:\n  actual={actual.hex()}\n  expect={expected.hex()}"
        )
        decoded = decode_telemetry(actual)
        assert decoded["checksum_ok"]
        assert decoded["power_w"] == sample.cpu_power_w
        assert decoded["usage_pct"] == sample.cpu_usage_pct
        assert decoded["freq_mhz"] == sample.cpu_freq_mhz

    # Max display range boundary check (999W, 99C, 100%, 9995 MHz)
    max_pkt = encode_telemetry(Telemetry(999, 99.0, 100, 9995), power_bar_max_w=160)
    assert max_pkt == pad_report(bytes.fromhex("106801090d010203e7640042c6000064270b6e16"))

    # Fahrenheit flag & conversion roundtrip
    f_pkt = encode_telemetry(Telemetry(0, 25.0, 5, 3600), fahrenheit=True)
    f_decoded = decode_telemetry(f_pkt)
    assert f_decoded["checksum_ok"]
    assert f_decoded["temp_unit"] == "F"
    assert abs(f_decoded["temp_value"] - 77.0) < 1e-4

    print(
        f"Self-test OK: verified {len(CAPTURED_VECTORS)} captured telemetry frames, "
        "init/off control frames, max bounds, and Fahrenheit conversion."
    )


def _import_hid() -> Any:
    """Lazily import ``hidapi`` so offline protocol subcommands require no native DLLs."""
    try:
        import hid  # type: ignore[import-not-found]
    except ImportError as exc:
        raise RuntimeError(
            "hidapi is required for USB HID commands. Run via: uv run python/sk700v_prototype.py ..."
        ) from exc
    return hid


def list_sudokoo_devices() -> None:
    """Print every connected USB HID interface matching Sudokoo VID 0x381C."""
    hid = _import_hid()
    devices = hid.enumerate(VENDOR_ID, 0)
    if not devices:
        print(f"No Sudokoo HID interfaces detected (VID 0x{VENDOR_ID:04X}).")
        return

    print(f"Detected {len(devices)} Sudokoo HID interface(s) (VID 0x{VENDOR_ID:04X}):")
    for idx, dev in enumerate(devices):
        pid = dev.get("product_id", 0)
        iface = dev.get("interface_number", -1)
        usage_page = dev.get("usage_page", 0)
        usage = dev.get("usage", 0)
        product = dev.get("product_string") or ""
        mfg = dev.get("manufacturer_string") or ""
        status = "SK700V (verified)" if pid == PRODUCT_ID_SK700V else "unverified PID"
        print(
            f"  [{idx}] {VENDOR_ID:04X}:{pid:04X} iface={iface} "
            f"usage_page=0x{usage_page:04X} usage=0x{usage:04X} "
            f"mfg={mfg!r} product={product!r} ({status})"
        )


def _open_sk700v() -> Any:
    """Open only the allowlisted Sudokoo SK700V (0x381C:0x0003) HID interface."""
    hid = _import_hid()
    candidates = hid.enumerate(VENDOR_ID, PRODUCT_ID_SK700V)
    if not candidates:
        raise RuntimeError(
            f"Sudokoo SK700V ({VENDOR_ID:04X}:{PRODUCT_ID_SK700V:04X}) not found."
        )

    # Prefer vendor-defined HID collections (0x00FF / >= 0xFF00) if multiple interfaces exist.
    candidates.sort(
        key=lambda d: 0 if (d.get("usage_page") == 0x00FF or d.get("usage_page", 0) >= 0xFF00) else 1
    )
    target = candidates[0]

    device = hid.device()
    path = target.get("path")
    if path:
        device.open_path(path)
    else:
        device.open(VENDOR_ID, PRODUCT_ID_SK700V)
    return device


def send_display_off() -> None:
    """Connect to the SK700V, send the Display OFF control packet, and close cleanly."""
    device = _open_sk700v()
    try:
        device.write(build_control_packet(False))
        print("Sent SK700V Display OFF packet.")
    finally:
        device.close()


def _sample_sequence(args: argparse.Namespace) -> list[Telemetry]:
    if args.demo:
        return [t for t, _ in CAPTURED_VECTORS]
    return [
        Telemetry(
            cpu_power_w=args.power,
            cpu_temp_c=args.temp,
            cpu_usage_pct=args.load,
            cpu_freq_mhz=args.freq,
        )
    ]


def run_stream(args: argparse.Namespace, *, dry_run: bool) -> None:
    """Stream encoded SK700V telemetry frames either to stdout (dry-run) or USB HID."""
    samples = _sample_sequence(args)
    device = None if dry_run else _open_sk700v()

    try:
        if device is not None:
            device.write(build_control_packet(True))
            print(f"Connected to SK700V ({VENDOR_ID:04X}:{PRODUCT_ID_SK700V:04X}); sent Display ON.")

        sent = 0
        idx = 0
        while True:
            sample = samples[idx % len(samples)]
            idx += 1
            packet = encode_telemetry(
                sample,
                power_bar_max_w=args.power_bar_max,
                fahrenheit=args.fahrenheit,
            )

            if device is not None:
                device.write(packet)
            prefix = "[dry-run]" if dry_run else "[sent]"
            print(
                f"{prefix} temp={sample.cpu_temp_c:.1f}C load={sample.cpu_usage_pct}% "
                f"power={sample.cpu_power_w}W freq={sample.cpu_freq_mhz}MHz | {packet[:20].hex()}..."
            )

            sent += 1
            if args.count > 0 and sent >= args.count:
                break
            time.sleep(max(0.1, args.interval))
    except KeyboardInterrupt:
        print("\nInterrupted; shutting down...")
    finally:
        if device is not None:
            try:
                device.write(build_control_packet(False))
            finally:
                device.close()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Clean SK700V USB HID protocol prototype and diagnostic utility."
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true", help="Verify packet parity against captures.")
    mode.add_argument("--list", action="store_true", help="List detected 0x381C HID interfaces.")
    mode.add_argument("--dry-run", action="store_true", help="Print encoded packets without USB I/O.")
    mode.add_argument("--send", action="store_true", help="Send init + telemetry packets to SK700V.")
    mode.add_argument("--off", action="store_true", help="Send Display OFF packet to SK700V and exit.")
    mode.add_argument("--decode", metavar="HEX", help="Decode a captured SK700V telemetry hex frame.")

    parser.add_argument("--demo", action="store_true", help="Cycle through captured telemetry samples.")
    parser.add_argument("--temp", type=float, default=55.0, help="CPU temperature in Celsius (default: 55.0).")
    parser.add_argument("--load", type=int, default=35, help="CPU load percent 0..100 (default: 35).")
    parser.add_argument("--power", type=int, default=65, help="CPU power in watts 0..999 (default: 65).")
    parser.add_argument("--freq", type=int, default=4850, help="CPU clock in MHz (default: 4850).")
    parser.add_argument(
        "--power-bar-max",
        type=int,
        default=DEFAULT_POWER_BAR_MAX_W,
        help=f"100%% power bar reference in watts (default: {DEFAULT_POWER_BAR_MAX_W}).",
    )
    parser.add_argument("--fahrenheit", action="store_true", help="Encode temperature in Fahrenheit.")
    parser.add_argument("--interval", type=float, default=1.0, help="Send interval in seconds (default: 1.0).")
    parser.add_argument("--count", type=int, default=0, help="Packets to send before exit (0 = loop).")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    if args.self_test:
        run_self_test()
        return 0
    if args.decode:
        raw = bytes.fromhex("".join(args.decode.split()))
        decoded = decode_telemetry(raw)
        print(decoded)
        return 0 if decoded["checksum_ok"] else 1
    if args.list:
        list_sudokoo_devices()
        return 0
    if args.off:
        send_display_off()
        return 0
    if args.dry_run:
        run_stream(args, dry_run=True)
        return 0
    if args.send:
        run_stream(args, dry_run=False)
        return 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
