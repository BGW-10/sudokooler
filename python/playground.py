import struct

import hid
import time

VENDOR_ID = 0x381C
PRODUCT_ID = 0x0003

BASE_PACKET = "106801090d0102002a1a00424400000f13a10f16"

def pad(packet, length=64):
    return packet + bytes([0] * (length - len(packet)))

def main():
    device = hid.device()
    device.open(VENDOR_ID, PRODUCT_ID)

    print("Device connected")

    pkt = bytearray.fromhex(BASE_PACKET)

    # pkt[8] = 68
    # pkt[9] = 0
    # pkt[-2] = 0x0f

    # 106801090d0102 002a 1a 00 42440000 0f 13a1 0f 16
    #                7 8  9  10 11121314 15 1617 18 19

    # 7 and 8 are power
    # 9 is power bar

    # 10 is C or F
    # 11 to 14 is float temp
    # 15 is load

    # 16 and 17 are frequency
    # 18 is checksum
    # 19 is terminate

    # "106801090d01000064320042700000280e100e16" instant turn off

    pkt[7] = 0x03
    pkt[8] = 0xE7
    pkt[9] = 100

    pkt[10] = 0  # Celsius flag
    pkt[11:15] = struct.pack('>f', float(99))
    pkt[15] = 100

    pkt[16] = 0x27
    pkt[17] = 0x0B

    sum = 0
    for i in range(1, 18):
        sum += pkt[i]
        print(i, hex(pkt[i]))

    checksum = sum & 0xFF
    pkt[18] = checksum

    print(hex(checksum))
    print(str(pkt))


    while True:
        device.write(pad(pkt))
        time.sleep(1.0)

if __name__ == "__main__":
    main()