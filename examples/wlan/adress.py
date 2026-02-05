#!/usr/bin/env python3
"""
MAC Address Randomness Analysis Tool

This script analyzes MAC addresses from received WLAN frames to determine
if they are random (corrupted) or consistent (valid transmission).

Usage:
  1. Run loopback_radio to start transmitting and receiving frames
  2. Run this script: python3 adress.py
  
The script listens on UDP port 55555 for received frames from the decoder.
"""

import socket
import struct
import collections
import math
from datetime import datetime

# UDP port where decoder sends received frames
UDP_PORT = 55555
UDP_IP = "127.0.0.1"

# Expected MAC addresses from loopback_radio.rs:
# Mac::new([0x42; 6], [0x23; 6], [0xff; 6])
EXPECTED_SRC_MAC = bytes([0x42] * 6)  # 42:42:42:42:42:42
EXPECTED_DST_MAC = bytes([0x23] * 6)  # 23:23:23:23:23:23
EXPECTED_BSS_MAC = bytes([0xff] * 6)  # ff:ff:ff:ff:ff:ff


def mac_to_str(mac_bytes):
    """Convert 6-byte MAC address to string format."""
    return ':'.join(f'{b:02x}' for b in mac_bytes)


def calculate_entropy(byte_counts, total_bytes):
    """Calculate Shannon entropy of byte distribution (0-8 bits)."""
    if total_bytes == 0:
        return 0
    entropy = 0
    for count in byte_counts.values():
        if count > 0:
            p = count / total_bytes
            entropy -= p * math.log2(p)
    return entropy


def analyze_mac_randomness(mac_list, name):
    """Analyze if a list of MAC addresses appears random."""
    if not mac_list:
        print(f"  {name}: No data")
        return
    
    # Count unique MACs
    unique_macs = set(mac_list)
    unique_ratio = len(unique_macs) / len(mac_list)
    
    # Count byte-level distribution
    all_bytes = b''.join(mac_list)
    byte_counts = collections.Counter(all_bytes)
    entropy = calculate_entropy(byte_counts, len(all_bytes))
    
    # Check how many match expected
    if name == "Source MAC":
        expected = EXPECTED_SRC_MAC
    elif name == "Dest MAC":
        expected = EXPECTED_DST_MAC
    else:
        expected = EXPECTED_BSS_MAC
    
    matches = sum(1 for mac in mac_list if mac == expected)
    match_ratio = matches / len(mac_list)
    
    # Most common MACs
    mac_counter = collections.Counter(mac_list)
    most_common = mac_counter.most_common(5)
    
    print(f"\n  {name}:")
    print(f"    Total frames: {len(mac_list)}")
    print(f"    Unique MACs: {len(unique_macs)} ({unique_ratio*100:.1f}%)")
    print(f"    Expected ({mac_to_str(expected)}): {matches} ({match_ratio*100:.1f}%)")
    print(f"    Byte entropy: {entropy:.2f} bits (max 8.0 = fully random)")
    print(f"    Most common MACs:")
    for mac, count in most_common:
        marker = " <- EXPECTED" if mac == expected else ""
        print(f"      {mac_to_str(mac)}: {count} ({count/len(mac_list)*100:.1f}%){marker}")
    
    # Randomness assessment
    if match_ratio > 0.9:
        assessment = "GOOD - Most frames have correct MAC"
    elif match_ratio > 0.5:
        assessment = "DEGRADED - Many frames corrupted"
    elif unique_ratio > 0.5:
        assessment = "RANDOM - Appears to be corrupted/noise"
    else:
        assessment = "STUCK - Same incorrect value repeated"
    
    print(f"    Assessment: {assessment}")
    return match_ratio, entropy


def analyze_frame_control(fc_list):
    """Analyze the 802.11 Frame Control field."""
    if not fc_list:
        return
    
    # Expected: Data frame (type=2, subtype=0) = 0x0008 in little endian
    fc_counter = collections.Counter(fc_list)
    most_common = fc_counter.most_common(5)
    
    print(f"\n  Frame Control Field (first 2 bytes):")
    print(f"    Expected: 0x0008 (Data frame)")
    for fc, count in most_common:
        fc_hex = fc.hex()
        fc_val = struct.unpack('<H', fc)[0]
        frame_type = (fc_val >> 2) & 0x3
        frame_subtype = (fc_val >> 4) & 0xf
        type_names = {0: "Management", 1: "Control", 2: "Data", 3: "Extension"}
        print(f"    0x{fc_hex} (type={type_names.get(frame_type, '?')}, subtype={frame_subtype}): {count}")


def main():
    print("=" * 60)
    print("MAC Address Randomness Analyzer")
    print("=" * 60)
    print(f"Listening on UDP {UDP_IP}:{UDP_PORT}")
    print("Press Ctrl+C to stop and see analysis")
    print()
    
    # Create UDP socket
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((UDP_IP, UDP_PORT))
    sock.settimeout(1.0)  # 1 second timeout for keyboard interrupt
    
    # Storage for analysis
    src_macs = []
    dst_macs = []
    bss_macs = []
    frame_controls = []
    frame_sizes = []
    
    frame_count = 0
    start_time = datetime.now()
    
    try:
        while True:
            try:
                data, addr = sock.recvfrom(4096)
                frame_count += 1
                frame_sizes.append(len(data))
                
                # 802.11 MAC header structure:
                # Bytes 0-1: Frame Control
                # Bytes 2-3: Duration/ID
                # Bytes 4-9: Address 1 (typically destination/receiver)
                # Bytes 10-15: Address 2 (typically source/transmitter)  
                # Bytes 16-21: Address 3 (typically BSSID)
                # Bytes 22-23: Sequence Control
                
                if len(data) >= 24:
                    frame_controls.append(data[0:2])
                    # Note: In the mac.rs code, addresses are stored as:
                    # src_mac at 4-10, dst_mac at 10-16, bss_mac at 16-22
                    src_macs.append(data[4:10])
                    dst_macs.append(data[10:16])
                    bss_macs.append(data[16:22])
                
                # Print progress every 10 frames
                if frame_count % 10 == 0:
                    elapsed = (datetime.now() - start_time).total_seconds()
                    rate = frame_count / elapsed if elapsed > 0 else 0
                    print(f"\rReceived {frame_count} frames ({rate:.1f} frames/sec)...", end='', flush=True)
                    
            except socket.timeout:
                continue
                
    except KeyboardInterrupt:
        print("\n")
    
    # Analysis
    elapsed = (datetime.now() - start_time).total_seconds()
    
    print("=" * 60)
    print("ANALYSIS RESULTS")
    print("=" * 60)
    print(f"Duration: {elapsed:.1f} seconds")
    print(f"Total frames received: {frame_count}")
    if elapsed > 0:
        print(f"Average rate: {frame_count/elapsed:.1f} frames/sec")
    
    if frame_sizes:
        print(f"Frame sizes: min={min(frame_sizes)}, max={max(frame_sizes)}, avg={sum(frame_sizes)/len(frame_sizes):.1f}")
    
    print("\n" + "-" * 60)
    print("MAC ADDRESS ANALYSIS")
    print("-" * 60)
    
    results = []
    results.append(analyze_mac_randomness(src_macs, "Source MAC"))
    results.append(analyze_mac_randomness(dst_macs, "Dest MAC"))
    results.append(analyze_mac_randomness(bss_macs, "BSS MAC"))
    
    analyze_frame_control(frame_controls)
    
    # Overall assessment
    print("\n" + "=" * 60)
    print("OVERALL ASSESSMENT")
    print("=" * 60)
    
    if not results or all(r is None for r in results):
        print("No frames received - cannot assess")
    else:
        valid_results = [r for r in results if r is not None]
        avg_match = sum(r[0] for r in valid_results) / len(valid_results)
        avg_entropy = sum(r[1] for r in valid_results) / len(valid_results)
        
        print(f"Average MAC match rate: {avg_match*100:.1f}%")
        print(f"Average byte entropy: {avg_entropy:.2f} bits")
        
        if avg_match > 0.9:
            print("\n✓ TRANSMISSION QUALITY: EXCELLENT")
            print("  MAC addresses are correctly decoded in most frames.")
        elif avg_match > 0.7:
            print("\n⚠ TRANSMISSION QUALITY: GOOD")
            print("  Most MAC addresses decoded correctly, some corruption.")
        elif avg_match > 0.3:
            print("\n⚠ TRANSMISSION QUALITY: POOR")
            print("  Significant corruption - MACs appear semi-random.")
        else:
            print("\n✗ TRANSMISSION QUALITY: FAILED")
            print("  MAC addresses appear random - frames are corrupted.")
            print("  This indicates PHY layer issues (timing, equalization, etc.)")
        
        if avg_entropy > 6:
            print("\n  High entropy suggests bit errors spread across all bytes.")
            print("  Possible causes: timing drift, frequency offset, noise.")
        elif avg_entropy > 3:
            print("\n  Medium entropy - some structure preserved but corrupted.")
        else:
            print("\n  Low entropy - data has structure (good or stuck pattern).")


if __name__ == "__main__":
    main()
