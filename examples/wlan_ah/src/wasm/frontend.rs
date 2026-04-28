use futuresdr::futures::StreamExt;
use futuresdr::runtime::FlowgraphId;
use futuresdr::runtime::Pmt;
use gloo_net::websocket::Message;
use gloo_net::websocket::futures::WebSocket;
use leptos::html::Span;
use leptos::logging::*;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use leptos::web_sys::HtmlInputElement;
use prophecy::ConstellationSinkDensity;
use prophecy::IqScatterDual;
use prophecy::FlowgraphHandle;
use prophecy::FlowgraphMermaid;
use prophecy::ListSelector;
use prophecy::RadioSelector;
use prophecy::RuntimeHandle;
use prophecy::SlidingTimeSink;
use prophecy::SlidingTimeSinkDots;
use prophecy::TimeSink;
use prophecy::TimeSinkMode;
use prophecy::Waterfall;
use prophecy::WaterfallMode;

#[component]
pub fn Wlan(fg_handle: FlowgraphHandle) -> impl IntoView {
    let fg_desc = {
        let fg_handle = fg_handle.clone();
        LocalResource::new(move || {
            let mut fg_handle = fg_handle.clone();
            async move {
                if let Ok(desc) = fg_handle.description().await {
                    return Some(desc);
                }
                None
            }
        })
    };

    let (width, set_width) = signal(2.0f32);
    let (min, set_min) = signal(-40.0f32);
    let (max, set_max) = signal(20.0f32);
    let min_label = NodeRef::<Span>::new();
    let max_label = NodeRef::<Span>::new();
    let (cor_min, _set_cor_min) = signal(0.0f32);
    // Correlation window max is user-controlled in linear scale.
    // The sink is configured in logarithmic mode, so we convert this to dB.
    let (cor_max_lin, set_cor_max_lin) = signal(100_000.0f32);
    let (cor_peak, set_cor_peak) = signal(0.0f32);
    let cor_max_label = NodeRef::<Span>::new();
    let (iq_min, _set_iq_min) = signal(0.0f32);
    let (iq_max, _set_iq_max) = signal(2.0f32);

    // Manual WebSocket for spectrogram — feeds both waterfall and power strip
    let (waterfall_data, set_waterfall_data) = signal(vec![]);
    let (power_data, set_power_data) = signal(vec![]);
    {
        spawn_local(async move {
            let mut ws = WebSocket::open("ws://127.0.0.1:9013").unwrap();
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Bytes(b)) => {
                        set_waterfall_data(b.clone());
                        set_power_data(b);
                    }
                    _ => {
                        log!("Spectrogram WebSocket {:?}", msg);
                    }
                }
            }
            log!("Spectrogram: WebSocket Closed");
        });
    }

    // Manual WebSocket for correlation time sink (port 9014)
    let (cor_data, set_cor_data) = signal(vec![]);
    let (real_time, set_real_time) = signal(0.0f64);
    {
        spawn_local(async move {
            let mut total_samples: u64 = 0;
            let sample_rate: f64 = 2_000_000.0;
            let mut ws = WebSocket::open("ws://127.0.0.1:9014").unwrap();
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Bytes(b)) => {
                        let n_samples = b.len() / 4;
                        total_samples += n_samples as u64;
                        set_real_time(total_samples as f64 / sample_rate);
                        // Track peak correlation value for readout.
                        let floats = unsafe {
                            std::slice::from_raw_parts(b.as_ptr() as *const f32, n_samples)
                        };
                        let batch_max = floats.iter().copied().fold(0.0f32, f32::max);
                        let prev_peak = cor_peak.get_untracked();
                        if batch_max > prev_peak {
                            set_cor_peak(batch_max);
                        }
                        set_cor_data(b);
                    }
                    _ => {
                        log!("Correlation WebSocket {:?}", msg);
                    }
                }
            }
            log!("Correlation: WebSocket Closed");
        });
    }

    // Manual WebSocket for IQ amplitude (port 9015)
    let (iq_data, set_iq_data) = signal(vec![]);
    {
        spawn_local(async move {
            let mut ws = WebSocket::open("ws://127.0.0.1:9015").unwrap();
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Bytes(b)) => {
                        set_iq_data(b);
                    }
                    _ => {
                        log!("IQ amplitude WebSocket {:?}", msg);
                    }
                }
            }
            log!("IQ amplitude: WebSocket Closed");
        });
    }

    // Manual WebSocket for channel estimate (port 9016, PMT VecCF32)
    // Raw bytes = interleaved f32 pairs [re, im, re, im, ...]
    // Convert to amplitude + phase f32 arrays for display
    let (h_est_amp, set_h_est_amp) = signal(vec![]);
    let (h_est_phase, set_h_est_phase) = signal(vec![]);
    {
        spawn_local(async move {
            let mut ws = WebSocket::open("ws://127.0.0.1:9016").unwrap();
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Bytes(b)) => {
                        let n_complex = b.len() / 8;
                        let floats = unsafe {
                            std::slice::from_raw_parts(b.as_ptr() as *const f32, n_complex * 2)
                        };
                        // Convert complex → amplitude (as f32 bytes for TimeSink)
                        let amp: Vec<f32> = (0..n_complex)
                            .map(|i| (floats[2*i]*floats[2*i] + floats[2*i+1]*floats[2*i+1]).sqrt())
                            .collect();
                        let amp_bytes: Vec<u8> = amp.iter()
                            .flat_map(|f| f.to_le_bytes())
                            .collect();
                        // Convert complex → phase in degrees (as f32 bytes for TimeSink)
                        let phase: Vec<f32> = (0..n_complex)
                            .map(|i| floats[2*i+1].atan2(floats[2*i]).to_degrees())
                            .collect();
                        let phase_bytes: Vec<u8> = phase.iter()
                            .flat_map(|f| f.to_le_bytes())
                            .collect();
                        set_h_est_amp(amp_bytes);
                        set_h_est_phase(phase_bytes);
                    }
                    _ => {
                        log!("Channel est WebSocket {:?}", msg);
                    }
                }
            }
            log!("Channel est: WebSocket Closed");
        });
    }

    // ── SyncLong correlation landscape (port 9019, VecF32, 160 pts) ──
    let (sync_corr_data, set_sync_corr_data) = signal(vec![]);
    {
        spawn_local(async move {
            let mut ws = WebSocket::open("ws://127.0.0.1:9019").unwrap();
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Bytes(b)) => set_sync_corr_data(b),
                    _ => log!("SyncCorr WS {:?}", msg),
                }
            }
        });
    }

    // ── SyncLong peak info (port 9020, VecF32[6]) ────────────────────
    let (sync_peak1, set_sync_peak1) = signal(0u32);
    let (sync_peak2, set_sync_peak2) = signal(0u32);
    let (sync_gap, set_sync_gap) = signal(0i32);
    let (sync_cfo, set_sync_cfo) = signal(0.0f32);
    {
        spawn_local(async move {
            let mut ws = WebSocket::open("ws://127.0.0.1:9020").unwrap();
            while let Some(msg) = ws.next().await {
                if let Ok(Message::Bytes(b)) = msg {
                    if b.len() >= 24 {
                        let f = unsafe { std::slice::from_raw_parts(b.as_ptr() as *const f32, 6) };
                        set_sync_peak1(f[0] as u32);
                        set_sync_peak2(f[1] as u32);
                        set_sync_gap(f[2] as i32);
                        set_sync_cfo(f[3]);
                    }
                }
            }
        });
    }

    // ── Time-domain LTF samples (port 9021, VecCF32) → magnitudes ────
    let (ltf_td_mag, set_ltf_td_mag) = signal(vec![]);
    {
        spawn_local(async move {
            let mut ws = WebSocket::open("ws://127.0.0.1:9021").unwrap();
            while let Some(msg) = ws.next().await {
                if let Ok(Message::Bytes(b)) = msg {
                    let n_complex = b.len() / 8;
                    let floats = unsafe {
                        std::slice::from_raw_parts(b.as_ptr() as *const f32, n_complex * 2)
                    };
                    let mag: Vec<f32> = (0..n_complex)
                        .map(|i| (floats[2*i].powi(2) + floats[2*i+1].powi(2)).sqrt())
                        .collect();
                    let bytes: Vec<u8> = mag.iter().flat_map(|f| f.to_le_bytes()).collect();
                    set_ltf_td_mag(bytes);
                }
            }
        });
    }

    let width_label = NodeRef::<Span>::new();
    let gain_label = NodeRef::<Span>::new();

    view! {
        // ── Controls (full width) ──────────────────────────────────────
        <div class="border-2 border-slate-500 rounded-md flex flex-row flex-wrap m-2 p-2">
            <div class="basis-1/4">
                <input type="range" min="0" max="10" value="2" class="align-middle"
                    on:change= move |v| {
                        let target = v.target().unwrap();
                        let input : HtmlInputElement = target.dyn_into().unwrap();
                        width_label.get().unwrap().set_inner_text(&format!("width: {}", input.value()));
                        set_width(input.value().parse().unwrap());
                    } />
                <span class="text-white p-1 m-1 text-sm" node_ref=width_label>"width: 2"</span>
            </div>
            <div class="basis-1/4 text-white text-sm">
                <RadioSelector fg_handle=fg_handle.clone() block_id=0 handler="sample_rate" values=[
                    ("5 MHz".to_string(), Pmt::F64(5e6)),
                    ("10 MHz".to_string(), Pmt::F64(10e6)),
                    ("20 MHz".to_string(), Pmt::F64(20e6)),
                ] label_class="p-1" />
            </div>
            <div class="basis-1/4">
                <span class="text-white m-1 text-sm">WLAN Channel</span>
                <ListSelector fg_handle=fg_handle.clone() block_id=0 handler="freq" values=[
                    // 11g
                    ("1".to_string(),	Pmt::F64(2412e6)),
                    ("2".to_string(),	Pmt::F64(2417e6)),
                    ("3".to_string(),	Pmt::F64(2422e6)),
                    ("4".to_string(),	Pmt::F64(2427e6)),
                    ("5".to_string(),	Pmt::F64(2432e6)),
                    ("6".to_string(),	Pmt::F64(2437e6)),
                    ("7".to_string(),	Pmt::F64(2442e6)),
                    ("8".to_string(),	Pmt::F64(2447e6)),
                    ("9".to_string(),	Pmt::F64(2452e6)),
                    ("10".to_string(),	Pmt::F64(2457e6)),
                    ("11".to_string(),	Pmt::F64(2462e6)),
                    ("12".to_string(),	Pmt::F64(2467e6)),
                    ("13".to_string(),	Pmt::F64(2472e6)),
                    ("14".to_string(),	Pmt::F64(2484e6)),
                    // 11a
                    ("34".to_string(),	Pmt::F64(5170e6)),
                    ("36".to_string(),	Pmt::F64(5180e6)),
                    ("38".to_string(),	Pmt::F64(5190e6)),
                    ("40".to_string(),	Pmt::F64(5200e6)),
                    ("42".to_string(),	Pmt::F64(5210e6)),
                    ("44".to_string(),	Pmt::F64(5220e6)),
                    ("46".to_string(),	Pmt::F64(5230e6)),
                    ("48".to_string(),	Pmt::F64(5240e6)),
                    ("50".to_string(),	Pmt::F64(5250e6)),
                    ("52".to_string(),	Pmt::F64(5260e6)),
                    ("54".to_string(),	Pmt::F64(5270e6)),
                    ("56".to_string(),	Pmt::F64(5280e6)),
                    ("58".to_string(),	Pmt::F64(5290e6)),
                    ("60".to_string(),	Pmt::F64(5300e6)),
                    ("62".to_string(),	Pmt::F64(5310e6)),
                    ("64".to_string(),	Pmt::F64(5320e6)),
                    ("100".to_string(),	Pmt::F64(5500e6)),
                    ("102".to_string(),	Pmt::F64(5510e6)),
                    ("104".to_string(),	Pmt::F64(5520e6)),
                    ("106".to_string(),	Pmt::F64(5530e6)),
                    ("108".to_string(),	Pmt::F64(5540e6)),
                    ("110".to_string(),	Pmt::F64(5550e6)),
                    ("112".to_string(),	Pmt::F64(5560e6)),
                    ("114".to_string(),	Pmt::F64(5570e6)),
                    ("116".to_string(),	Pmt::F64(5580e6)),
                    ("118".to_string(),	Pmt::F64(5590e6)),
                    ("120".to_string(),	Pmt::F64(5600e6)),
                    ("122".to_string(),	Pmt::F64(5610e6)),
                    ("124".to_string(),	Pmt::F64(5620e6)),
                    ("126".to_string(),	Pmt::F64(5630e6)),
                    ("128".to_string(),	Pmt::F64(5640e6)),
                    ("132".to_string(),	Pmt::F64(5660e6)),
                    ("134".to_string(),	Pmt::F64(5670e6)),
                    ("136".to_string(),	Pmt::F64(5680e6)),
                    ("138".to_string(),	Pmt::F64(5690e6)),
                    ("140".to_string(),	Pmt::F64(5700e6)),
                    ("142".to_string(),	Pmt::F64(5710e6)),
                    ("144".to_string(),	Pmt::F64(5720e6)),
                    ("149".to_string(),	Pmt::F64(5745e6)),
                    ("151".to_string(),	Pmt::F64(5755e6)),
                    ("153".to_string(),	Pmt::F64(5765e6)),
                    ("155".to_string(),	Pmt::F64(5775e6)),
                    ("157".to_string(),	Pmt::F64(5785e6)),
                    ("159".to_string(),	Pmt::F64(5795e6)),
                    ("161".to_string(),	Pmt::F64(5805e6)),
                    ("165".to_string(),	Pmt::F64(5825e6)),
                    //11p
                    ("172".to_string(),	Pmt::F64(5860e6)),
                    ("174".to_string(),	Pmt::F64(5870e6)),
                    ("176".to_string(),	Pmt::F64(5880e6)),
                    ("178".to_string(),	Pmt::F64(5890e6)),
                    ("180".to_string(),	Pmt::F64(5900e6)),
                    ("182".to_string(),	Pmt::F64(5910e6)),
                    ("184".to_string(),	Pmt::F64(5920e6)),
                ] />
            </div>
            <div class="basis-1/4">
                <input type="range" min="0" max="80" value="40" class="align-middle"
                    on:change= {
                        let fg_handle = fg_handle.clone();
                        move |v| {
                            let target = v.target().unwrap();
                            let input : HtmlInputElement = target.dyn_into().unwrap();
                            gain_label.get().unwrap().set_inner_text(&format!("gain: {}", input.value()));
                            let p = Pmt::U32(input.value().parse().unwrap());
                            let mut fg_handle = fg_handle.clone();
                            spawn_local(async move {
                                let _ = fg_handle.call(0, "gain", p).await;
                            });
                }} />
                <span class="text-white p-1 m-1 text-sm" node_ref=gain_label>"gain: 40"</span>
            </div>
        </div>

        // ── Two-column grid (flowgraph order) ──────────────────────────
        <div class="grid grid-cols-2 gap-2 m-2">

            // ── Left col: ⑥ Phase 2× with ±90°/±180° markers ──────────
            <div>
                <h2 class="text-sm text-white mb-1">"⑥ Channel Estimate — Phase (deg)"</h2>
                <div class="border-2 rounded-md border-slate-500 p-1"
                     style="position: relative; height: 340px">
                    <div style="position: absolute; inset: 4px">
                        <TimeSink min=Signal::derive(|| -180.0f32) max=Signal::derive(|| 180.0f32) linear=true
                            mode=TimeSinkMode::Data(h_est_phase) />
                    </div>
                    // Horizontal markers at ±180° (edges), ±90° (25%/75%)
                    <div style="position: absolute; inset: 4px; pointer-events: none;">
                        <div style="position:absolute; left:0; right:0; top:0%;   border-top:1px dashed #fca5a5;"></div>
                        <div style="position:absolute; left:0; right:0; top:25%;  border-top:1px dashed #93c5fd;"></div>
                        <div style="position:absolute; left:0; right:0; top:50%;  border-top:1px solid  #64748b;"></div>
                        <div style="position:absolute; left:0; right:0; top:75%;  border-top:1px dashed #93c5fd;"></div>
                        <div style="position:absolute; left:0; right:0; bottom:0%;border-bottom:1px dashed #fca5a5;"></div>
                        <span style="position:absolute; right:4px; top:0%;   transform:translateY(-50%); font-size:10px; color:#fca5a5;">"+180°"</span>
                        <span style="position:absolute; right:4px; top:25%;  transform:translateY(-50%); font-size:10px; color:#93c5fd;">"+90°"</span>
                        <span style="position:absolute; right:4px; top:50%;  transform:translateY(-50%); font-size:10px; color:#94a3b8;">"0°"</span>
                        <span style="position:absolute; right:4px; top:75%;  transform:translateY(-50%); font-size:10px; color:#93c5fd;">"-90°"</span>
                        <span style="position:absolute; right:4px; bottom:0%;transform:translateY(50%);  font-size:10px; color:#fca5a5;">"-180°"</span>
                    </div>
                </div>

                // ⑥ Amplitude, now under Phase in the left col
                <h2 class="text-sm text-white mt-2 mb-1">"⑥ Channel Estimate — Amplitude"</h2>
                <div class="border-2 rounded-md border-slate-500 p-1"
                     style="position: relative; height: 160px">
                    <div style="position: absolute; inset: 4px">
                        <TimeSink min=Signal::derive(|| 0.0f32) max=Signal::derive(|| 3.0f32) linear=true
                            mode=TimeSinkMode::Data(h_est_amp) />
                    </div>
                    // Markers: unity (|H|=1), +9dB (~2.818) and -9dB (~0.355) on a 0..3 linear scale
                    // top% = 100 * (1 - value/3)
                    <div style="position: absolute; inset: 4px; pointer-events: none;">
                        <div style="position:absolute; left:0; right:0; top:66.7%; border-top:1px solid  #fcd34d;"></div>
                        <div style="position:absolute; left:0; right:0; top:6.1%;  border-top:1px dashed #94a3b8;"></div>
                        <div style="position:absolute; left:0; right:0; top:88.2%; border-top:1px dashed #94a3b8;"></div>
                        <span style="position:absolute; right:4px; top:66.7%; transform:translateY(-50%); font-size:10px; color:#fcd34d;">"|H|=1"</span>
                        <span style="position:absolute; right:4px; top:6.1%;  transform:translateY(-50%); font-size:10px; color:#94a3b8;">"+9dB"</span>
                        <span style="position:absolute; right:4px; top:88.2%; transform:translateY(-50%); font-size:10px; color:#94a3b8;">"-9dB"</span>
                    </div>
                </div>
            </div>

            // ── Right col (top): Spectrogram ───────────────────────────
            <div>
                <div class="flex items-center gap-2 mb-1">
                    <h2 class="text-sm text-white">"④ Spectrogram"</h2>
                    <input type="range" min="-100" max="50" value="-40" class="w-20 align-middle"
                        on:change=move |v| {
                            let target = v.target().unwrap();
                            let input: HtmlInputElement = target.dyn_into().unwrap();
                            min_label.get().unwrap().set_inner_text(&format!("min: {} dB", input.value()));
                            set_min(input.value().parse().unwrap());
                        } />
                    <span class="text-xs text-white" node_ref=min_label>"min: -40 dB"</span>
                    <input type="range" min="-40" max="100" value="20" class="w-20 align-middle"
                        on:change=move |v| {
                            let target = v.target().unwrap();
                            let input: HtmlInputElement = target.dyn_into().unwrap();
                            max_label.get().unwrap().set_inner_text(&format!("max: {} dB", input.value()));
                            set_max(input.value().parse().unwrap());
                        } />
                    <span class="text-xs text-white" node_ref=max_label>"max: 20 dB"</span>
                </div>
                <div class="border-2 rounded-md border-slate-500"
                     style="height: 160px; container-type: size; position: relative; overflow: hidden">
                    <div style="position: absolute; width: 100cqh; height: 100cqw; transform-origin: 0 0; transform: translateY(100cqh) rotate(-90deg)">
                        <Waterfall min=min max=max mode=WaterfallMode::Data(waterfall_data) />
                    </div>
                </div>

                // ── ① IQ Amp, under ④ ──────────────────────────────────
                <h2 class="text-sm text-white mt-2 mb-1">"① IQ Amp"</h2>
                <div class="border-2 rounded-md border-slate-500"
                     style="height: 160px; container-type: size; position: relative; overflow: hidden">
                    <span class="absolute left-1 top-0 text-[10px] text-slate-400 z-10">
                        {move || format!("{:.2}", iq_max.get())}
                    </span>
                    <span class="absolute left-1 bottom-0 text-[10px] text-slate-400 z-10">
                        {move || format!("{:.2}", iq_min.get())}
                    </span>
                    <div style="position: absolute; width: 100cqh; height: 100cqw; transform-origin: 0 0; transform: translateY(100cqh) rotate(-90deg)">
                        <SlidingTimeSink min=iq_min max=iq_max linear=true data=iq_data />
                    </div>
                </div>

                // ── ③ Correlation, under ① ─────────────────────────────
                <div class="flex items-center gap-2 mt-2 mb-1">
                    <h2 class="text-sm text-white">"③ Correlation (log)"</h2>
                    <input type="range" min="1000" max="1000000" step="1000" value="100000" class="w-32 align-middle"
                        on:change=move |v| {
                            let target = v.target().unwrap();
                            let input: HtmlInputElement = target.dyn_into().unwrap();
                            let val: f32 = input.value().parse().unwrap_or(100000.0);
                            cor_max_label.get().unwrap().set_inner_text(&format!("max: {:.0}", val));
                            set_cor_max_lin(val.clamp(1000.0, 1_000_000.0));
                        } />
                    <span class="text-xs text-white" node_ref=cor_max_label>"max: 100000"</span>
                    <span class="text-xs text-slate-300">
                        {move || format!("peak: {:.0}", cor_peak.get())}
                    </span>
                </div>
                <div class="border-2 rounded-md border-slate-500"
                     style="height: 160px; container-type: size; position: relative; overflow: hidden">
                    <span class="absolute left-1 top-0 text-[10px] text-slate-400 z-10">
                        {move || format!("{:.0}", cor_max_lin.get())}
                    </span>
                    <span class="absolute left-1 bottom-0 text-[10px] text-slate-400 z-10">"0"</span>
                    <div style="position: absolute; width: 100cqh; height: 100cqw; transform-origin: 0 0; transform: translateY(100cqh) rotate(-90deg)">
                        <SlidingTimeSinkDots
                            min=cor_min
                            max=Signal::derive(move || 10.0 * cor_max_lin.get().max(1.0e-20).log10())
                            linear=false
                            data=cor_data
                        />
                    </div>
                    // Fixed-value threshold markers, positions recomputed
                    // reactively as the max slider changes.
                    // top% = 100·(1 − 10·log10(value) / 10·log10(max))
                    <div style="position: absolute; inset: 0; pointer-events: none;">
                        <div
                            style:position="absolute" style:left="0" style:right="0"
                            style:border-top="1px dashed #94a3b8"
                            style:top=move || {
                                let denom = (10.0 * cor_max_lin.get().max(1000.0).log10()).max(1.0);
                                let pct = (1.0 - (10.0 * 1000.0f32.log10()) / denom).clamp(0.0, 1.0) * 100.0;
                                format!("{pct:.1}%")
                            }
                        ></div>
                        <div
                            style:position="absolute" style:left="0" style:right="0"
                            style:border-top="1px dashed #fbbf24"
                            style:top=move || {
                                let denom = (10.0 * cor_max_lin.get().max(1000.0).log10()).max(1.0);
                                let pct = (1.0 - (10.0 * 10_000.0f32.log10()) / denom).clamp(0.0, 1.0) * 100.0;
                                format!("{pct:.1}%")
                            }
                        ></div>
                        <div
                            style:position="absolute" style:left="0" style:right="0"
                            style:border-top="1px dashed #f87171"
                            style:top=move || {
                                let denom = (10.0 * cor_max_lin.get().max(1000.0).log10()).max(1.0);
                                let pct = (1.0 - (10.0 * 100_000.0f32.log10()) / denom).clamp(0.0, 1.0) * 100.0;
                                format!("{pct:.1}%")
                            }
                        ></div>
                        <span
                            style:position="absolute" style:right="4px" style:transform="translateY(-50%)"
                            style:font-size="10px" style:color="#94a3b8"
                            style:top=move || {
                                let denom = (10.0 * cor_max_lin.get().max(1000.0).log10()).max(1.0);
                                let pct = (1.0 - (10.0 * 1000.0f32.log10()) / denom).clamp(0.0, 1.0) * 100.0;
                                format!("{pct:.1}%")
                            }
                        >"1k"</span>
                        <span
                            style:position="absolute" style:right="4px" style:transform="translateY(-50%)"
                            style:font-size="10px" style:color="#fbbf24"
                            style:top=move || {
                                let denom = (10.0 * cor_max_lin.get().max(1000.0).log10()).max(1.0);
                                let pct = (1.0 - (10.0 * 10_000.0f32.log10()) / denom).clamp(0.0, 1.0) * 100.0;
                                format!("{pct:.1}%")
                            }
                        >"10k"</span>
                        <span
                            style:position="absolute" style:right="4px" style:transform="translateY(-50%)"
                            style:font-size="10px" style:color="#f87171"
                            style:top=move || {
                                let denom = (10.0 * cor_max_lin.get().max(1000.0).log10()).max(1.0);
                                let pct = (1.0 - (10.0 * 100_000.0f32.log10()) / denom).clamp(0.0, 1.0) * 100.0;
                                format!("{pct:.1}%")
                            }
                        >"100k"</span>
                    </div>
                </div>
                <div class="text-[10px] text-slate-400 mt-0.5">
                    {move || format!("t={:.1}ms", real_time.get() * 1000.0)}
                </div>

                // ── ⑤ Power, under ③ ───────────────────────────────────
                <h2 class="text-sm text-white mt-2 mb-1">"⑤ Power"</h2>
                <div class="border-2 rounded-md border-slate-500"
                     style="height: 160px; container-type: size; position: relative; overflow: hidden">
                    <div style="position: absolute; width: 100cqh; height: 100cqw; transform-origin: 0 0; transform: translateY(100cqh) rotate(-90deg)">
                        <SlidingTimeSink min=min max=max data=power_data />
                    </div>
                </div>

            </div>

            // ── ⑦ Preamble (SIG field) — sym1 cyan / sym2 red ─────────
            <div>
                <h2 class="text-sm text-white mb-1">
                    "⑦ Preamble — SIG sym1 (cyan) / sym2 (red) — rotation = CFO"
                </h2>
                <div class="border-2 border-slate-500 rounded-md mx-auto"
                     style="max-width: min(60vh, 100%); aspect-ratio: 1 / 1;">
                    <IqScatterDual
                        websocket_a="ws://127.0.0.1:9017"
                        websocket_b="ws://127.0.0.1:9018"
                        color_a="rgba(56,189,248,0.95)"
                        color_b="rgba(255,64,64,0.95)" />
                </div>
            </div>

            // ── ⑧ Data constellation density ───────────────────────────
            <div>
                <h2 class="text-sm text-white mb-1">"⑧ Data constellation"</h2>
                <div class="border-2 border-slate-500 rounded-md mx-auto"
                     style="max-width: min(60vh, 100%); aspect-ratio: 1 / 1;">
                    <ConstellationSinkDensity width=width websocket="ws://127.0.0.1:9012" />
                </div>
            </div>

            // ── ⑨ SyncLong correlation landscape ──────────────────────
            // LTF has two identical 64-sample halves → two peaks ~64 apart.
            // If gap != 64 or the primary peak jitters between two indices,
            // sync_long is locking on the wrong sample.
            <div class="col-span-2">
                <div class="flex items-center gap-3 mb-1">
                    <h2 class="text-sm text-white">"⑨ SyncLong correlation (160 samples)"</h2>
                    <span class="text-[11px] text-slate-300">
                        "peak1=" {move || sync_peak1.get()}
                        " peak2=" {move || sync_peak2.get()}
                        " gap=" {move || sync_gap.get()} " (expect 64)"
                        " fine_cfo=" {move || format!("{:.5} rad/samp", sync_cfo.get())}
                    </span>
                </div>
                <div class="border-2 rounded-md border-slate-500"
                     style="position: relative; height: 180px">
                    <div style="position: absolute; inset: 4px">
                        <TimeSink min=Signal::derive(|| 0.0f32) max=Signal::derive(|| 5.0f32) linear=true
                            mode=TimeSinkMode::Data(sync_corr_data) />
                    </div>
                    // Markers at sample indices 0, 32, 64, 96, 128, 160 (of 160)
                    <div style="position: absolute; inset: 4px; pointer-events: none;">
                        <div style="position:absolute; top:0; bottom:0; left:20%;  border-left:1px dashed #64748b;"></div>
                        <div style="position:absolute; top:0; bottom:0; left:40%;  border-left:1px solid  #fcd34d;"></div>
                        <div style="position:absolute; top:0; bottom:0; left:60%;  border-left:1px dashed #64748b;"></div>
                        <div style="position:absolute; top:0; bottom:0; left:80%;  border-left:1px dashed #64748b;"></div>
                        <span style="position:absolute; left:40%; top:2px; transform:translateX(-50%); font-size:10px; color:#fcd34d;">"64"</span>
                    </div>
                </div>
            </div>

            // ── ⑩ Time-domain LTF magnitudes (128 samples post-sync) ──
            // Two 64-sample halves should be identical in envelope. Any
            // mismatch = wrong sync offset. Midline at sample 64.
            <div class="col-span-2">
                <h2 class="text-sm text-white mb-1">
                    "⑩ Time-domain LTF magnitude (128 samples post-sync, pre-CFO)"
                </h2>
                <div class="border-2 rounded-md border-slate-500"
                     style="position: relative; height: 140px">
                    <div style="position: absolute; inset: 4px">
                        <TimeSink min=Signal::derive(|| 0.0f32) max=Signal::derive(|| 0.5f32) linear=true
                            mode=TimeSinkMode::Data(ltf_td_mag) />
                    </div>
                    <div style="position: absolute; inset: 4px; pointer-events: none;">
                        <div style="position:absolute; top:0; bottom:0; left:50%; border-left:1px solid #fcd34d;"></div>
                        <span style="position:absolute; left:50%; top:2px; transform:translateX(-50%); font-size:10px; color:#fcd34d;">"LTF split"</span>
                    </div>
                </div>
            </div>

            // ── Full width: Flowgraph ──────────────────────────────────
            <div class="col-span-2 border-2 rounded-md border-slate-500 p-2">
                {move || {
                    if let Some(Some(desc)) = fg_desc.get() {
                        return view! { <FlowgraphMermaid fg=desc /> }.into_any();
                    }
                    ().into_any()
                }}
            </div>
        </div>
    }
}

#[component]
pub fn Gui() -> impl IntoView {
    // let rt_url = window().location().origin().unwrap();
    // let rt_handle = RuntimeHandle::from_url(rt_url);
    let rt_handle = RuntimeHandle::from_url("http://127.0.0.1:1337");

    let fg_handle = LocalResource::new(move || {
        let rt_handle = rt_handle.clone();
        async move {
            if let Ok(fg) = rt_handle.get_flowgraph(FlowgraphId(0)).await {
                Some(fg)
            } else {
                None
            }
        }
    });

    view! {
        <h1 class="text-xl text-white m-4">FutureSDR WLAN</h1>
        {move || {
            match fg_handle.get() {
                Some(wrapped) => match wrapped {
                    Some(handle) => view! { <Wlan fg_handle=handle /> }.into_any(),
                    _ => view! {}.into_any(),
                }
                _ => view! { <div>"Connecting"</div> }.into_any(),
            }
        }}
    }
}

pub fn frontend() {
    console_error_panic_hook::set_once();
    futuresdr::runtime::init();
    mount_to_body(|| view! { <Gui /> })
}
