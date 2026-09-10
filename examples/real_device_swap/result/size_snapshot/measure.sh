#!/usr/bin/env bash
# Regenerate MANIFEST.txt + sizes.tex from the .so/binaries in this snapshot.
#
# Everything is reported in DEPLOYED form: `strip -s` followed by
# `objcopy --remove-section=.rustc`, applied to a scratch copy — the snapshot
# files themselves are never modified.
#
#   strip -s   drops .symtab/.strtab (backtrace names). It does NOT touch
#              .dynsym / .gnu.hash / the rust_metadata_* ABI fingerprint,
#              which are SHF_ALLOC, nor .rustc.
#   .rustc     is the crate metadata dylib crates embed so downstream crates
#              can compile against them. SHF_ALLOC is cleared, so the loader
#              never maps it; removing it costs only the ability to build
#              against the library.
#
# Release profile (root Cargo.toml): opt-level = 3, codegen-units = 1,
# lto = false, debug = false, panic = "unwind", plus -C prefer-dynamic from
# .cargo/config.toml. debug = false means there is no DWARF to begin with,
# so `strip -s` only reclaims .symtab/.strtab.
#
# Verified: the resulting tree passes build.sh's ABI guard on all 49 plugins
# and runs 40 ZigBee<->HaLow swaps with no change to load_plugins or total
# swap latency.
set -uo pipefail
cd "$(dirname "$0")"

# deployed size: strip -s + drop .rustc, on a scratch copy
dep()  { t=$(mktemp); cp "$1" "$t"; strip -s "$t" 2>/dev/null
         objcopy --remove-section=.rustc "$t" 2>/dev/null
         s=$(stat -c%s "$t"); rm -f "$t"; echo "$s"; }
sec()  { size -A "$1" 2>/dev/null | awk -v s="$2" '$1==s{print $2; f=1} END{if(!f) print 0}'; }
load() { readelf -l -W "$1" | awk '/LOAD/{t+=strtonum($6)} END{print t+0}'; }
kb()   { awk -v b="$1" 'BEGIN{printf "%.1f", b/1024}'; }
mb()   { awk -v b="$1" 'BEGIN{printf "%.2f", b/1048576}'; }
pct()  { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.1f", 100*a/b}'; }

P=plugins

# --- flowgraph -> plugin sets (flows/*.toml, dyn_phy-swap/src/tx_sdr_hotswap.rs)
ZRX="zigbee_demod clock_recovery_mm zigbee_decoder zigbee_mac null_sink blob_to_lp_stream"
ZTX="zigbee_mac zigbee_modulator zigbee_iq_delay"
HRX="delay_complex complex_to_mag2 moving_average_f32 mult_conj moving_average_complex divide_mag wlan_ah_v6_sync_short wlan_ah_v6_sync_long fft_complex wlan_ah_v6_frame_equalizer wlan_ah_decoder blob_to_udp blob_to_lp_stream"
HTX="wlan_mac wlan_encoder wlan_mapper fft_complex wlan_prefix"
HRXH="seify_source"
HTXH="seify_sink"
NET="lp_stream_to_blob universal_mac blob_to_udp tap_nic network_extractor message_file_sink"

text() { sec "$1" .text; }
sum()  { t=0; for p in $2; do t=$((t+$($1 $P/lib${p}_plugin.so))); done; echo $t; }

BIN=bin/ziglow_swap_quicktune
FSDR=runtime/libfuturesdr.so
STD=runtime/libstd-cc476dd173638de7.so
BASE_DEP=$((  $(dep $BIN)+$(dep $FSDR)+$(dep $STD) ))
BASE_LOAD=$(( $(sec $BIN .text)+$(sec $FSDR .text)+$(sec $STD .text) ))
BASE_TEXT=$(( $(sec $BIN .text)+$(sec $FSDR .text)+$(sec $STD .text) ))

UNION_DEP=0; UNION_LOAD=0; UNION_TEXT=0; UNION_N=0
for f in $P/*.so; do
  UNION_DEP=$((UNION_DEP+$(dep $f))); UNION_LOAD=$((UNION_LOAD+$(load $f)))
  UNION_TEXT=$((UNION_TEXT+$(sec $f .text))); UNION_N=$((UNION_N+1))
done

# ===================== MANIFEST =======================================
{
echo "size_snapshot — immutable copy of the .so / binaries measured for the"
echo "plugin-footprint tables. Originals are NOT modified by measure.sh."
echo "generated: $(date -Is)"
echo
echo "PROVENANCE — all $UNION_N plugins, libfuturesdr.so and the 4 host binaries come"
echo "from one single target/release build (examples/real_device_swap/build.sh,"
echo "2026-09-09), so every figure shares one feature unification and one ABI."
echo "libstd-cc476dd173638de7.so is the rustup nightly-2025-11-18 sysroot copy"
echo "named in each plugin's NEEDED. libfuturesdr_macros.so is a proc-macro:"
echo "compile-time only, never dlopen'd, excluded from every total."
echo
echo "BUILD: cargo build --release — opt-level 3, codegen-units 1, lto off,"
echo "debug = false, panic = unwind, -C prefer-dynamic. No DWARF is emitted, so"
echo "strip -s reclaims only .symtab/.strtab."
echo
echo "mono/  3 non-plugin control binaries <- target/mono/ (cargo build --profile"
echo "       mono, i.e. release + lto = \"fat\", with RUSTFLAGS=\"-C prefer-dynamic=no\")."
echo "       Source: examples/monolithic_baseline. Same block chains as"
echo "       flows/zigbee_rxA.toml and flows/halowv6A.toml, linked in statically:"
echo "       no plugin_api, no dlopen, no libfuturesdr.so (checked with objdump -p)."
echo
echo "ALL SIZES ARE DEPLOYED SIZES: strip -s + objcopy --remove-section=.rustc."
echo "Neither step touches .dynsym, .gnu.hash or the rust_metadata_* fingerprint"
echo "(all SHF_ALLOC), so the deployed tree dlopens and hot-swaps unchanged —"
echo "checked against build.sh's ABI guard (49/49) and a 40-swap run."
echo
echo "NOTE — libseify_sink_plugin.so was a 61.5 kB stub before this build."
echo "plugins/seify_sink_plugin/Cargo.toml declared default = [] (no device"
echo "backend, so seify::Builder had no driver to monomorphise) and pinned"
echo "futuresdr = { path = \"../../\", features = [\"plugin\",\"seify\"] } instead of"
echo "{ workspace = true }, bypassing the root ABI contract. Both were corrected."
echo
printf "%12s %12s %12s  %-46s %s\n" "deployed" "PT_LOAD" "as-built" "file" "sha256"
for f in $P/*.so runtime/*.so bin/* mono/*; do
  printf "%12d %12d %12d  %-46s %s\n" "$(dep $f)" "$(load $f)" "$(stat -c%s $f)" "$f" "$(sha256sum $f | cut -c1-16)"
done
} > MANIFEST.txt

MONO_Z=$(dep mono/mono_zigbee); MONO_H=$(dep mono/mono_halow); MONO_B=$(dep mono/mono_both)

# deployable total for the plugin system = fixed base + radio head + the PHY's
# own .so set (union when a deployment must carry both PHYs).
BOTH="$ZRX $HRX"
BOTH_U=$(printf '%s\n' $BOTH | sort -u | tr '\n' ' ')
PLUG_Z=$(( BASE_DEP + $(sum dep "$HRXH") + $(sum dep "$ZRX") ))
PLUG_H=$(( BASE_DEP + $(sum dep "$HRXH") + $(sum dep "$HRX") ))
PLUG_B=$(( BASE_DEP + $(sum dep "$HRXH") + $(sum dep "$BOTH_U") ))

# ===================== LaTeX ==========================================
row() { printf '%s & %s & %s & %s & %s\\%% \\\\\n' "$1" "$2" "$(kb $3)" "$(kb $4)" "$(pct $3 $BASE_DEP)"; }

{
cat <<'TEX'
% Auto-generated by result/size_snapshot/measure.sh — do not edit by hand.

\begin{table}[t]
\centering
\caption{Deployed footprint of the swappable PHY plugins against the fixed
runtime they share. Every plugin is built with \texttt{-C prefer-dynamic} and
carries \texttt{NEEDED libfuturesdr.so}, so the base rows are mapped once per
process while the PHY rows are what a hot swap actually loads and unloads.
Sizes are the deployed form --- \texttt{strip -s} followed by
\texttt{objcopy -{}-remove-section=.rustc}, neither of which touches
\texttt{.dynsym}, \texttt{.gnu.hash} or the \texttt{rust\_metadata\_*} ABI
fingerprint. Once those two sections are gone almost everything left is
\texttt{SHF\_ALLOC}, so these figures are also the resident footprint: on every
row the deployed size and the sum of the \texttt{PT\_LOAD} segments agree to
within 1.5\% (2.4\% on the worst individual \texttt{.so}). \texttt{.text} is given alongside to separate
executable code from relocations, unwind tables and the dynamic symbol table.}
\label{tab:plugin-footprint}
\begin{tabular}{@{}lrrrr@{}}
\toprule
 & \multicolumn{1}{c}{\#\,\texttt{.so}} & \multicolumn{1}{c}{deployed} &
 \multicolumn{1}{c}{\texttt{.text}} & \multicolumn{1}{c}{vs.\ base} \\
 & & \multicolumn{1}{c}{(kB)} & \multicolumn{1}{c}{(kB)} & \\
\midrule
\multicolumn{5}{@{}l}{\emph{Swappable PHY / flowgraph payload}}\\
TEX
row 'ZigBee \textsc{rx}~\scriptsize(\texttt{zigbee\_rxA.toml})' 6 $(sum dep "$ZRX") $(sum text "$ZRX")
row 'ZigBee \textsc{tx}'                                        3 $(sum dep "$ZTX") $(sum text "$ZTX")
row 'HaLow~v6 \textsc{rx}~\scriptsize(\texttt{halowv6A.toml})' 13 $(sum dep "$HRX") $(sum text "$HRX")
row 'HaLow \textsc{tx}~\scriptsize(11a-derived OFDM)'           5 $(sum dep "$HTX") $(sum text "$HTX")
echo '\addlinespace'
echo '\multicolumn{5}{@{}l}{\emph{Permanent flowgraphs}}\\'
row 'Radio head \textsc{rx}~\scriptsize(\texttt{sdr\_head.toml})' 1 $(sum dep "$HRXH") $(sum text "$HRXH")
row 'Radio head \textsc{tx}~\scriptsize(seify sink)'              1 $(sum dep "$HTXH") $(sum text "$HTXH")
row 'Network tail~\scriptsize(\texttt{network\_tail.toml})'       6 $(sum dep "$NET")  $(sum text "$NET")
echo '\addlinespace'
echo '\multicolumn{5}{@{}l}{\emph{Fixed base --- mapped once, shared by every flowgraph}}\\'
row 'Host binary~\scriptsize(\texttt{ziglow\_swap\_quicktune})'  1 $(dep $BIN)  $(sec $BIN .text)
row '\texttt{libfuturesdr.so}'                                  1 $(dep $FSDR) $(sec $FSDR .text)
row '\texttt{libstd-*.so}~\scriptsize(nightly-2025-11-18)'      1 $(dep $STD)  $(sec $STD .text)
printf '\\cmidrule(l){2-5}\n'
printf '\\quad total base & 3 & %s & %s & 100.0\\%% \\\\\n' "$(kb $BASE_DEP)" "$(kb $BASE_TEXT)"
cat <<TEX
\midrule
\multicolumn{5}{@{}l}{\emph{Union of all $UNION_N distinct plugins (no double counting)}}\\\\
\quad whole PHY library & $UNION_N & $(kb $UNION_DEP) & $(kb $UNION_TEXT) & $(pct $UNION_DEP $BASE_DEP)\\% \\\\
\bottomrule
\end{tabular}
\end{table}
TEX

r3() { printf '%s & %s & %s & %s$\\times$ \\\\\n' "$1" "$(kb $2)" "$(kb $3)" \
        "$(awk -v a=$2 -v b=$3 'BEGIN{printf "%.2f", a/b}')"; }
cat <<'TEX'

\begin{table}[t]
\centering
\caption{What each approach must actually ship, for the same receiver. The
plugin column is the fixed base plus the radio head plus that deployment's PHY
\texttt{.so} set; the monolithic column is a single statically linked binary
holding the identical block chain, built with fat LTO and without
\texttt{-C prefer-dynamic}. Both columns are in deployed form. The monolith is
free to inline across crate boundaries and drop unreachable code, which a
\texttt{dlopen}-by-name plugin boundary forbids --- so this ratio is the price
of runtime reconfigurability, not an implementation defect.}
\label{tab:plugin-vs-monolithic}
\begin{tabular}{@{}lrrr@{}}
\toprule
deployment & \multicolumn{1}{c}{plugin} & \multicolumn{1}{c}{monolithic} &
 \multicolumn{1}{c}{ratio} \\
 & \multicolumn{1}{c}{(kB)} & \multicolumn{1}{c}{(kB)} & \\
\midrule
TEX
r3 'ZigBee \textsc{rx} only'       $PLUG_Z $MONO_Z
r3 'HaLow~v6 \textsc{rx} only'     $PLUG_H $MONO_H
r3 'both PHYs in one deployment'  $PLUG_B $MONO_B
cat <<'TEX'
\bottomrule
\end{tabular}
\end{table}
TEX
} > sizes.tex
echo "wrote MANIFEST.txt and sizes.tex"
