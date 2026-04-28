use any_spawner::Executor;
use futuresdr::runtime::FlowgraphId;
use futuresdr::runtime::Pmt;
use leptos::html::Span;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use leptos::web_sys::HtmlInputElement;
use prophecy::ConstellationSinkDensity;
use prophecy::FlowgraphHandle;
use prophecy::FlowgraphMermaid;
use prophecy::ListSelector;
use prophecy::RadioSelector;
use prophecy::RuntimeHandle;
use prophecy::TimeSink;
use prophecy::TimeSinkMode;
use prophecy::Waterfall;
use prophecy::WaterfallMode;

#[component]
pub fn Hallow(fg_handle: FlowgraphHandle) -> impl IntoView {
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
    let (eye_width, _set_eye_width) = signal(1.0f32);
    let (ts_min, _set_ts_min) = signal(-1.0f32);
    let (ts_max, _set_ts_max) = signal(1.0f32);
    let (wf_min, _set_wf_min) = signal(-40.0f32);
    let (wf_max, _set_wf_max) = signal(0.0f32);
    let (spec_min, _set_spec_min) = signal(-60.0f32);
    let (spec_max, _set_spec_max) = signal(0.0f32);

    let width_label = NodeRef::<Span>::new();
    let gain_label = NodeRef::<Span>::new();

    view! {
        <div class="border-2 border-slate-500 rounded-md flex flex-row flex-wrap m-4 p-4">
            <div class="basis-1/3">
                <input type="range" min="0" max="10" value="2" class="align-middle"
                    on:change= move |v| {
                        let target = v.target().unwrap();
                        let input : HtmlInputElement = target.dyn_into().unwrap();
                        width_label.get().unwrap().set_inner_text(&format!("width: {}", input.value()));
                        set_width(input.value().parse().unwrap());
                    } />
                <span class="text-white p-2 m-2" node_ref=width_label>"width: 2"</span>
            </div>

            <div class="basis-1/3 text-white">
                <RadioSelector fg_handle=fg_handle.clone() block_id=0 handler="sample_rate" values=[
                    ("1 MHz".to_string(), Pmt::F64(1e6)),
                    ("2 MHz".to_string(), Pmt::F64(2e6)),
                    ("4 MHz".to_string(), Pmt::F64(4e6)),
                ] label_class="p-2" />
            </div>
            <div class="basis-1/3">
                <span class="text-white m-2">"HaLow Channel"</span>
                <ListSelector fg_handle=fg_handle.clone() block_id=0 handler="freq" values=[
                    // 1 MHz channels
                    ("1".to_string(),   Pmt::F64(902.5e6)),
                    ("3".to_string(),   Pmt::F64(903.5e6)),
                    ("5".to_string(),   Pmt::F64(904.5e6)),
                    ("7".to_string(),   Pmt::F64(905.5e6)),
                    ("9".to_string(),   Pmt::F64(906.5e6)),
                    ("11".to_string(),  Pmt::F64(907.5e6)),
                    ("36".to_string(),  Pmt::F64(908.5e6)),
                    ("37".to_string(),  Pmt::F64(909.5e6)),
                    ("38".to_string(),  Pmt::F64(910.5e6)),
                    ("39".to_string(),  Pmt::F64(911.5e6)),
                    ("40".to_string(),  Pmt::F64(912.5e6)),
                    ("41".to_string(),  Pmt::F64(913.5e6)),
                    ("42".to_string(),  Pmt::F64(914.5e6)),
                    ("43".to_string(),  Pmt::F64(915.5e6)),
                    ("44".to_string(),  Pmt::F64(916.5e6)),
                    ("45".to_string(),  Pmt::F64(917.5e6)),
                    ("46".to_string(),  Pmt::F64(918.5e6)),
                    ("47".to_string(),  Pmt::F64(919.5e6)),
                    ("48".to_string(),  Pmt::F64(920.5e6)),
                    ("149".to_string(), Pmt::F64(921.5e6)),
                    ("150".to_string(), Pmt::F64(922.5e6)),
                    ("151".to_string(), Pmt::F64(923.5e6)),
                    ("152".to_string(), Pmt::F64(924.5e6)),
                    ("100".to_string(), Pmt::F64(925.5e6)),
                    ("104".to_string(), Pmt::F64(926.5e6)),
                    ("108".to_string(), Pmt::F64(927.5e6)),
                    // 2 MHz channels
                    ("2".to_string(),   Pmt::F64(903.0e6)),
                    ("6".to_string(),   Pmt::F64(905.0e6)),
                    ("10".to_string(),  Pmt::F64(907.0e6)),
                    ("153".to_string(), Pmt::F64(909.0e6)),
                    ("154".to_string(), Pmt::F64(911.0e6)),
                    ("155".to_string(), Pmt::F64(913.0e6)),
                    ("156".to_string(), Pmt::F64(915.0e6)),
                    ("157".to_string(), Pmt::F64(917.0e6)),
                    ("158".to_string(), Pmt::F64(919.0e6)),
                    ("159".to_string(), Pmt::F64(921.0e6)),
                    ("160".to_string(), Pmt::F64(923.0e6)),
                    ("161".to_string(), Pmt::F64(925.0e6)),
                    ("112".to_string(), Pmt::F64(927.0e6)),
                    // 4 MHz channels
                    ("8".to_string(),   Pmt::F64(906.0e6)),
                    ("162".to_string(), Pmt::F64(910.0e6)),
                    ("163".to_string(), Pmt::F64(914.0e6)),
                    ("164".to_string(), Pmt::F64(918.0e6)),
                    ("165".to_string(), Pmt::F64(922.0e6)),
                    ("116".to_string(), Pmt::F64(926.0e6)),
                ] />
            </div>
            <div class="basis-1/3">
                <input type="range" min="0" max="80" value="40" class="align-middle"
                    on:change= {
                        let fg_handle = fg_handle.clone();
                        move |v| {
                            let target = v.target().unwrap();
                            let input : HtmlInputElement = target.dyn_into().unwrap();
                            gain_label.get().unwrap().set_inner_text(&format!("gain: {} dB", input.value()));
                            let gain : f64 = input.value().parse().unwrap();
                            let p = Pmt::F64(gain);
                            let mut fg_handle = fg_handle.clone();
                            spawn_local(async move {
                                let _ = fg_handle.call(0, "gain", p).await;
                            });
                    }} />
                <span class="text-white p-2 m-2" node_ref=gain_label>"gain: 40 dB"</span>
            </div>
        </div>

        // Constellation plot (equalized symbols)
        <div class="border-2 border-slate-500 rounded-md m-4" style="height: 400px">
            <h2 class="text-white p-2">"Constellation (Equalized Symbols)"</h2>
            <ConstellationSinkDensity width=width />
        </div>

        // Sync correlation metric
        <div class="border-2 border-slate-500 rounded-md m-4" style="height: 300px">
            <h2 class="text-white p-2">"Sync Correlation Metric"</h2>
            <TimeSink mode=TimeSinkMode::Websocket("ws://127.0.0.1:9004".to_string()) min=ts_min max=ts_max />
        </div>

        // Post-sync time domain
        <div class="border-2 border-slate-500 rounded-md m-4" style="height: 300px">
            <h2 class="text-white p-2">"Post-Sync Time Domain"</h2>
            <TimeSink mode=TimeSinkMode::Websocket("ws://127.0.0.1:9001".to_string()) min=ts_min max=ts_max />
        </div>

        // Power spectrum (clearer than waterfall)
        <div class="border-2 border-slate-500 rounded-md m-4" style="height: 300px">
            <h2 class="text-white p-2">"Power Spectrum (dB)"</h2>
            <TimeSink mode=TimeSinkMode::Websocket("ws://127.0.0.1:9005".to_string()) min=spec_min max=spec_max />
        </div>

        // Post-FFT waterfall
        <div class="border-2 border-slate-500 rounded-md m-4" style="height: 400px">
            <h2 class="text-white p-2">"Post-FFT Waterfall"</h2>
            <Waterfall mode=WaterfallMode::Websocket("ws://127.0.0.1:9003".to_string()) min=wf_min max=wf_max />
        </div>

        // Eye diagram (density)
        <div class="border-2 border-slate-500 rounded-md m-4" style="height: 400px">
            <h2 class="text-white p-2">"Eye Diagram"</h2>
            <ConstellationSinkDensity width=eye_width websocket="ws://127.0.0.1:9006" />
        </div>

        // Flowgraph diagram
        <div class="border-2 border-slate-500 rounded-md m-4 p-4">
            {move || {
                match fg_desc.get() {
                    Some(Some(desc)) => view! { <FlowgraphMermaid fg=desc /> }.into_any(),
                    _ => view! {}.into_any(),
                }
            }}
        </div>
    }
}

#[component]
pub fn Gui() -> impl IntoView {
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
        <h1 class="text-xl text-white m-4">"FutureSDR HaLow (802.11ah)"</h1>
        {move || {
            match fg_handle.get() {
                Some(wrapped) => match wrapped {
                    Some(handle) => view! { <Hallow fg_handle=handle /> }.into_any(),
                    _ => view! {}.into_any(),
                }
                _ => view! { <div>"Connecting"</div> }.into_any(),
            }
        }}
    }
}

pub fn frontend() {
    console_error_panic_hook::set_once();
    Executor::init_wasm_bindgen().unwrap();
    mount_to_body(|| view! { <Gui /> })
}
