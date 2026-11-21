#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "hidapi>=0.15.0",
# ]
# ///
"""Standalone SK700V USB HID protocol reference & diagnostic tool.

Run directly with ``uv`` (or plain ``python3`` for offline commands)::

    uv run python/sk700v_prototype.py --self-test
    uv run python/sk700v_prototype.py --list
    uv run python/sk700v_prototype.py --dry-run --count 3
    uv run python/sk700v_prototype.py --decode 106801090d010200281900423c00001013a10516
    uv run python/sk700v_prototype.py --send
    uv run python/sk700v_prototype.py --off

SK700V USB HID Wire Protocol (VID = 0x381C, PID = 0x0003)
=========================================================

Every HID Output report is **64 bytes**, zero-padded, starting with Report ID ``0x10``:

- **Control Frame** (9 bytes + 55 zero bytes)::

      [0]=0x10  [1..=3]=68 01 09  [4]=0x02  [5]=0x03  [6]=state(01=ON|00=OFF)  [7]=csum  [8]=0x16

- **Telemetry Frame** (20 bytes + 44 zero bytes)::

      [0]=0x10  [1..=3]=68 01 09  [4]=0x0D  [5]=0x01  [6]=0x02(active)
      [7..=8]=power_w (BE u16)   [9]=power_bar_pct (u8: (power_w * 100) // max_w)
      [10]=unit (00=C, 01=F)     [11..=14]=temp (BE IEEE-754 f32)
      [15]=usage_pct (u8)        [16..=17]=freq_mhz (BE u16)
      [18]=sum(frame[1:18]) & 0xFF   [19]=0x16   [20..=63]=0x00
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import math
import struct
import sys
import time
from typing import Any

VENDOR_ID = 0x381C
PRODUCT_ID_SK700V = 0x0003
REPORT_ID = 0x10
REPORT_LENGTH = 64
FRAME_HEADER = bytes((0x68, 0x01, 0x09))
FRAME_TERMINATOR = 0x16
CMD_TELEMETRY = 0x01
CMD_DISPLAY_POWER = 0x03


@dataclass(frozen=True)
class Telemetry:
    cpu_power_w: int
    cpu_temp_c: float
    cpu_usage_pct: int
    cpu_freq_mhz: int


# Captured MasterCraft frames (Ryzen 9800X3D, power_bar_max_w = 156).
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
    """8-bit wrapping sum over ``frame[1:end]`` (excludes Report ID byte 0)."""
    return sum(frame[1:end]) & 0xFF


def pad_report(packet: bytes | bytearray, length: int = REPORT_LENGTH) -> bytes:
    if len(packet) > length:
        raise ValueError(f"packet length {len(packet)} exceeds {length}")
    return bytes(packet) + bytes(length - len(packet))


def build_control_packet(display_on: bool) -> bytes:
    """Build a 64-byte Display ON (``106801090203017816...``) or OFF (``...007716...``) frame."""
    frame = bytearray(9)
    frame[0] = REPORT_ID
    frame[1:4] = FRAME_HEADER
    frame[4] = 0x02
    frame[5] = CMD_DISPLAY_POWER
    frame[6] = 0x01 if display_on else 0x00
    frame[7] = compute_checksum(frame, 7)
    frame[8] = FRAME_TERMINATOR
    return pad_report(frame)


def encode_telemetry(
    telemetry: Telemetry, *, power_bar_max_w: int = 160, fahrenheit: bool = False
) -> bytes:
    """Encode ``telemetry`` into a 64-byte SK700V HID report."""
    frame = bytearray(20)
    frame[0] = REPORT_ID
    frame[1:4] = FRAME_HEADER
    frame[4] = 0x0D
    frame[5] = CMD_TELEMETRY
    frame[6] = 0x02  # display active

    power_w = max(0, min(999, int(telemetry.cpu_power_w)))
    struct.pack_into(">H", frame, 7, power_w)
    frame[9] = min(100, (power_w * 100) // max(1, power_bar_max_w))

    temp_c = telemetry.cpu_temp_c if math.isfinite(telemetry.cpu_temp_c) else 0.0
    frame[10] = 0x01 if fahrenheit else 0x00
    temp_val = (temp_c * 9.0 / 5.0) + 32.0 if fahrenheit else temp_c
    struct.pack_into(">f", frame, 11, float(temp_val))

    frame[15] = max(0, min(100, int(telemetry.cpu_usage_pct)))
    struct.pack_into(">H", frame, 16, max(0, min(0xFFFF, int(telemetry.cpu_freq_mhz))))
    frame[18] = compute_checksum(frame, 18)
    frame[19] = FRAME_TERMINATOR
    return pad_report(frame)


def decode_telemetry(packet: bytes) -> dict[str, Any]:
    """Decode and validate a raw 20..64-byte SK700V telemetry packet."""
    if len(packet) < 20 or packet[0] != REPORT_ID or packet[1:4] != FRAME_HEADER:
        raise ValueError(f"unexpected SK700V packet prefix: {packet[:4].hex()}")
    if packet[4] != 0x0D or packet[5] != CMD_TELEMETRY or packet[19] != FRAME_TERMINATOR:
        raise ValueError("not a valid SK700V telemetry frame")

    expected_csum = compute_checksum(packet, 18)
    (power_w,) = struct.unpack_from(">H", packet, 7)
    (temp_val,) = struct.unpack_from(">f", packet, 11)
    (freq_mhz,) = struct.unpack_from(">H", packet, 16)
    return {
        "power_w": power_w,
        "power_bar_pct": packet[9],
        "temp_unit": "F" if packet[10] == 0x01 else "C",
        "temp_value": temp_val,
        "usage_pct": packet[15],
        "freq_mhz": freq_mhz,
        "checksum_ok": packet[18] == expected_csum,
    }


def run_self_test() -> None:
    """Assert byte-for-byte parity with captured SK700V packets."""
    assert build_control_packet(True) == pad_report(bytes.fromhex("106801090203017816"))
    assert build_control_packet(False) == pad_report(bytes.fromhex("106801090203007716"))

    for sample, expected_hex in CAPTURED_VECTORS:
        actual = encode_telemetry(sample, power_bar_max_w=156)
        assert actual == pad_report(bytes.fromhex(expected_hex)), f"mismatch for {sample}"
        assert decode_telemetry(actual)["checksum_ok"]

    max_pkt = encode_telemetry(Telemetry(999, 99.0, 100, 9995), power_bar_max_w=160)
    assert max_pkt == pad_report(bytes.fromhex("106801090d010203e7640042c6000064270b6e16"))

    f_dec = decode_telemetry(encode_telemetry(Telemetry(0, 25.0, 5, 3600), fahrenheit=True))
    assert f_dec["checksum_ok"] and f_dec["temp_unit"] == "F" and abs(f_dec["temp_value"] - 77.0) < 1e-4
    print(f"Self-test OK: verified {len(CAPTURED_VECTORS)} captured frames, control frames, and bounds.")


def _open_sk700v() -> Any:
    import hid  # type: ignore[import-not-found]

    candidates = hid.enumerate(VENDOR_ID, PRODUCT_ID_SK700V)
    if not candidates:
        raise RuntimeError(f"Sudokoo SK700V ({VENDOR_ID:04X}:{PRODUCT_ID_SK700V:04X}) not found.")
    candidates.sort(key=lambda d: 0 if (d.get("usage_page") == 0x00FF or d.get("usage_page", 0) >= 0xFF00) else 1)
    dev = hid.device()
    dev.open_path(candidates[0]["path"]) if candidates[0].get("path") else dev.open(VENDOR_ID, PRODUCT_ID_SK700V)
    return dev


def list_sudokoo_devices() -> None:
    import hid  # type: ignore[import-not-found]

    devices = hid.enumerate(VENDOR_ID, 0)
    if not devices:
        print(f"No Sudokoo HID interfaces detected (VID 0x{VENDOR_ID:04X}).")
        return
    for idx, d in enumerate(devices):
        pid = d.get("product_id", 0)
        tag = "SK700V (verified)" if pid == PRODUCT_ID_SK700V else "unverified PID"
        print(f"  [{idx}] {VENDOR_ID:04X}:{pid:04X} iface={d.get('interface_number', -1)} "
              f"usage_page=0x{d.get('usage_page', 0):04X} product={d.get('product_string', '')!r} ({tag})")


def run_stream(*, dry_run: bool, count: int) -> None:
    dev = None if dry_run else _open_sk700v()
    try:
        if dev is not None:
            dev.write(build_control_packet(True))
        for idx in range(count) if count > 0 else iter(int, 1):
            sample = CAPTURED_VECTORS[idx % len(CAPTURED_VECTORS)][0]
            pkt = encode_telemetry(sample)
            if dev is not None:
                dev.write(pkt)
            tag = "[dry-run]" if dry_run else "[sent]"
            print(f"{tag} temp={sample.cpu_temp_c:.1f}C load={sample.cpu_usage_pct}% "
                  f"power={sample.cpu_power_w}W freq={sample.cpu_freq_mhz}MHz | {pkt[:20].hex()}...")
            time.sleep(1.0)
    except KeyboardInterrupt:
        pass
    finally:
        if dev is not None:
            try:
                dev.write(build_control_packet(False))
            finally:
                dev.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="SK700V USB HID protocol reference & diagnostic script.")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true", help="Verify byte-for-byte parity with captures.")
    mode.add_argument("--list", action="store_true", help="List detected 0x381C HID interfaces.")
    mode.add_argument("--decode", metavar="HEX", help="Decode a raw SK700V telemetry hex frame.")
    mode.add_argument("--dry-run", action="store_true", help="Print sample encoded packets without USB I/O.")
    mode.add_argument("--send", action="store_true", help="Stream sample frames to the SK700V.")
    mode.add_argument("--off", action="store_true", help="Send Display OFF packet and exit.")
    parser.add_argument("--count", type=int, default=0, help="Packets to emit in --dry-run/--send (0 = loop).")
    args = parser.parse_args(argv)

    if args.self_test:
        run_self_test()
    elif args.list:
        list_sudokoo_devices()
    elif args.decode:
        decoded = decode_telemetry(bytes.fromhex("".join(args.decode.split())))
        print(decoded)
        return 0 if decoded["checksum_ok"] else 1
    elif args.off:
        dev = _open_sk700v()
        try:
            dev.write(build_control_packet(False))
        finally:
            dev.close()
    else:
        run_stream(dry_run=args.dry_run, count=args.count)
    return 0


if __name__ == "__main__":
    sys.exit(main())
