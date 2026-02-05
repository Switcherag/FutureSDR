#!/usr/bin/env python3
"""
MAC Address Quality Analysis for Noisy WiFi Sniffer Data
Analyzes decoding quality and correlations in captured MAC addresses.
"""

import re
import numpy as np
import matplotlib.pyplot as plt
from collections import Counter, defaultdict
from pathlib import Path

# Known OUI prefixes (first 3 bytes) - partial list for common vendors
KNOWN_OUIS = {
    'Huawei': ['00:e0:fc', '04:02:1f', '04:4f:aa', '48:00:31', 'e4:c2:d1', '34:29:12'],
    'Realme': ['c0:fb:f9', '3c:84:6a', 'a8:93:4a'],
    'Apple': ['00:03:93', '00:0a:27', '00:0a:95', '00:0d:93', '00:11:24', '00:14:51'],
    'Samsung': ['00:00:f0', '00:02:78', '00:07:ab', '00:09:18', '00:12:47', '00:12:fb'],
    'Xiaomi': ['00:9e:c8', '04:cf:8c', '0c:1d:af', '10:2a:b3', '14:f6:5a'],
    'Intel': ['00:02:b3', '00:03:47', '00:04:23', '00:07:e9', '00:0c:f1', '00:0e:35'],
    'Broadcom': ['00:05:b5', '00:0a:f7', '00:0b:e9', '00:10:18', '00:16:e3'],
}

def parse_mac_file(filepath):
    """Parse MAC addresses from the file, categorizing by type."""
    results = {
        'valid_macs': [],           # Valid MAC format addresses
        'vendor_resolved': [],       # Addresses with vendor name decoded
        'hex_values': [],           # Short hex values (likely corrupted)
        'empty_entries': 0,         # Empty lines or fields
        'partial_decodes': [],      # Partial decodes (vendor name visible)
        'raw_lines': []             # All non-empty lines
    }
    
    mac_pattern = re.compile(r'([0-9a-fA-F]{2}(?::[0-9a-fA-F]{2}){5})')
    hex_pattern = re.compile(r'^0x[0-9a-fA-F]+$')
    vendor_pattern = re.compile(r'([A-Za-z]+[A-Za-z0-9]*_[0-9a-fA-F:]+)')
    
    with open(filepath, 'r') as f:
        for line_num, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                results['empty_entries'] += 1
                continue
            
            results['raw_lines'].append((line_num, line))
            
            # Split by tab to get source and destination
            parts = line.split('\t')
            
            for part in parts:
                part = part.strip()
                if not part:
                    results['empty_entries'] += 1
                    continue
                
                # Check for vendor-resolved names
                if vendor_pattern.match(part):
                    results['vendor_resolved'].append((line_num, part))
                    # Extract MAC portion if present
                    mac_match = mac_pattern.search(part)
                    if mac_match:
                        results['valid_macs'].append((line_num, mac_match.group(1)))
                # Check for short hex values (corrupted)
                elif hex_pattern.match(part):
                    results['hex_values'].append((line_num, part))
                # Check for valid MAC
                else:
                    mac_matches = mac_pattern.findall(part)
                    for mac in mac_matches:
                        results['valid_macs'].append((line_num, mac))
    
    return results

def mac_to_bytes(mac):
    """Convert MAC address string to bytes array."""
    return [int(b, 16) for b in mac.split(':')]

def analyze_byte_autocorrelation(macs):
    """Compute autocorrelation of MAC address bytes."""
    if len(macs) < 10:
        return None, None
    
    # Convert all MACs to byte arrays
    byte_arrays = [mac_to_bytes(mac) for mac in macs]
    all_bytes = np.array(byte_arrays).flatten()
    
    # Normalize
    all_bytes = all_bytes - np.mean(all_bytes)
    
    # Compute autocorrelation
    n = len(all_bytes)
    max_lag = min(50, n // 4)
    autocorr = np.correlate(all_bytes, all_bytes, mode='full')
    autocorr = autocorr[n-1:n-1+max_lag]
    autocorr = autocorr / autocorr[0]  # Normalize
    
    return autocorr, max_lag

def analyze_positional_entropy(macs):
    """Analyze entropy at each byte position in MAC addresses."""
    if not macs:
        return None
    
    byte_arrays = [mac_to_bytes(mac) for mac in macs]
    position_values = [[] for _ in range(6)]
    
    for arr in byte_arrays:
        for i, val in enumerate(arr):
            position_values[i].append(val)
    
    entropies = []
    for pos_vals in position_values:
        counts = Counter(pos_vals)
        total = len(pos_vals)
        entropy = -sum((c/total) * np.log2(c/total) for c in counts.values() if c > 0)
        entropies.append(entropy / 8.0)  # Normalize to 0-1
    
    return entropies

def analyze_bit_patterns(macs):
    """Analyze bit patterns that might indicate noise."""
    if not macs:
        return {}
    
    results = {
        'locally_administered': 0,  # Bit 1 of first byte set (randomized)
        'multicast': 0,             # Bit 0 of first byte set
        'all_same_nibble': 0,       # Suspicious pattern
        'sequential': 0,            # Sequential bytes
        'repeating': 0,             # Repeating patterns
    }
    
    for mac in macs:
        bytes_arr = mac_to_bytes(mac)
        
        # Check LAA bit
        if bytes_arr[0] & 0x02:
            results['locally_administered'] += 1
        
        # Check multicast bit
        if bytes_arr[0] & 0x01:
            results['multicast'] += 1
        
        # Check for suspicious patterns
        nibbles = []
        for b in bytes_arr:
            nibbles.extend([b >> 4, b & 0x0f])
        
        if len(set(nibbles)) <= 2:
            results['all_same_nibble'] += 1
        
        # Check for sequential bytes
        diffs = [bytes_arr[i+1] - bytes_arr[i] for i in range(5)]
        if all(d == diffs[0] for d in diffs):
            results['sequential'] += 1
        
        # Check for repeating patterns
        if bytes_arr[:3] == bytes_arr[3:]:
            results['repeating'] += 1
    
    return results

def compute_inter_mac_correlation(macs):
    """Compute correlation between consecutive MAC addresses."""
    if len(macs) < 2:
        return None
    
    correlations = []
    for i in range(len(macs) - 1):
        bytes1 = mac_to_bytes(macs[i])
        bytes2 = mac_to_bytes(macs[i + 1])
        
        # Compute similarity (number of matching bytes)
        matches = sum(1 for b1, b2 in zip(bytes1, bytes2) if b1 == b2)
        correlations.append(matches / 6.0)
    
    return correlations

def identify_decoding_quality(results):
    """Categorize addresses by likely decoding quality."""
    quality_scores = []
    
    for line_num, mac in results['valid_macs']:
        score = 0
        reasons = []
        
        bytes_arr = mac_to_bytes(mac)
        
        # Check if it matches a known OUI
        oui = mac[:8].lower()
        for vendor, prefixes in KNOWN_OUIS.items():
            if any(oui.startswith(p.lower()[:8]) for p in prefixes):
                score += 30
                reasons.append(f'Known OUI ({vendor})')
                break
        
        # Locally administered = likely randomized (good decode)
        if bytes_arr[0] & 0x02:
            score += 20
            reasons.append('LAA bit set')
        
        # Check entropy of the MAC
        unique_bytes = len(set(bytes_arr))
        if unique_bytes >= 5:
            score += 20
            reasons.append('High byte diversity')
        elif unique_bytes <= 2:
            score -= 20
            reasons.append('Low byte diversity (suspicious)')
        
        # Check for valid hex characters distribution
        mac_clean = mac.replace(':', '')
        char_counts = Counter(mac_clean)
        if len(char_counts) >= 6:
            score += 15
            reasons.append('Good character diversity')
        
        # Check if appears with vendor name
        if any(line_num == ln for ln, _ in results['vendor_resolved']):
            score += 15
            reasons.append('Vendor resolved')
        
        quality_scores.append({
            'line': line_num,
            'mac': mac,
            'score': max(0, min(100, score)),
            'reasons': reasons
        })
    
    return quality_scores

def plot_analysis(results, macs_only, output_dir):
    """Create comprehensive visualization plots."""
    fig = plt.figure(figsize=(16, 14))
    
    # 1. Data quality overview
    ax1 = fig.add_subplot(3, 3, 1)
    categories = ['Valid MACs', 'Vendor Resolved', 'Hex Values\n(Corrupted)', 'Empty']
    counts = [
        len(results['valid_macs']),
        len(results['vendor_resolved']),
        len(results['hex_values']),
        results['empty_entries']
    ]
    colors = ['#2ecc71', '#3498db', '#e74c3c', '#95a5a6']
    bars = ax1.bar(categories, counts, color=colors, edgecolor='black')
    ax1.set_ylabel('Count')
    ax1.set_title('Data Quality Overview')
    for bar, count in zip(bars, counts):
        ax1.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 1, 
                str(count), ha='center', va='bottom', fontsize=10)
    
    # 2. Autocorrelation plot
    ax2 = fig.add_subplot(3, 3, 2)
    autocorr, max_lag = analyze_byte_autocorrelation(macs_only)
    if autocorr is not None:
        lags = np.arange(max_lag)
        ax2.bar(lags, autocorr, color='steelblue', alpha=0.7)
        ax2.axhline(y=0, color='black', linestyle='-', linewidth=0.5)
        ax2.axhline(y=1.96/np.sqrt(len(macs_only)*6), color='red', linestyle='--', label='95% CI')
        ax2.axhline(y=-1.96/np.sqrt(len(macs_only)*6), color='red', linestyle='--')
        ax2.set_xlabel('Lag (bytes)')
        ax2.set_ylabel('Autocorrelation')
        ax2.set_title('Byte Autocorrelation\n(noise shows periodic patterns)')
        ax2.legend()
    else:
        ax2.text(0.5, 0.5, 'Insufficient data', ha='center', va='center', transform=ax2.transAxes)
        ax2.set_title('Byte Autocorrelation')
    
    # 3. Positional entropy
    ax3 = fig.add_subplot(3, 3, 3)
    entropies = analyze_positional_entropy(macs_only)
    if entropies:
        positions = ['Byte 1\n(OUI)', 'Byte 2\n(OUI)', 'Byte 3\n(OUI)', 
                    'Byte 4\n(NIC)', 'Byte 5\n(NIC)', 'Byte 6\n(NIC)']
        colors = ['#e74c3c' if e < 0.7 else '#2ecc71' for e in entropies]
        bars = ax3.bar(positions, entropies, color=colors, edgecolor='black')
        ax3.axhline(y=1.0, color='green', linestyle='--', alpha=0.5, label='Max entropy')
        ax3.axhline(y=0.7, color='orange', linestyle='--', alpha=0.5, label='Good threshold')
        ax3.set_ylabel('Normalized Entropy (0-1)')
        ax3.set_title('Entropy by Byte Position\n(low = predictable/noise)')
        ax3.set_ylim(0, 1.1)
        ax3.legend(loc='lower right')
    
    # 4. Bit pattern analysis
    ax4 = fig.add_subplot(3, 3, 4)
    patterns = analyze_bit_patterns(macs_only)
    if patterns:
        labels = ['Locally\nAdmin', 'Multicast', 'Same\nNibble', 'Sequential', 'Repeating']
        values = [patterns['locally_administered'], patterns['multicast'],
                 patterns['all_same_nibble'], patterns['sequential'], patterns['repeating']]
        total = len(macs_only)
        percentages = [v/total*100 if total > 0 else 0 for v in values]
        colors = ['#3498db', '#9b59b6', '#e74c3c', '#e74c3c', '#e74c3c']
        bars = ax4.bar(labels, percentages, color=colors, edgecolor='black')
        ax4.set_ylabel('Percentage of MACs')
        ax4.set_title('MAC Address Patterns\n(red = suspicious patterns)')
        for bar, pct in zip(bars, percentages):
            ax4.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 0.5, 
                    f'{pct:.1f}%', ha='center', va='bottom', fontsize=9)
    
    # 5. Inter-MAC correlation histogram
    ax5 = fig.add_subplot(3, 3, 5)
    correlations = compute_inter_mac_correlation(macs_only)
    if correlations:
        ax5.hist(correlations, bins=20, color='steelblue', edgecolor='black', alpha=0.7)
        ax5.axvline(x=np.mean(correlations), color='red', linestyle='--', 
                   label=f'Mean: {np.mean(correlations):.3f}')
        ax5.set_xlabel('Similarity (0-1)')
        ax5.set_ylabel('Frequency')
        ax5.set_title('Consecutive MAC Similarity\n(high = possible noise correlation)')
        ax5.legend()
    
    # 6. Byte value distribution heatmap
    ax6 = fig.add_subplot(3, 3, 6)
    if macs_only:
        byte_arrays = np.array([mac_to_bytes(mac) for mac in macs_only])
        byte_heatmap = np.zeros((6, 256))
        for i in range(6):
            counts = Counter(byte_arrays[:, i])
            for val, count in counts.items():
                byte_heatmap[i, val] = count
        
        # Normalize each row
        byte_heatmap = byte_heatmap / (byte_heatmap.sum(axis=1, keepdims=True) + 1e-10)
        
        im = ax6.imshow(byte_heatmap, aspect='auto', cmap='hot', interpolation='nearest')
        ax6.set_xlabel('Byte Value (0-255)')
        ax6.set_ylabel('Byte Position')
        ax6.set_yticks(range(6))
        ax6.set_yticklabels(['B1 (OUI)', 'B2 (OUI)', 'B3 (OUI)', 'B4 (NIC)', 'B5 (NIC)', 'B6 (NIC)'])
        ax6.set_title('Byte Value Distribution\n(uniform = good randomness)')
        plt.colorbar(im, ax=ax6, label='Frequency')
    
    # 7. Quality score distribution
    ax7 = fig.add_subplot(3, 3, 7)
    quality_scores = identify_decoding_quality(results)
    if quality_scores:
        scores = [q['score'] for q in quality_scores]
        ax7.hist(scores, bins=20, color='steelblue', edgecolor='black', alpha=0.7)
        ax7.axvline(x=np.mean(scores), color='red', linestyle='--', 
                   label=f'Mean: {np.mean(scores):.1f}')
        ax7.axvline(x=50, color='orange', linestyle='--', alpha=0.7, label='Quality threshold')
        ax7.set_xlabel('Decoding Quality Score (0-100)')
        ax7.set_ylabel('Frequency')
        ax7.set_title('MAC Decoding Quality Distribution')
        ax7.legend()
    
    # 8. Hex corruption timeline
    ax8 = fig.add_subplot(3, 3, 8)
    if results['hex_values'] or results['valid_macs']:
        # Create timeline of good vs corrupted
        all_entries = []
        for ln, _ in results['valid_macs']:
            all_entries.append((ln, 'valid'))
        for ln, _ in results['hex_values']:
            all_entries.append((ln, 'corrupted'))
        
        all_entries.sort(key=lambda x: x[0])
        
        # Rolling corruption rate
        window = 20
        corruption_rate = []
        positions = []
        for i in range(0, len(all_entries) - window + 1, 5):
            window_data = all_entries[i:i+window]
            corrupted = sum(1 for _, t in window_data if t == 'corrupted')
            corruption_rate.append(corrupted / window * 100)
            positions.append(window_data[window//2][0])
        
        if corruption_rate:
            ax8.plot(positions, corruption_rate, 'r-', linewidth=2)
            ax8.fill_between(positions, corruption_rate, alpha=0.3, color='red')
            ax8.set_xlabel('Line Number')
            ax8.set_ylabel('Corruption Rate (%)')
            ax8.set_title('Corruption Rate Over Time\n(20-entry rolling window)')
            ax8.set_ylim(0, 100)
    
    # 9. OUI distribution pie chart
    ax9 = fig.add_subplot(3, 3, 9)
    oui_counts = Counter()
    for _, mac in results['valid_macs']:
        oui = mac[:8].upper()
        oui_counts[oui] += 1
    
    # Get top 10 OUIs
    top_ouis = oui_counts.most_common(10)
    if top_ouis:
        labels = [oui for oui, _ in top_ouis]
        sizes = [count for _, count in top_ouis]
        other = sum(oui_counts.values()) - sum(sizes)
        if other > 0:
            labels.append('Other')
            sizes.append(other)
        
        ax9.pie(sizes, labels=labels, autopct='%1.1f%%', startangle=90)
        ax9.set_title('OUI (Vendor Prefix) Distribution\n(top 10)')
    
    plt.tight_layout()
    plt.savefig(output_dir / 'mac_quality_analysis.png', dpi=150, bbox_inches='tight')
    plt.show()
    print(f"\nPlot saved to: {output_dir / 'mac_quality_analysis.png'}")

def print_summary(results, macs_only):
    """Print detailed summary of the analysis."""
    print("=" * 70)
    print("MAC ADDRESS QUALITY ANALYSIS - NOISY WiFi SNIFFER DATA")
    print("=" * 70)
    
    total_entries = len(results['valid_macs']) + len(results['hex_values']) + results['empty_entries']
    
    print(f"\n--- Data Overview ---")
    print(f"Total valid MAC addresses: {len(results['valid_macs'])}")
    print(f"Vendor-resolved addresses: {len(results['vendor_resolved'])}")
    print(f"Corrupted (hex values): {len(results['hex_values'])}")
    print(f"Empty entries: {results['empty_entries']}")
    
    if total_entries > 0:
        corruption_rate = len(results['hex_values']) / total_entries * 100
        print(f"\nOverall corruption rate: {corruption_rate:.1f}%")
    
    print(f"\n--- Vendor Resolved Examples (Good Decodes) ---")
    for i, (ln, entry) in enumerate(results['vendor_resolved'][:5]):
        print(f"  Line {ln}: {entry}")
    
    print(f"\n--- Corrupted Examples (Hex Values) ---")
    for i, (ln, entry) in enumerate(results['hex_values'][:5]):
        print(f"  Line {ln}: {entry}")
    
    # Bit pattern analysis
    patterns = analyze_bit_patterns(macs_only)
    if patterns:
        print(f"\n--- Bit Pattern Analysis ---")
        total = len(macs_only)
        print(f"Locally Administered (randomized): {patterns['locally_administered']} ({patterns['locally_administered']/total*100:.1f}%)")
        print(f"Multicast addresses: {patterns['multicast']} ({patterns['multicast']/total*100:.1f}%)")
        print(f"Suspicious (same nibble): {patterns['all_same_nibble']}")
        print(f"Suspicious (sequential): {patterns['sequential']}")
        print(f"Suspicious (repeating): {patterns['repeating']}")
    
    # Quality scores
    quality_scores = identify_decoding_quality(results)
    if quality_scores:
        scores = [q['score'] for q in quality_scores]
        print(f"\n--- Decoding Quality Assessment ---")
        print(f"Average quality score: {np.mean(scores):.1f}/100")
        print(f"High quality (>70): {sum(1 for s in scores if s > 70)} ({sum(1 for s in scores if s > 70)/len(scores)*100:.1f}%)")
        print(f"Medium quality (40-70): {sum(1 for s in scores if 40 <= s <= 70)} ({sum(1 for s in scores if 40 <= s <= 70)/len(scores)*100:.1f}%)")
        print(f"Low quality (<40): {sum(1 for s in scores if s < 40)} ({sum(1 for s in scores if s < 40)/len(scores)*100:.1f}%)")
        
        print(f"\n--- Best Quality Decodes (Top 5) ---")
        sorted_quality = sorted(quality_scores, key=lambda x: x['score'], reverse=True)[:5]
        for q in sorted_quality:
            print(f"  Score {q['score']}: {q['mac']} - {', '.join(q['reasons'])}")
        
        print(f"\n--- Lowest Quality Decodes (Bottom 5) ---")
        sorted_quality = sorted(quality_scores, key=lambda x: x['score'])[:5]
        for q in sorted_quality:
            print(f"  Score {q['score']}: {q['mac']} - {', '.join(q['reasons']) if q['reasons'] else 'No positive indicators'}")

def main():
    script_dir = Path(__file__).parent
    input_file = script_dir / 'adress.txt'
    
    if not input_file.exists():
        print(f"Error: {input_file} not found!")
        return
    
    print(f"Reading from: {input_file}")
    
    results = parse_mac_file(input_file)
    macs_only = [mac for _, mac in results['valid_macs']]
    
    if not macs_only:
        print("No valid MAC addresses found!")
        return
    
    print_summary(results, macs_only)
    
    print("\n" + "=" * 70)
    print("Generating visualizations...")
    print("=" * 70)
    
    plot_analysis(results, macs_only, script_dir)

if __name__ == '__main__':
    main()
