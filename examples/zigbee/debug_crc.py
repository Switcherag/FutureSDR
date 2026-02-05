#!/usr/bin/env python3
"""
Debug CRC for ZigBee packets.
IEEE 802.15.4 uses CRC-16-CCITT with polynomial 0x1021 (bit-reversed: 0x8408).
"""

import sys
from pathlib import Path


def calc_crc_rust_style(data: bytes) -> int:
    """
    CRC calculation matching the Rust implementation in mac.rs.
    Processes LSB first, uses polynomial with bits at 15, 10, 3.
    """
    crc = 0
    for b in data:
        for k in range(8):
            bit = ((b >> k) & 1) ^ (crc & 1)
            crc >>= 1
            if bit:
                crc ^= (1 << 15) | (1 << 10) | (1 << 3)
    return crc


def calc_crc_standard(data: bytes) -> int:
    """
    Standard IEEE 802.15.4 CRC-16 calculation.
    Polynomial: 0x8408 (bit-reversed 0x1021)
    Initial value: 0x0000
    """
    crc = 0x0000
    for byte in data:
        crc ^= byte
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0x8408
            else:
                crc >>= 1
    return crc


def calc_crc_standard_init_ffff(data: bytes) -> int:
    """
    CRC-16-CCITT with init=0xFFFF (sometimes used).
    """
    crc = 0xFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0x8408
            else:
                crc >>= 1
    return crc


def calc_crc_msb_first(data: bytes) -> int:
    """
    CRC processing MSB first (alternative interpretation).
    """
    crc = 0x0000
    for byte in data:
        for k in range(7, -1, -1):
            bit = ((byte >> k) & 1) ^ ((crc >> 15) & 1)
            crc = (crc << 1) & 0xFFFF
            if bit:
                crc ^= 0x1021
    return crc


def reverse_bits_16(val: int) -> int:
    """Reverse bits in a 16-bit value."""
    result = 0
    for i in range(16):
        if val & (1 << i):
            result |= 1 << (15 - i)
    return result


def reverse_bits_8(val: int) -> int:
    """Reverse bits in an 8-bit value."""
    result = 0
    for i in range(8):
        if val & (1 << i):
            result |= 1 << (7 - i)
    return result


def check_crc_residue(data: bytes) -> bool:
    """
    Check CRC by computing over data+CRC - should give 0.
    This is how the Rust code checks: calc_crc(data) == 0
    """
    return calc_crc_rust_style(data) == 0


def parse_log_file(filename: str):
    """Parse log file and extract packets."""
    packets = []
    with open(filename, 'r') as f:
        for line in f:
            parts = line.strip().split(',')
            if len(parts) >= 3:
                try:
                    timestamp = float(parts[0])
                    crc_ok = parts[1].strip() == "CRC_OK"
                    hex_data = parts[2].strip()
                    data = bytes.fromhex(hex_data)
                    packets.append({
                        'timestamp': timestamp,
                        'crc_ok': crc_ok,
                        'data': data,
                        'hex': hex_data
                    })
                except (ValueError, IndexError):
                    continue
    return packets


def analyze_packet_crc(data: bytes):
    """Analyze CRC for a single packet."""
    if len(data) < 3:
        return None
    
    # Split into payload and CRC
    payload = data[:-2]
    crc_bytes = data[-2:]
    received_crc = crc_bytes[0] | (crc_bytes[1] << 8)  # Little-endian
    received_crc_be = (crc_bytes[0] << 8) | crc_bytes[1]  # Big-endian
    
    # Calculate CRC using different methods
    results = {
        'payload_len': len(payload),
        'received_crc_le': received_crc,
        'received_crc_be': received_crc_be,
        'rust_style_residue': calc_crc_rust_style(data),
        'rust_style_payload': calc_crc_rust_style(payload),
        'standard_payload': calc_crc_standard(payload),
        'standard_ffff_payload': calc_crc_standard_init_ffff(payload),
        'msb_first_payload': calc_crc_msb_first(payload),
    }
    
    # Check various CRC interpretations
    results['match_rust_le'] = results['rust_style_payload'] == received_crc
    results['match_rust_be'] = results['rust_style_payload'] == received_crc_be
    results['match_standard_le'] = results['standard_payload'] == received_crc
    results['match_standard_be'] = results['standard_payload'] == received_crc_be
    results['residue_zero'] = results['rust_style_residue'] == 0
    
    return results


def test_known_vectors():
    """Test CRC implementation with known test vectors."""
    print("=" * 60)
    print("CRC ALGORITHM VERIFICATION")
    print("=" * 60)
    
    # IEEE 802.15.4 test: "123456789" should give specific CRC
    test_data = b"123456789"
    
    print(f"\nTest data: {test_data.hex()} ('{test_data.decode()}')")
    print(f"  Rust-style CRC:    0x{calc_crc_rust_style(test_data):04x}")
    print(f"  Standard CRC:      0x{calc_crc_standard(test_data):04x}")
    print(f"  Standard init=FF:  0x{calc_crc_standard_init_ffff(test_data):04x}")
    print(f"  MSB-first CRC:     0x{calc_crc_msb_first(test_data):04x}")
    
    # Check polynomial interpretation
    poly_bits = (1 << 15) | (1 << 10) | (1 << 3)
    print(f"\nPolynomial from Rust code (bits 15,10,3): 0x{poly_bits:04x}")
    print(f"Standard 802.15.4 polynomial (0x8408):    0x8408")
    print(f"Match: {poly_bits == 0x8408}")
    
    # Test with a simple ACK frame
    # ACK frame: FC=0x0002, Seq=0xE1
    ack_payload = bytes([0x02, 0x00, 0xE1])
    print(f"\nACK frame payload: {ack_payload.hex()}")
    print(f"  Rust-style CRC:    0x{calc_crc_rust_style(ack_payload):04x}")
    print(f"  Standard CRC:      0x{calc_crc_standard(ack_payload):04x}")


def analyze_ack_packets(packets):
    """Analyze 3-byte ACK packets specifically."""
    print("\n" + "=" * 60)
    print("ACK PACKET ANALYSIS (3-byte packets)")
    print("=" * 60)
    
    ack_packets = [p for p in packets if len(p['data']) == 3]
    
    if not ack_packets:
        print("No 3-byte ACK packets found.")
        return
    
    print(f"\nFound {len(ack_packets)} ACK packets")
    print("\nNote: Standard ACK should be 5 bytes (FC:2 + Seq:1 + CRC:2)")
    print("      3-byte packets suggest CRC bytes are MISSING!")
    
    print("\nFirst 10 ACK packets:")
    for i, pkt in enumerate(ack_packets[:10]):
        data = pkt['data']
        fc = data[0] | (data[1] << 8)
        seq = data[2]
        print(f"  {i+1}. FC=0x{fc:04x}, Seq={seq:3d}, Data: {data.hex()}")
        
        # If we HAD a proper 5-byte ACK, what would CRC be?
        expected_crc = calc_crc_standard(data)
        expected_crc_rust = calc_crc_rust_style(data)
        print(f"      Expected CRC (standard): 0x{expected_crc:04x}")
        print(f"      Expected CRC (rust):     0x{expected_crc_rust:04x}")


def analyze_data_packets(packets):
    """Analyze data packets with full CRC."""
    print("\n" + "=" * 60)
    print("DATA PACKET CRC ANALYSIS")
    print("=" * 60)
    
    # Look at packets that should have CRC (not 3-byte ACKs)
    data_packets = [p for p in packets if len(p['data']) >= 5]
    
    if not data_packets:
        print("No data packets found.")
        return
    
    print(f"\nAnalyzing {min(20, len(data_packets))} data packets:\n")
    
    match_counts = {
        'residue_zero': 0,
        'match_rust_le': 0,
        'match_rust_be': 0,
        'match_standard_le': 0,
        'match_standard_be': 0,
    }
    
    for i, pkt in enumerate(data_packets[:20]):
        data = pkt['data']
        result = analyze_packet_crc(data)
        
        if result:
            print(f"Packet {i+1}: {len(data)} bytes")
            print(f"  Data: {data[:10].hex()}... (first 10 bytes)")
            print(f"  Last 4 bytes: ...{data[-4:].hex()}")
            print(f"  Received CRC (LE): 0x{result['received_crc_le']:04x}")
            print(f"  Calculated (rust): 0x{result['rust_style_payload']:04x}")
            print(f"  Calculated (std):  0x{result['standard_payload']:04x}")
            print(f"  Residue check:     {result['residue_zero']}")
            print()
            
            for key in match_counts:
                if result.get(key):
                    match_counts[key] += 1
    
    print("\nMatch statistics:")
    for key, count in match_counts.items():
        print(f"  {key}: {count}/{min(20, len(data_packets))}")


def investigate_bit_errors(packets):
    """Look for patterns in CRC failures."""
    print("\n" + "=" * 60)
    print("BIT ERROR INVESTIGATION")
    print("=" * 60)
    
    data_packets = [p for p in packets if len(p['data']) >= 5][:50]
    
    if not data_packets:
        return
    
    xor_values = []
    for pkt in data_packets:
        data = pkt['data']
        payload = data[:-2]
        crc_bytes = data[-2:]
        received_crc = crc_bytes[0] | (crc_bytes[1] << 8)
        calculated_crc = calc_crc_rust_style(payload)
        xor_val = received_crc ^ calculated_crc
        xor_values.append(xor_val)
    
    print("\nXOR between received and calculated CRC:")
    print("(If consistent, might indicate systematic error)")
    
    # Check for common XOR patterns
    from collections import Counter
    xor_counts = Counter(xor_values)
    
    print("\nMost common XOR values:")
    for xor_val, count in xor_counts.most_common(10):
        print(f"  0x{xor_val:04x}: {count} times ({bin(xor_val)})")


def try_different_lengths(packets):
    """Try CRC with different payload lengths."""
    print("\n" + "=" * 60)
    print("LENGTH VARIATION ANALYSIS")
    print("=" * 60)
    print("Testing if decoder might be including/excluding bytes incorrectly")
    
    data_packets = [p for p in packets if len(p['data']) >= 7][:10]
    
    for i, pkt in enumerate(data_packets):
        data = pkt['data']
        print(f"\nPacket {i+1}: {len(data)} bytes - {data.hex()}")
        
        # Try different interpretations
        for offset in range(-2, 3):
            if 2 <= len(data) + offset <= len(data):
                if offset < 0:
                    test_data = data[:offset]
                elif offset > 0:
                    # Can't extend data
                    continue
                else:
                    test_data = data
                
                if len(test_data) >= 3:
                    payload = test_data[:-2]
                    crc_bytes = test_data[-2:]
                    received = crc_bytes[0] | (crc_bytes[1] << 8)
                    calculated = calc_crc_rust_style(payload)
                    residue = calc_crc_rust_style(test_data)
                    
                    match = "✓" if residue == 0 else "✗"
                    print(f"  Offset {offset:+d}: len={len(test_data)}, "
                          f"recv=0x{received:04x}, calc=0x{calculated:04x}, "
                          f"residue=0x{residue:04x} {match}")


def check_nibble_swap(packets):
    """Check if nibbles might be swapped in decoding."""
    print("\n" + "=" * 60)
    print("NIBBLE SWAP CHECK")
    print("=" * 60)
    print("Testing if chip-to-byte decoding has nibble order issue")
    
    data_packets = [p for p in packets if len(p['data']) >= 5][:10]
    
    def swap_nibbles(data):
        """Swap high and low nibbles in each byte."""
        return bytes(((b >> 4) | ((b & 0x0F) << 4)) for b in data)
    
    for i, pkt in enumerate(data_packets):
        data = pkt['data']
        swapped = swap_nibbles(data)
        
        # Try CRC on swapped data
        if len(swapped) >= 3:
            payload = swapped[:-2]
            crc_bytes = swapped[-2:]
            received = crc_bytes[0] | (crc_bytes[1] << 8)
            calculated = calc_crc_rust_style(payload)
            residue = calc_crc_rust_style(swapped)
            
            if residue == 0:
                print(f"Packet {i+1}: NIBBLE SWAP FIXES CRC!")
                print(f"  Original: {data.hex()}")
                print(f"  Swapped:  {swapped.hex()}")


def main():
    # Test CRC algorithms first
    test_known_vectors()
    
    # Look for log file
    log_files = ['log2.txt', 'log.txt']
    log_file = None
    for f in log_files:
        if Path(f).exists():
            log_file = f
            break
    
    if log_file is None:
        print("\nNo log file found (log.txt or log2.txt)")
        print("Run the receiver first to generate data.")
        return
    
    print(f"\n\nLoading packets from {log_file}...")
    packets = parse_log_file(log_file)
    print(f"Loaded {len(packets)} packets")
    
    # Analyze different aspects
    analyze_ack_packets(packets)
    analyze_data_packets(packets)
    investigate_bit_errors(packets)
    try_different_lengths(packets)
    check_nibble_swap(packets)
    
    print("\n" + "=" * 60)
    print("CONCLUSIONS")
    print("=" * 60)
    print("""
Key observations:
1. 3-byte ACK packets are suspicious - standard ACK is 5 bytes
   (Frame Control: 2 bytes + Sequence: 1 byte + CRC: 2 bytes)
   
2. The Rust CRC polynomial (bits 15,10,3 = 0x8408) matches IEEE 802.15.4

3. Possible issues to investigate:
   - PHY length field interpretation (does it include CRC or not?)
   - Decoder might be truncating packets
   - Chip-to-symbol mapping might have bit ordering issues
   - Force-decode fallback might produce garbage data
    """)


if __name__ == "__main__":
    main()
