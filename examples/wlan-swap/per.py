#!/usr/bin/env python3
"""
PER (Packet Error Rate) Analysis Script

Processes received_packets.txt to analyze packet reception at different TX gain levels.
Expected packet format: "testxxGG" where GG is the 2-digit gain value (00-88)

The TX sends 1000 packets at each gain level (88, 84, 80, ..., 4, 0)
This script counts received packets per gain and plots reception ratio.
"""

import re
import matplotlib.pyplot as plt
from collections import defaultdict
import sys

def parse_packets(filename):
    """
    Parse the received packets file and extract gain values.
    Returns a dict mapping gain -> count of received packets.
    """
    gain_counts = defaultdict(int)
    
    # Pattern to match "testxx" followed by 2 digits (gain value)
    # The packet may have binary garbage around it, so we search for the pattern
    pattern = re.compile(rb'testxx(\d{2})')
    
    try:
        with open(filename, 'rb') as f:
            data = f.read()
    except FileNotFoundError:
        print(f"Error: File '{filename}' not found")
        sys.exit(1)
    
    # Find all matches
    matches = pattern.findall(data)
    
    for match in matches:
        gain = int(match.decode('ascii'))
        gain_counts[gain] += 1
    
    return dict(gain_counts)

def analyze_per(gain_counts, expected_per_gain=1000):
    """
    Analyze PER (Packet Error Rate) for each gain level.
    Returns sorted list of (gain, received_count, reception_ratio)
    """
    results = []
    
    for gain, count in gain_counts.items():
        ratio = count / expected_per_gain
        results.append((gain, count, ratio))
    
    # Sort by gain (descending - highest gain first)
    results.sort(key=lambda x: x[0], reverse=True)
    
    return results

def gain_to_dbm(gain):
    """Convert gain setting to dBm (dBm = gain - 85)."""
    return gain - 85

def print_results(results, expected_per_gain=1000):
    """Print a summary table of the results."""
    print("\n" + "=" * 72)
    print("PER Test Results")
    print("=" * 72)
    print(f"{'Gain':<8} {'dBm':<8} {'Received':<12} {'Expected':<12} {'Ratio':<12} {'PER (%)':<12}")
    print("-" * 72)
    
    total_received = 0
    total_expected = 0
    
    for gain, count, ratio in results:
        per = (1 - ratio) * 100  # Packet Error Rate in %
        dbm = gain_to_dbm(gain)
        print(f"{gain:<8} {dbm:<8} {count:<12} {expected_per_gain:<12} {ratio:<12.3f} {per:<12.1f}")
        total_received += count
        total_expected += expected_per_gain
    
    print("-" * 72)
    overall_ratio = total_received / total_expected if total_expected > 0 else 0
    overall_per = (1 - overall_ratio) * 100
    print(f"{'TOTAL':<8} {'':<8} {total_received:<12} {total_expected:<12} {overall_ratio:<12.3f} {overall_per:<12.1f}")
    print("=" * 72)

def plot_results(results, output_file='per_results.png'):
    """
    Plot the reception ratio vs TX gain and dBm.
    """
    if not results:
        print("No data to plot!")
        return
    
    gains = [r[0] for r in results]
    dbm_values = [gain_to_dbm(g) for g in gains]
    ratios = [r[2] for r in results]
    counts = [r[1] for r in results]
    
    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(12, 10))
    
    # Plot 1: Reception Ratio vs dBm (primary) and Gain (secondary)
    ax1.plot(dbm_values, ratios, 'b-o', linewidth=2, markersize=8)
    ax1.set_xlabel('RX SNR (dB)', fontsize=12)
    ax1.set_ylabel('Reception Ratio (received/expected)', fontsize=12)
    ax1.set_title('Packet Reception Ratio vs TX Power', fontsize=14)
    ax1.grid(True, alpha=0.3)
    ax1.set_xlim(max(dbm_values) + 2, min(dbm_values) - 2)  # High power on left
    ax1.set_ylim(0, 1.1)
    ax1.axhline(y=1.0, color='g', linestyle='--', alpha=0.5, label='100% reception')
    ax1.axhline(y=0.5, color='orange', linestyle='--', alpha=0.5, label='50% reception')
    ax1.legend()
    
    # Add secondary x-axis for gain
    ax1_gain = ax1.twiny()
    ax1_gain.set_xlim(ax1.get_xlim()[0] + 85, ax1.get_xlim()[1] + 85)
    ax1_gain.set_xlabel('TX Gain Setting', fontsize=10, color='gray')
    ax1_gain.tick_params(axis='x', colors='gray')
    
    # Plot 2: Received Packet Count vs dBm (bar chart)
    colors = ['green' if r > 0.9 else 'orange' if r > 0.5 else 'red' for r in ratios]
    ax2.bar(dbm_values, counts, color=colors, edgecolor='black', alpha=0.7, width=3)
    ax2.set_xlabel('TX Power (dBm)', fontsize=12)
    ax2.set_ylabel('Received Packet Count', fontsize=12)
    ax2.set_title('Received Packets vs TX Power', fontsize=14)
    ax2.grid(True, alpha=0.3, axis='y')
    ax2.set_xlim(max(dbm_values) + 2, min(dbm_values) - 2)  # High power on left
    
    # Add secondary x-axis for gain
    ax2_gain = ax2.twiny()
    ax2_gain.set_xlim(ax2.get_xlim()[0] + 85, ax2.get_xlim()[1] + 85)
    ax2_gain.set_xlabel('TX Gain Setting', fontsize=10, color='gray')
    ax2_gain.tick_params(axis='x', colors='gray')
    
    # Add expected line
    expected = 10000
    ax2.axhline(y=expected, color='blue', linestyle='--', alpha=0.7, label=f'Expected ({expected})')
    ax2.legend()
    
    plt.tight_layout()
    plt.savefig(output_file, dpi=150)
    print(f"\nPlot saved to: {output_file}")
    plt.show()

def main():
    # Configuration
    input_file = 'received_packets.txt'
    expected_per_gain = 10000  # Number of packets sent at each gain level
    
    if len(sys.argv) > 1:
        input_file = sys.argv[1]
    if len(sys.argv) > 2:
        expected_per_gain = int(sys.argv[2])
    
    print(f"Processing: {input_file}")
    print(f"Expected packets per gain level: {expected_per_gain}")
    
    # Parse packets
    gain_counts = parse_packets(input_file)
    
    if not gain_counts:
        print("No packets found matching pattern 'testxxGG'")
        sys.exit(1)
    
    print(f"\nFound packets at {len(gain_counts)} different gain levels")
    
    # Analyze results
    results = analyze_per(gain_counts, expected_per_gain)
    
    # Print summary
    print_results(results, expected_per_gain)
    
    # Plot results
    plot_results(results)

if __name__ == '__main__':
    main()
