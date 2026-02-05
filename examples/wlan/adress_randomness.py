#!/usr/bin/env python3
"""
MAC Address Randomness Analyzer

Analyzes the randomness of MAC addresses captured from WiFi traffic.
Reads from adress.txt and computes various statistical metrics.
"""

import re
import math
from collections import Counter
from pathlib import Path


def parse_mac_addresses(filepath: str) -> list[str]:
    """Extract all valid MAC addresses from the file."""
    mac_pattern = re.compile(r'([0-9a-fA-F]{2}(?::[0-9a-fA-F]{2}){5})')
    
    macs = []
    with open(filepath, 'r') as f:
        content = f.read()
        matches = mac_pattern.findall(content)
        macs.extend(matches)
    
    # Normalize to lowercase
    return [mac.lower() for mac in macs]


def mac_to_bytes(mac: str) -> list[int]:
    """Convert MAC address string to list of byte values."""
    return [int(b, 16) for b in mac.split(':')]


def is_locally_administered(mac: str) -> bool:
    """Check if MAC is locally administered (random) vs globally unique (OUI)."""
    first_byte = int(mac.split(':')[0], 16)
    return bool(first_byte & 0x02)


def is_unicast(mac: str) -> bool:
    """Check if MAC is unicast (vs multicast)."""
    first_byte = int(mac.split(':')[0], 16)
    return not bool(first_byte & 0x01)


def calculate_entropy(data: list[int], base: int = 256) -> float:
    """Calculate Shannon entropy of byte data."""
    if not data:
        return 0.0
    
    counter = Counter(data)
    total = len(data)
    entropy = 0.0
    
    for count in counter.values():
        if count > 0:
            prob = count / total
            entropy -= prob * math.log2(prob)
    
    # Normalize to 0-1 range (max entropy for bytes is 8 bits)
    return entropy / 8.0


def calculate_bit_entropy(data: list[int]) -> float:
    """Calculate entropy at bit level."""
    bits = []
    for byte in data:
        for i in range(8):
            bits.append((byte >> i) & 1)
    
    if not bits:
        return 0.0
    
    ones = sum(bits)
    zeros = len(bits) - ones
    total = len(bits)
    
    entropy = 0.0
    for count in [ones, zeros]:
        if count > 0:
            prob = count / total
            entropy -= prob * math.log2(prob)
    
    return entropy  # Max is 1.0 for perfectly random bits


def analyze_byte_distribution(macs: list[str]) -> dict:
    """Analyze the distribution of each byte position."""
    byte_positions = [[] for _ in range(6)]
    
    for mac in macs:
        bytes_val = mac_to_bytes(mac)
        for i, b in enumerate(bytes_val):
            byte_positions[i].append(b)
    
    results = {}
    for i, bytes_list in enumerate(byte_positions):
        counter = Counter(bytes_list)
        unique_values = len(counter)
        most_common = counter.most_common(5)
        entropy = calculate_entropy(bytes_list)
        
        results[f'byte_{i}'] = {
            'unique_values': unique_values,
            'entropy': entropy,
            'most_common': most_common
        }
    
    return results


def chi_square_uniformity(data: list[int], num_bins: int = 256) -> float:
    """
    Chi-square test for uniformity.
    Returns normalized chi-square (lower = more uniform/random).
    """
    if not data:
        return 0.0
    
    observed = Counter(data)
    expected = len(data) / num_bins
    
    chi_sq = 0.0
    for i in range(num_bins):
        obs = observed.get(i, 0)
        chi_sq += (obs - expected) ** 2 / expected
    
    # Return normalized chi-square (lower is better)
    return chi_sq / num_bins


def runs_test(bits: list[int]) -> dict:
    """
    Runs test for randomness.
    Counts runs of consecutive identical bits.
    """
    if len(bits) < 2:
        return {'runs': 0, 'expected_runs': 0, 'ratio': 0}
    
    runs = 1
    for i in range(1, len(bits)):
        if bits[i] != bits[i-1]:
            runs += 1
    
    n = len(bits)
    ones = sum(bits)
    zeros = n - ones
    
    if ones == 0 or zeros == 0:
        return {'runs': runs, 'expected_runs': 1, 'ratio': runs}
    
    # Expected number of runs for random sequence
    expected_runs = 1 + (2 * ones * zeros) / n
    
    return {
        'runs': runs,
        'expected_runs': expected_runs,
        'ratio': runs / expected_runs if expected_runs > 0 else 0
    }


def serial_correlation(data: list[int]) -> float:
    """Calculate serial correlation coefficient (lag-1 autocorrelation)."""
    if len(data) < 2:
        return 0.0
    
    mean = sum(data) / len(data)
    
    numerator = 0.0
    denominator = 0.0
    
    for i in range(len(data) - 1):
        numerator += (data[i] - mean) * (data[i+1] - mean)
        denominator += (data[i] - mean) ** 2
    
    # Add last element to denominator
    denominator += (data[-1] - mean) ** 2
    
    if denominator == 0:
        return 0.0
    
    return numerator / denominator


def nibble_analysis(macs: list[str]) -> dict:
    """Analyze distribution of hex nibbles (0-9, a-f)."""
    nibbles = []
    for mac in macs:
        # Remove colons and get each hex character
        hex_str = mac.replace(':', '')
        nibbles.extend(int(c, 16) for c in hex_str)
    
    counter = Counter(nibbles)
    entropy = 0.0
    total = len(nibbles)
    
    for count in counter.values():
        if count > 0:
            prob = count / total
            entropy -= prob * math.log2(prob)
    
    # Max entropy for 16 values is 4 bits
    return {
        'entropy': entropy / 4.0,
        'distribution': dict(sorted(counter.items()))
    }


def analyze_mac_addresses(filepath: str):
    """Main analysis function."""
    print("=" * 70)
    print("MAC Address Randomness Analysis")
    print("=" * 70)
    
    macs = parse_mac_addresses(filepath)
    unique_macs = list(set(macs))
    
    print(f"\nTotal MAC addresses found: {len(macs)}")
    print(f"Unique MAC addresses: {len(unique_macs)}")
    if macs:
        print(f"Duplicate ratio: {1 - len(unique_macs)/len(macs):.2%}")
    
    if not unique_macs:
        print("No MAC addresses found!")
        return
    
    # Analyze locally administered bit
    locally_admin = [mac for mac in unique_macs if is_locally_administered(mac)]
    unicast = [mac for mac in unique_macs if is_unicast(mac)]
    
    print(f"\n--- Address Type Analysis ---")
    print(f"Locally administered (randomized): {len(locally_admin)} ({len(locally_admin)/len(unique_macs)*100:.1f}%)")
    print(f"Globally unique (OUI-based): {len(unique_macs) - len(locally_admin)} ({(len(unique_macs)-len(locally_admin))/len(unique_macs)*100:.1f}%)")
    print(f"Unicast addresses: {len(unicast)} ({len(unicast)/len(unique_macs)*100:.1f}%)")
    print(f"Multicast addresses: {len(unique_macs) - len(unicast)} ({(len(unique_macs)-len(unicast))/len(unique_macs)*100:.1f}%)")
    
    # Collect all bytes for overall analysis
    all_bytes = []
    for mac in unique_macs:
        all_bytes.extend(mac_to_bytes(mac))
    
    # Overall entropy
    print(f"\n--- Entropy Analysis ---")
    byte_entropy = calculate_entropy(all_bytes)
    bit_entropy = calculate_bit_entropy(all_bytes)
    print(f"Byte-level entropy (normalized 0-1): {byte_entropy:.4f}")
    print(f"Bit-level entropy (0-1, 1=perfect): {bit_entropy:.4f}")
    print(f"  → {'Good randomness' if bit_entropy > 0.95 else 'Possible bias detected'}")
    
    # Nibble analysis
    nibble_result = nibble_analysis(unique_macs)
    print(f"Nibble entropy (0-1): {nibble_result['entropy']:.4f}")
    
    # Chi-square test
    chi_sq = chi_square_uniformity(all_bytes)
    print(f"\n--- Chi-Square Uniformity Test ---")
    print(f"Normalized chi-square: {chi_sq:.4f}")
    print(f"  → {'Good uniformity' if chi_sq < 2.0 else 'Non-uniform distribution'}")
    
    # Serial correlation
    corr = serial_correlation(all_bytes)
    print(f"\n--- Serial Correlation (lag-1) ---")
    print(f"Correlation coefficient: {corr:.4f}")
    print(f"  → {'Low correlation (good)' if abs(corr) < 0.1 else 'Correlation detected (patterns)'}")
    
    # Runs test on bits
    all_bits = []
    for b in all_bytes:
        for i in range(8):
            all_bits.append((b >> i) & 1)
    
    runs_result = runs_test(all_bits)
    print(f"\n--- Runs Test (bit sequences) ---")
    print(f"Actual runs: {runs_result['runs']}")
    print(f"Expected runs: {runs_result['expected_runs']:.0f}")
    print(f"Ratio (ideal ≈ 1.0): {runs_result['ratio']:.4f}")
    print(f"  → {'Good randomness' if 0.9 < runs_result['ratio'] < 1.1 else 'Possible pattern detected'}")
    
    # Per-byte analysis
    print(f"\n--- Per-Byte Position Analysis ---")
    byte_analysis = analyze_byte_distribution(unique_macs)
    
    for pos, data in byte_analysis.items():
        print(f"\n{pos.upper()}:")
        print(f"  Unique values: {data['unique_values']}/256")
        print(f"  Entropy (0-1): {data['entropy']:.4f}")
        print(f"  Most common: {', '.join([f'{hex(v)}({c})' for v, c in data['most_common'][:3]])}")
    
    # OUI analysis (first 3 bytes)
    print(f"\n--- OUI (Vendor) Analysis ---")
    ouis = [':'.join(mac.split(':')[:3]) for mac in unique_macs]
    oui_counter = Counter(ouis)
    print(f"Unique OUIs: {len(oui_counter)}")
    print(f"Top 10 OUIs:")
    for oui, count in oui_counter.most_common(10):
        pct = count / len(unique_macs) * 100
        # Check if locally administered
        la = " (locally administered)" if int(oui.split(':')[0], 16) & 0x02 else ""
        print(f"  {oui}: {count} ({pct:.1f}%){la}")
    
    # Hex character frequency
    print(f"\n--- Hex Character Distribution ---")
    all_hex = ''.join(mac.replace(':', '') for mac in unique_macs)
    hex_counter = Counter(all_hex)
    expected_freq = len(all_hex) / 16
    print(f"Expected frequency per char: {expected_freq:.1f}")
    print("Actual frequencies:")
    for char in '0123456789abcdef':
        actual = hex_counter.get(char, 0)
        deviation = (actual - expected_freq) / expected_freq * 100 if expected_freq > 0 else 0
        bar = '#' * int(actual / expected_freq * 10) if expected_freq > 0 else ''
        print(f"  {char}: {actual:4d} ({deviation:+5.1f}%) {bar}")
    
    # Summary score
    print(f"\n{'=' * 70}")
    print("RANDOMNESS SUMMARY")
    print("=" * 70)
    
    score = 0
    max_score = 6
    
    if bit_entropy > 0.95:
        score += 1
        print("✓ Bit entropy is high (good)")
    else:
        print("✗ Bit entropy is low (bias detected)")
    
    if 0.9 < runs_result['ratio'] < 1.1:
        score += 1
        print("✓ Runs test passed (no obvious patterns)")
    else:
        print("✗ Runs test failed (patterns detected)")
    
    if chi_sq < 2.0:
        score += 1
        print("✓ Chi-square test passed (uniform distribution)")
    else:
        print("✗ Chi-square test failed (non-uniform)")
    
    if abs(corr) < 0.1:
        score += 1
        print("✓ Low serial correlation (good)")
    else:
        print("✗ High serial correlation (sequential dependency)")
    
    if len(locally_admin) / len(unique_macs) > 0.5:
        score += 1
        print("✓ Majority of addresses are locally administered")
    else:
        print("✗ Many addresses use real OUIs")
    
    avg_byte_entropy = sum(d['entropy'] for d in byte_analysis.values()) / 6
    if avg_byte_entropy > 0.7:
        score += 1
        print("✓ Good per-byte entropy")
    else:
        print("✗ Low per-byte entropy")
    
    print(f"\nOverall Randomness Score: {score}/{max_score}")
    print(f"Assessment: ", end="")
    if score >= 5:
        print("HIGH randomness - addresses appear well randomized")
    elif score >= 3:
        print("MEDIUM randomness - some bias or patterns present")
    else:
        print("LOW randomness - significant patterns detected")


if __name__ == "__main__":
    import sys
    
    if len(sys.argv) > 1:
        filepath = sys.argv[1]
    else:
        filepath = Path(__file__).parent / "adress.txt"
    
    analyze_mac_addresses(str(filepath))
