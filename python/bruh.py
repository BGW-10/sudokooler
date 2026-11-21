import struct

import hid
import time

VENDOR_ID = 0x381C
PRODUCT_ID = 0x0003

BASE_PACKET = "106801090d010200382300424400002614690616"
BASE_PACKET = "106801090d010200382300424400002614690616"

def pad(packet, length=64):
    return packet + bytes([0] * (length - len(packet)))

def main():
    device = hid.device()
    device.open(VENDOR_ID, PRODUCT_ID)

    print("Device connected")

    pkt = bytearray.fromhex(BASE_PACKET)

    pkt[6] = 1

    #sum = 0
    #for i in range(1, 18):
        #sum += pkt[i]

    #checksum = sum & 0xFF
    #pkt[-2] = checksum

    #print(hex(checksum))
    print(str(pkt))

    


    while True:
        device.write(pad(pkt))
        time.sleep(1.0)

if __name__ == "__main__":
    main()