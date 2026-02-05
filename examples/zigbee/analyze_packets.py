#!/usr/bin/env python3
"""
ZigBee Packet Analyzer
Analyzes log.txt to find patterns, correlations, and packet groupings.
"""

import sys
import numpy as np
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from datetime import datetime
from collections import defaultdict
from dataclasses import dataclass
from typing import List, Dict, Tuple, Optional
import re

# Set style
plt.style.use('seaborn-v0_8-darkgrid')
plt.rcParams['figure.figsize'] = (12, 6)
plt.rcParams['font.size'] = 10

@dataclass
class Packet:
    timestamp: float
    size: int
    crc_status: str
    data: bytes
    
    @property
    def frame_control(self) -> int:
        if len(self.data) >= 2:
            return self.data[0] | (self.data[1] << 8)
        return 0
    
    @property
    def sequence_number(self) -> int:
        if len(self.data) >= 3:
            return self.data[2]
        return 0
    
    @property
    def frame_type(self) -> str:
        fc = self.frame_control
        frame_types = {
            0: "Beacon",
            1: "Data", 
            2: "Ack",
            3: "MAC Cmd"
        }
        return frame_types.get(fc & 0x07, "Unknown")


def parse_log(filename: str) -> List[Packet]:
    """Parse log.txt and return list of Packet objects."""
    packets = []
    
    try:
        with open(filename, 'r') as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                
                parts = line.split(',', 3)
                if len(parts) < 4:
                    continue
                
                try:
                    timestamp = float(parts[0])
                    size = int(parts[1])
                    crc_status = parts[2]
                    hex_data = parts[3].replace(' ', '')
                    data = bytes.fromhex(hex_data)
                    
                    packets.append(Packet(
                        timestamp=timestamp,
                        size=size,
                        crc_status=crc_status,
                        data=data
                    ))
                except (ValueError, IndexError) as e:
                    print(f"Warning: Could not parse line: {line[:50]}... ({e})")
                    continue
                    
    except FileNotFoundError:
        print(f"Error: {filename} not found. Run the receiver first to generate data.")
        sys.exit(1)
    
    return packets


def analyze_timing(packets: List[Packet]) -> None:
    """Analyze packet timing patterns."""
    print("\n" + "="*60)
    print("TIMING ANALYSIS")
    print("="*60)
    
    if len(packets) < 2:
        print("Not enough packets for timing analysis")
        return
    
    # Calculate inter-packet intervals
    intervals = []
    for i in range(1, len(packets)):
        dt = packets[i].timestamp - packets[i-1].timestamp
        intervals.append(dt)
    
    intervals = np.array(intervals)
    
    print(f"\nTotal packets: {len(packets)}")
    print(f"Time span: {packets[-1].timestamp - packets[0].timestamp:.2f} seconds")
    print(f"\nInter-packet intervals (ms):")
    print(f"  Min:    {intervals.min()*1000:.2f}")
    print(f"  Max:    {intervals.max()*1000:.2f}")
    print(f"  Mean:   {intervals.mean()*1000:.2f}")
    print(f"  Median: {np.median(intervals)*1000:.2f}")
    print(f"  Std:    {intervals.std()*1000:.2f}")
    
    # Find clusters of packets (gaps > 100ms indicate new group)
    gap_threshold = 0.1  # 100ms
    groups = []
    current_group = [packets[0]]
    
    for i in range(1, len(packets)):
        if intervals[i-1] > gap_threshold:
            groups.append(current_group)
            current_group = [packets[i]]
        else:
            current_group.append(packets[i])
    
    if current_group:
        groups.append(current_group)
    
    print(f"\n\nPACKET GROUPS (gap > {gap_threshold*1000:.0f}ms):")
    print(f"Found {len(groups)} groups")
    
    # Analyze group composition
    group_signatures = defaultdict(int)
    for group in groups:
        sizes = tuple(p.size for p in group)
        group_signatures[sizes] += 1
    
    print("\nGroup compositions (sizes -> count):")
    for sizes, count in sorted(group_signatures.items(), key=lambda x: -x[1])[:10]:
        print(f"  {sizes} -> {count} times")
    
    # Group interval analysis
    if len(groups) >= 2:
        group_intervals = []
        for i in range(1, len(groups)):
            dt = groups[i][0].timestamp - groups[i-1][0].timestamp
            group_intervals.append(dt)
        
        group_intervals = np.array(group_intervals)
        print(f"\nGroup intervals (seconds):")
        print(f"  Min:    {group_intervals.min():.3f}")
        print(f"  Max:    {group_intervals.max():.3f}")
        print(f"  Mean:   {group_intervals.mean():.3f}")
        print(f"  Median: {np.median(group_intervals):.3f}")


def analyze_sizes(packets: List[Packet]) -> None:
    """Analyze packet size distribution."""
    print("\n" + "="*60)
    print("SIZE ANALYSIS")
    print("="*60)
    
    size_counts = defaultdict(int)
    for p in packets:
        size_counts[p.size] += 1
    
    print("\nPacket sizes (bytes -> count):")
    for size, count in sorted(size_counts.items()):
        pct = count / len(packets) * 100
        bar = "#" * int(pct / 2)
        print(f"  {size:3d} bytes: {count:5d} ({pct:5.1f}%) {bar}")


def analyze_sequence_numbers(packets: List[Packet]) -> None:
    """Analyze sequence number patterns."""
    print("\n" + "="*60)
    print("SEQUENCE NUMBER ANALYSIS")
    print("="*60)
    
    # Group by size first
    by_size = defaultdict(list)
    for p in packets:
        by_size[p.size].append(p)
    
    for size, pkts in sorted(by_size.items()):
        if len(pkts) < 2:
            continue
            
        seq_nums = [p.sequence_number for p in pkts]
        
        # Calculate sequence number deltas
        deltas = []
        for i in range(1, len(seq_nums)):
            delta = (seq_nums[i] - seq_nums[i-1]) % 256
            deltas.append(delta)
        
        if deltas:
            print(f"\nSize {size} bytes ({len(pkts)} packets):")
            print(f"  Sequence range: {min(seq_nums)} - {max(seq_nums)}")
            
            delta_counts = defaultdict(int)
            for d in deltas:
                delta_counts[d] += 1
            
            print("  Sequence deltas:")
            for d, count in sorted(delta_counts.items(), key=lambda x: -x[1])[:5]:
                print(f"    delta={d}: {count} times ({count/len(deltas)*100:.1f}%)")


def analyze_data_correlation(packets: List[Packet]) -> None:
    """Analyze data byte correlations between consecutive packets."""
    print("\n" + "="*60)
    print("DATA CORRELATION ANALYSIS")
    print("="*60)
    
    # Group by size
    by_size = defaultdict(list)
    for p in packets:
        by_size[p.size].append(p)
    
    for size, pkts in sorted(by_size.items()):
        if len(pkts) < 2 or size < 5:
            continue
        
        print(f"\nSize {size} bytes ({len(pkts)} packets):")
        
        # Find bytes that are constant across all packets
        constant_bytes = []
        varying_bytes = []
        
        for byte_idx in range(min(size, 20)):  # Check first 20 bytes
            values = set()
            for p in pkts:
                if byte_idx < len(p.data):
                    values.add(p.data[byte_idx])
            
            if len(values) == 1:
                constant_bytes.append((byte_idx, list(values)[0]))
            else:
                varying_bytes.append((byte_idx, len(values)))
        
        if constant_bytes:
            print("  Constant bytes (position: value):")
            for idx, val in constant_bytes[:10]:
                print(f"    [{idx:2d}]: 0x{val:02x}")
        
        if varying_bytes:
            print("  Varying bytes (position: unique_values):")
            for idx, count in varying_bytes[:10]:
                print(f"    [{idx:2d}]: {count} unique values")
        
        # Show all packets hex dump
        print(f"\n  All {len(pkts)} packets (hex dump, first 20 bytes):")
        for i, p in enumerate(pkts):
            hex_str = ' '.join(f'{b:02x}' for b in p.data[:20])
            print(f"    #{i+1}: {hex_str}")


def analyze_frame_types(packets: List[Packet]) -> None:
    """Analyze ZigBee frame types."""
    print("\n" + "="*60)
    print("FRAME TYPE ANALYSIS")
    print("="*60)
    
    type_counts = defaultdict(int)
    fc_counts = defaultdict(int)
    
    for p in packets:
        type_counts[p.frame_type] += 1
        fc_counts[p.frame_control] += 1
    
    print("\nFrame types:")
    for ftype, count in sorted(type_counts.items(), key=lambda x: -x[1]):
        pct = count / len(packets) * 100
        print(f"  {ftype}: {count} ({pct:.1f}%)")
    
    print("\nFrame control values (top 10):")
    for fc, count in sorted(fc_counts.items(), key=lambda x: -x[1])[:10]:
        pct = count / len(packets) * 100
        # Parse frame control
        frame_type = fc & 0x07
        security = (fc >> 3) & 0x01
        pending = (fc >> 4) & 0x01
        ack_req = (fc >> 5) & 0x01
        pan_compress = (fc >> 6) & 0x01
        
        print(f"  0x{fc:04x}: {count} ({pct:.1f}%) - type={frame_type}, sec={security}, ack={ack_req}, pan_comp={pan_compress}")


def analyze_addresses(packets: List[Packet]) -> None:
    """Analyze source/destination addresses."""
    print("\n" + "="*60)
    print("ADDRESS ANALYSIS")
    print("="*60)
    
    # For data frames with PAN ID compression (0x8861 or 0x6188)
    address_pairs = defaultdict(int)
    pan_ids = defaultdict(int)
    
    for p in packets:
        if len(p.data) >= 15:  # Need enough data for addresses
            fc = p.frame_control
            
            # Check for data frame with addressing
            if (fc & 0x07) == 1:  # Data frame
                try:
                    # Seq number at byte 2
                    # PAN ID at bytes 3-4
                    # Dest addr at bytes 5-6 (or 5-8)
                    # Source addr after dest
                    
                    pan_id = p.data[3] | (p.data[4] << 8)
                    pan_ids[pan_id] += 1
                    
                    # Short addressing assumed
                    dst_addr = p.data[5] | (p.data[6] << 8)
                    src_addr = p.data[7] | (p.data[8] << 8)
                    
                    address_pairs[(src_addr, dst_addr)] += 1
                except IndexError:
                    pass
    
    print("\nPAN IDs:")
    for pan_id, count in sorted(pan_ids.items(), key=lambda x: -x[1])[:10]:
        print(f"  0x{pan_id:04x}: {count}")
    
    print("\nAddress pairs (src -> dst):")
    for (src, dst), count in sorted(address_pairs.items(), key=lambda x: -x[1])[:10]:
        print(f"  0x{src:04x} -> 0x{dst:04x}: {count}")


def analyze_crc(packets: List[Packet]) -> None:
    """Analyze CRC status."""
    print("\n" + "="*60)
    print("CRC ANALYSIS")
    print("="*60)
    
    ok_count = sum(1 for p in packets if p.crc_status == "OK")
    fail_count = sum(1 for p in packets if p.crc_status == "FAIL")
    
    print(f"\nCRC OK:   {ok_count} ({ok_count/len(packets)*100:.1f}%)")
    print(f"CRC FAIL: {fail_count} ({fail_count/len(packets)*100:.1f}%)")
    
    # Analyze by size
    print("\nCRC status by packet size:")
    by_size = defaultdict(lambda: {"OK": 0, "FAIL": 0})
    for p in packets:
        by_size[p.size][p.crc_status] += 1
    
    for size in sorted(by_size.keys()):
        ok = by_size[size]["OK"]
        fail = by_size[size]["FAIL"]
        total = ok + fail
        print(f"  {size:3d} bytes: OK={ok:4d}, FAIL={fail:4d} ({ok/total*100:5.1f}% OK)")


def print_summary_table(packets: List[Packet]) -> None:
    """Print a summary table of first N packets."""
    print("\n" + "="*60)
    print("FIRST 30 PACKETS")
    print("="*60)
    
    print(f"\n{'#':>3} {'Time':>12} {'Size':>5} {'CRC':>4} {'Type':>8} {'Seq':>4} {'First 10 bytes'}")
    print("-" * 70)
    
    start_time = packets[0].timestamp if packets else 0
    
    for i, p in enumerate(packets[:30]):
        rel_time = p.timestamp - start_time
        hex_preview = ' '.join(f'{b:02x}' for b in p.data[:10])
        print(f"{i+1:3d} {rel_time:12.6f} {p.size:5d} {p.crc_status:>4} {p.frame_type:>8} {p.sequence_number:4d} {hex_preview}")


def main():
    logfile = "log.txt"
    if len(sys.argv) > 1:
        logfile = sys.argv[1]
    
    print(f"Parsing {logfile}...")
    packets = parse_log(logfile)
    
    if not packets:
        print("No packets found in log file!")
        return
    
    print(f"Loaded {len(packets)} packets")
    
    # Run all analyses
    print_summary_table(packets)
    analyze_crc(packets)
    analyze_timing(packets)
    analyze_sizes(packets)
    analyze_frame_types(packets)
    analyze_sequence_numbers(packets)
    analyze_data_correlation(packets)
    analyze_addresses(packets)
    
    print("\n" + "="*60)
    print("ANALYSIS COMPLETE")
    print("="*60)
    
    # Generate graphs
    print("\nGenerating graphs...")
    plot_all_graphs(packets)
    
    # Generate packet correlation graphs
    print("\nGenerating packet correlation analysis...")
    plot_packet_correlations(packets)


def plot_packet_correlations(packets: List[Packet]) -> None:
    """Generate correlation heatmaps between packets of the same size."""
    
    # Group packets by size
    by_size = defaultdict(list)
    for p in packets:
        by_size[p.size].append(p)
    
    # Only analyze sizes with enough packets
    sizes_to_analyze = [(size, pkts) for size, pkts in by_size.items() 
                        if len(pkts) >= 5 and size >= 10]
    sizes_to_analyze.sort(key=lambda x: -len(x[1]))  # Sort by count descending
    
    if not sizes_to_analyze:
        print("Not enough packets for correlation analysis")
        return
    
    # Analyze top 6 most common sizes
    sizes_to_analyze = sizes_to_analyze[:6]
    
    n_plots = len(sizes_to_analyze)
    cols = 2
    rows = (n_plots + 1) // 2
    
    fig, axes = plt.subplots(rows, cols, figsize=(16, 5 * rows))
    axes = axes.flatten() if n_plots > 1 else [axes]
    
    for idx, (size, pkts) in enumerate(sizes_to_analyze):
        ax = axes[idx]
        
        # Use ALL packets for visualization (no limit)
        n_pkts = len(pkts)
        
        # Create correlation matrix
        corr_matrix = np.zeros((n_pkts, n_pkts))
        
        for i in range(n_pkts):
            for j in range(n_pkts):
                # Calculate byte-level similarity (Hamming distance normalized)
                d1, d2 = pkts[i].data, pkts[j].data
                min_len = min(len(d1), len(d2))
                if min_len > 0:
                    matching = sum(1 for k in range(min_len) if d1[k] == d2[k])
                    corr_matrix[i, j] = matching / min_len
                else:
                    corr_matrix[i, j] = 0
        
        im = ax.imshow(corr_matrix, cmap='viridis', aspect='auto', vmin=0, vmax=1)
        ax.set_xlabel('Packet Index')
        ax.set_ylabel('Packet Index')
        ax.set_title(f'{size}-byte Packets Correlation (All {n_pkts} packets)')
        plt.colorbar(im, ax=ax, label='Similarity (0-1)')
    
    # Hide unused axes
    for idx in range(len(sizes_to_analyze), len(axes)):
        axes[idx].set_visible(False)
    
    plt.tight_layout()
    plt.savefig('zigbee_packet_correlation.png', dpi=150, bbox_inches='tight')
    print("Saved: zigbee_packet_correlation.png")
    
    # ========== Second figure: Detailed byte-level correlation ==========
    fig2, axes2 = plt.subplots(2, 3, figsize=(18, 10))
    axes2 = axes2.flatten()
    
    for idx, (size, pkts) in enumerate(sizes_to_analyze[:6]):
        ax = axes2[idx]
        pkts = pkts[:100]  # Use more packets for byte analysis
        
        if len(pkts) < 2:
            continue
        
        # Analyze byte-by-byte correlation across all packets
        min_len = min(len(p.data) for p in pkts)
        max_bytes = min(min_len, 30)  # First 30 bytes
        
        # Create byte matrix
        byte_matrix = np.array([[p.data[i] for i in range(max_bytes)] for p in pkts])
        
        # Calculate correlation matrix between byte positions
        byte_corr = np.zeros((max_bytes, max_bytes))
        for i in range(max_bytes):
            for j in range(max_bytes):
                if np.std(byte_matrix[:, i]) > 0 and np.std(byte_matrix[:, j]) > 0:
                    byte_corr[i, j] = np.corrcoef(byte_matrix[:, i], byte_matrix[:, j])[0, 1]
                else:
                    byte_corr[i, j] = 1.0 if i == j else 0.0
        
        im = ax.imshow(byte_corr, cmap='RdBu_r', aspect='auto', vmin=-1, vmax=1)
        ax.set_xlabel('Byte Position')
        ax.set_ylabel('Byte Position')
        ax.set_title(f'{size}B: Byte Position Correlation')
        plt.colorbar(im, ax=ax, label='Correlation')
    
    plt.tight_layout()
    plt.savefig('zigbee_byte_correlation.png', dpi=150, bbox_inches='tight')
    print("Saved: zigbee_byte_correlation.png")
    
    # ========== Third figure: Packet similarity clustering ==========
    fig3, axes3 = plt.subplots(2, 3, figsize=(18, 10))
    axes3 = axes3.flatten()
    
    for idx, (size, pkts) in enumerate(sizes_to_analyze[:6]):
        ax = axes3[idx]
        pkts_subset = pkts[:100]
        n_pkts = len(pkts_subset)
        
        if n_pkts < 3:
            continue
        
        # Calculate pairwise XOR distance (number of different bits)
        def xor_distance(p1, p2):
            d1, d2 = p1.data, p2.data
            min_len = min(len(d1), len(d2))
            diff_bits = 0
            for k in range(min_len):
                xor = d1[k] ^ d2[k]
                diff_bits += bin(xor).count('1')
            return diff_bits
        
        # Create distance to first packet for each packet
        distances = [xor_distance(pkts_subset[0], p) for p in pkts_subset]
        
        # Plot distance progression
        ax.plot(range(n_pkts), distances, 'b.-', alpha=0.7, markersize=3)
        ax.axhline(y=np.mean(distances), color='red', linestyle='--', 
                   label=f'Mean: {np.mean(distances):.1f} bits')
        ax.set_xlabel('Packet Index')
        ax.set_ylabel('XOR Distance to First Packet (bits)')
        ax.set_title(f'{size}B: Bit Difference from First Packet')
        ax.legend()
    
    plt.tight_layout()
    plt.savefig('zigbee_xor_distance.png', dpi=150, bbox_inches='tight')
    print("Saved: zigbee_xor_distance.png")
    
    # ========== Fourth figure: Unique patterns analysis ==========
    print("\n" + "="*60)
    print("UNIQUE PACKET PATTERNS BY SIZE")
    print("="*60)
    
    fig4, axes4 = plt.subplots(2, 3, figsize=(18, 10))
    axes4 = axes4.flatten()
    
    for idx, (size, pkts) in enumerate(sizes_to_analyze[:6]):
        ax = axes4[idx]
        
        # Find unique packet patterns (by data content)
        unique_patterns = {}
        pattern_counts = []
        
        for p in pkts:
            # Use first 20 bytes as pattern key
            pattern_key = tuple(p.data[:min(20, len(p.data))])
            if pattern_key not in unique_patterns:
                unique_patterns[pattern_key] = len(unique_patterns)
            pattern_counts.append(unique_patterns[pattern_key])
        
        # Count occurrences of each pattern
        pattern_freq = defaultdict(int)
        for pc in pattern_counts:
            pattern_freq[pc] += 1
        
        freq_values = sorted(pattern_freq.values(), reverse=True)[:20]  # Top 20
        
        ax.bar(range(len(freq_values)), freq_values, color='teal', alpha=0.7)
        ax.set_xlabel('Pattern Rank')
        ax.set_ylabel('Occurrences')
        ax.set_title(f'{size}B: {len(unique_patterns)} unique patterns in {len(pkts)} packets')
        
        print(f"\n{size}-byte packets: {len(unique_patterns)} unique patterns in {len(pkts)} packets ({len(unique_patterns)/len(pkts)*100:.1f}% unique)")
    
    plt.tight_layout()
    plt.savefig('zigbee_unique_patterns.png', dpi=150, bbox_inches='tight')
    print("\nSaved: zigbee_unique_patterns.png")
    
    plt.show()


def plot_all_graphs(packets: List[Packet]) -> None:
    """Generate all visualization graphs."""
    
    if len(packets) < 2:
        print("Not enough packets for visualization")
        return
    
    start_time = packets[0].timestamp
    
    # Create figure with subplots
    fig = plt.figure(figsize=(16, 20))
    
    # ========== 1. Packet frequency over time ==========
    ax1 = fig.add_subplot(4, 2, 1)
    times = [(p.timestamp - start_time) for p in packets]
    
    # Bin into 1-second intervals
    max_time = max(times)
    bins = np.arange(0, max_time + 1, 1)
    hist, bin_edges = np.histogram(times, bins=bins)
    
    ax1.bar(bin_edges[:-1], hist, width=0.9, alpha=0.7, color='steelblue')
    ax1.set_xlabel('Time (seconds)')
    ax1.set_ylabel('Packets per second')
    ax1.set_title('Packet Frequency Over Time')
    ax1.axhline(y=np.mean(hist), color='red', linestyle='--', label=f'Mean: {np.mean(hist):.1f} pkt/s')
    ax1.legend()
    
    # ========== 2. Inter-packet interval distribution ==========
    ax2 = fig.add_subplot(4, 2, 2)
    intervals = []
    for i in range(1, len(packets)):
        dt = (packets[i].timestamp - packets[i-1].timestamp) * 1000  # ms
        intervals.append(dt)
    
    intervals = np.array(intervals)
    # Filter to show detail (exclude very large gaps)
    intervals_filtered = intervals[intervals < 500]
    
    ax2.hist(intervals_filtered, bins=50, alpha=0.7, color='coral', edgecolor='black')
    ax2.set_xlabel('Inter-packet interval (ms)')
    ax2.set_ylabel('Count')
    ax2.set_title('Inter-Packet Interval Distribution (<500ms)')
    ax2.axvline(x=np.median(intervals_filtered), color='red', linestyle='--', 
                label=f'Median: {np.median(intervals_filtered):.2f}ms')
    ax2.legend()
    
    # ========== 3. Packet size distribution ==========
    ax3 = fig.add_subplot(4, 2, 3)
    sizes = [p.size for p in packets]
    unique_sizes = sorted(set(sizes))
    size_counts = [sizes.count(s) for s in unique_sizes]
    
    bars = ax3.bar(range(len(unique_sizes)), size_counts, color='seagreen', alpha=0.7)
    ax3.set_xticks(range(len(unique_sizes)))
    ax3.set_xticklabels([str(s) for s in unique_sizes], rotation=45)
    ax3.set_xlabel('Packet Size (bytes)')
    ax3.set_ylabel('Count')
    ax3.set_title('Packet Size Distribution')
    
    # Add value labels on bars
    for bar, count in zip(bars, size_counts):
        if count > 10:
            ax3.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 5, 
                    str(count), ha='center', va='bottom', fontsize=8)
    
    # ========== 4. Packet size over time (scatter) ==========
    ax4 = fig.add_subplot(4, 2, 4)
    colors = {'OK': 'green', 'FAIL': 'red'}
    for crc_status in ['OK', 'FAIL']:
        mask = [p.crc_status == crc_status for p in packets]
        t = [times[i] for i in range(len(packets)) if mask[i]]
        s = [sizes[i] for i in range(len(packets)) if mask[i]]
        if t:
            ax4.scatter(t, s, alpha=0.5, s=10, c=colors[crc_status], label=f'CRC {crc_status}')
    
    ax4.set_xlabel('Time (seconds)')
    ax4.set_ylabel('Packet Size (bytes)')
    ax4.set_title('Packet Size Over Time')
    ax4.legend()
    
    # ========== 5. Sequence number progression ==========
    ax5 = fig.add_subplot(4, 2, 5)
    
    # Filter by common sizes
    for size, color, label in [(45, 'blue', '45B Data'), (52, 'orange', '52B Data'), (3, 'gray', '3B Ack')]:
        mask_pkts = [(p.timestamp - start_time, p.sequence_number) for p in packets if p.size == size]
        if mask_pkts:
            t, seq = zip(*mask_pkts[:500])  # Limit points for clarity
            ax5.scatter(t, seq, alpha=0.6, s=15, c=color, label=label)
    
    ax5.set_xlabel('Time (seconds)')
    ax5.set_ylabel('Sequence Number')
    ax5.set_title('Sequence Number Progression by Packet Size')
    ax5.legend()
    
    # ========== 6. Byte correlation heatmap for 45-byte packets ==========
    ax6 = fig.add_subplot(4, 2, 6)
    
    packets_45 = [p for p in packets if p.size == 45][:100]  # First 100
    if len(packets_45) >= 10:
        # Create matrix of first 20 bytes
        matrix = np.array([[b for b in p.data[:20]] for p in packets_45])
        
        # Calculate byte variance (normalized)
        byte_variance = np.var(matrix, axis=0)
        byte_variance_norm = byte_variance / (byte_variance.max() + 1e-6)
        
        # Show as bar chart
        ax6.bar(range(20), byte_variance_norm, color='purple', alpha=0.7)
        ax6.set_xlabel('Byte Position')
        ax6.set_ylabel('Normalized Variance')
        ax6.set_title('Byte Variance in 45-byte Packets (first 20 bytes)')
        ax6.set_xticks(range(20))
    else:
        ax6.text(0.5, 0.5, 'Not enough 45-byte packets', ha='center', va='center')
        ax6.set_title('Byte Variance (insufficient data)')
    
    # ========== 7. Group interval histogram ==========
    ax7 = fig.add_subplot(4, 2, 7)
    
    # Detect groups (gap > 100ms)
    gap_threshold = 0.1
    group_starts = [0]
    for i in range(1, len(packets)):
        if (packets[i].timestamp - packets[i-1].timestamp) > gap_threshold:
            group_starts.append(i)
    
    if len(group_starts) >= 2:
        group_intervals = []
        for i in range(1, len(group_starts)):
            dt = packets[group_starts[i]].timestamp - packets[group_starts[i-1]].timestamp
            group_intervals.append(dt * 1000)  # ms
        
        group_intervals = np.array(group_intervals)
        ax7.hist(group_intervals, bins=30, alpha=0.7, color='teal', edgecolor='black')
        ax7.set_xlabel('Group Interval (ms)')
        ax7.set_ylabel('Count')
        ax7.set_title(f'Packet Group Interval Distribution ({len(group_starts)} groups)')
        ax7.axvline(x=np.median(group_intervals), color='red', linestyle='--',
                   label=f'Median: {np.median(group_intervals):.0f}ms')
        ax7.legend()
    else:
        ax7.text(0.5, 0.5, 'Not enough groups detected', ha='center', va='center')
    
    # ========== 8. Frame type pie chart ==========
    ax8 = fig.add_subplot(4, 2, 8)
    
    frame_types = defaultdict(int)
    for p in packets:
        frame_types[p.frame_type] += 1
    
    labels = list(frame_types.keys())
    values = list(frame_types.values())
    colors_pie = plt.cm.Set3(np.linspace(0, 1, len(labels)))
    
    wedges, texts, autotexts = ax8.pie(values, labels=labels, autopct='%1.1f%%', 
                                        colors=colors_pie, startangle=90)
    ax8.set_title('Frame Type Distribution')
    
    plt.tight_layout()
    plt.savefig('zigbee_analysis.png', dpi=150, bbox_inches='tight')
    print("Saved: zigbee_analysis.png")
    
    # ========== Additional figure: Detailed timing analysis ==========
    fig2, axes2 = plt.subplots(2, 2, figsize=(14, 10))
    
    # 1. Zoomed packet burst (first 50 packets)
    ax = axes2[0, 0]
    first_n = min(50, len(packets))
    t_first = [(p.timestamp - start_time) * 1000 for p in packets[:first_n]]
    s_first = [p.size for p in packets[:first_n]]
    
    ax.stem(t_first, s_first, linefmt='b-', markerfmt='bo', basefmt='k-')
    ax.set_xlabel('Time (ms)')
    ax.set_ylabel('Packet Size (bytes)')
    ax.set_title(f'First {first_n} Packets - Timing Detail')
    
    # 2. Address distribution
    ax = axes2[0, 1]
    address_counts = defaultdict(int)
    for p in packets:
        if len(p.data) >= 9:
            addr = p.data[7] | (p.data[8] << 8)
            address_counts[addr] += 1
    
    top_addrs = sorted(address_counts.items(), key=lambda x: -x[1])[:10]
    if top_addrs:
        addr_labels = [f'0x{a:04x}' for a, _ in top_addrs]
        addr_values = [v for _, v in top_addrs]
        ax.barh(range(len(top_addrs)), addr_values, color='indianred', alpha=0.7)
        ax.set_yticks(range(len(top_addrs)))
        ax.set_yticklabels(addr_labels)
        ax.set_xlabel('Packet Count')
        ax.set_title('Top 10 Source Addresses')
        ax.invert_yaxis()
    
    # 3. Correlation: consecutive packet size pairs
    ax = axes2[1, 0]
    size_pairs = [(packets[i].size, packets[i+1].size) for i in range(len(packets)-1)]
    pair_counts = defaultdict(int)
    for pair in size_pairs:
        pair_counts[pair] += 1
    
    # Create heatmap matrix for common sizes
    common_sizes = [3, 45, 47, 52, 96, 99]
    matrix = np.zeros((len(common_sizes), len(common_sizes)))
    for i, s1 in enumerate(common_sizes):
        for j, s2 in enumerate(common_sizes):
            matrix[i, j] = pair_counts.get((s1, s2), 0)
    
    im = ax.imshow(matrix, cmap='YlOrRd', aspect='auto')
    ax.set_xticks(range(len(common_sizes)))
    ax.set_yticks(range(len(common_sizes)))
    ax.set_xticklabels([str(s) for s in common_sizes])
    ax.set_yticklabels([str(s) for s in common_sizes])
    ax.set_xlabel('Next Packet Size')
    ax.set_ylabel('Current Packet Size')
    ax.set_title('Consecutive Packet Size Correlation')
    
    # Add text annotations
    for i in range(len(common_sizes)):
        for j in range(len(common_sizes)):
            val = int(matrix[i, j])
            if val > 0:
                ax.text(j, i, str(val), ha='center', va='center', 
                       color='white' if val > matrix.max()/2 else 'black', fontsize=8)
    
    plt.colorbar(im, ax=ax, label='Count')
    
    # 4. Packet rate rolling average
    ax = axes2[1, 1]
    
    # Calculate rolling packet rate (packets per 5-second window)
    window_size = 5  # seconds
    rate_times = []
    rate_values = []
    
    for t in np.arange(0, max(times) - window_size, 1):
        count = sum(1 for pt in times if t <= pt < t + window_size)
        rate_times.append(t + window_size/2)
        rate_values.append(count / window_size)
    
    ax.plot(rate_times, rate_values, 'b-', linewidth=1.5, alpha=0.8)
    ax.fill_between(rate_times, rate_values, alpha=0.3)
    ax.set_xlabel('Time (seconds)')
    ax.set_ylabel('Packets per second')
    ax.set_title(f'Packet Rate Over Time ({window_size}s rolling window)')
    ax.axhline(y=np.mean(rate_values), color='red', linestyle='--', 
               label=f'Mean: {np.mean(rate_values):.1f} pkt/s')
    ax.legend()
    
    plt.tight_layout()
    plt.savefig('zigbee_timing.png', dpi=150, bbox_inches='tight')
    print("Saved: zigbee_timing.png")
    
    # ========== Third figure: Byte-level analysis ==========
    fig3, axes3 = plt.subplots(2, 2, figsize=(14, 10))
    
    # 1. First byte distribution
    ax = axes3[0, 0]
    first_bytes = [p.data[0] if len(p.data) > 0 else 0 for p in packets]
    fb_counts = defaultdict(int)
    for fb in first_bytes:
        fb_counts[fb] += 1
    
    fb_items = sorted(fb_counts.items(), key=lambda x: -x[1])[:10]
    fb_labels = [f'0x{b:02x}' for b, _ in fb_items]
    fb_values = [v for _, v in fb_items]
    
    ax.bar(fb_labels, fb_values, color='slateblue', alpha=0.7)
    ax.set_xlabel('First Byte Value')
    ax.set_ylabel('Count')
    ax.set_title('Distribution of First Byte (Frame Control Low)')
    
    # 2. Byte position entropy for data packets
    ax = axes3[0, 1]
    data_packets = [p for p in packets if p.size >= 20 and p.size not in [3]][:200]
    
    if len(data_packets) >= 10:
        matrix = np.array([[b for b in p.data[:20]] for p in data_packets])
        
        # Calculate entropy for each byte position
        def byte_entropy(column):
            _, counts = np.unique(column, return_counts=True)
            probs = counts / len(column)
            return -np.sum(probs * np.log2(probs + 1e-10))
        
        entropies = [byte_entropy(matrix[:, i]) for i in range(20)]
        
        ax.bar(range(20), entropies, color='darkorange', alpha=0.7)
        ax.set_xlabel('Byte Position')
        ax.set_ylabel('Entropy (bits)')
        ax.set_title('Byte Entropy in Data Packets (first 20 bytes)')
        ax.set_xticks(range(20))
        ax.axhline(y=8, color='red', linestyle='--', alpha=0.5, label='Max entropy (8 bits)')
        ax.legend()
    
    # 3. Sequence number delta distribution for main packet types
    ax = axes3[1, 0]
    
    for size, color in [(45, 'blue'), (52, 'orange'), (3, 'green')]:
        size_pkts = [p for p in packets if p.size == size]
        if len(size_pkts) >= 2:
            deltas = [(size_pkts[i].sequence_number - size_pkts[i-1].sequence_number) % 256 
                     for i in range(1, len(size_pkts))]
            ax.hist(deltas, bins=50, alpha=0.5, label=f'{size}B', color=color)
    
    ax.set_xlabel('Sequence Number Delta')
    ax.set_ylabel('Count')
    ax.set_title('Sequence Number Delta Distribution by Packet Size')
    ax.legend()
    
    # 4. Packet size autocorrelation
    ax = axes3[1, 1]
    
    sizes_arr = np.array([p.size for p in packets])
    max_lag = min(50, len(sizes_arr) // 4)
    
    autocorr = []
    for lag in range(max_lag):
        if lag == 0:
            autocorr.append(1.0)
        else:
            corr = np.corrcoef(sizes_arr[:-lag], sizes_arr[lag:])[0, 1]
            autocorr.append(corr)
    
    ax.bar(range(max_lag), autocorr, color='mediumpurple', alpha=0.7)
    ax.set_xlabel('Lag (packets)')
    ax.set_ylabel('Autocorrelation')
    ax.set_title('Packet Size Autocorrelation')
    ax.axhline(y=0, color='black', linestyle='-', linewidth=0.5)
    
    plt.tight_layout()
    plt.savefig('zigbee_bytes.png', dpi=150, bbox_inches='tight')
    print("Saved: zigbee_bytes.png")
    
    plt.show()


if __name__ == "__main__":
    main()
