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
use std::rc::Rc;

#[component]
pub fn MacConsole(fg_handle: FlowgraphHandle) -> impl IntoView {
    let (rx_messages, set_rx_messages) = signal(Vec::<String>::new());
    let (tx_messages, set_tx_messages) = signal(Vec::<String>::new());
    let (tx_input, set_tx_input) = signal(String::new());
    let (status_msg, set_status_msg) = signal(String::new());
    let (auto_send_active, set_auto_send_active) = signal(false);
    let (auto_send_count, set_auto_send_count) = signal(0u64);
    
    // Clone fg_handle for auto-send effect
    let fg_handle_for_auto = fg_handle.clone();
    
    // Auto-send effect: sends messages every second when active
    Effect::new(move |_| {
        if auto_send_active.get() {
            let fg_handle_clone = fg_handle_for_auto.clone();
            let interval_handle = set_interval(
                move || {
                    let mut fg = fg_handle_clone.clone();
                    let count = auto_send_count.get();
                    
                    // Send message via tx port
                    let msg = format!("FutureSDR {}", count);
                    let msg_for_display = msg.clone();
                    let pmt = Pmt::Blob(msg.as_bytes().to_vec());
                    
                    spawn_local(async move {
                        match fg.call(0, "tx", pmt).await {
                            Ok(_) => {
                                leptos::logging::log!("Auto-sent: {}", msg);
                            }
                            Err(e) => {
                                leptos::logging::error!("Auto-send failed: {:?}", e);
                            }
                        }
                    });
                    
                    // Update counter
                    set_auto_send_count.update(|c| *c += 1);
                    
                    // Update TX display
                    set_tx_messages.update(|msgs| {
                        msgs.push(format!("[Auto] {}", msg_for_display));
                        if msgs.len() > 50 {
                            msgs.remove(0);
                        }
                    });
                },
                std::time::Duration::from_secs(1),
            );
            
            // Store the handle so we can clean it up
            on_cleanup(move || {
                drop(interval_handle);
            });
        }
    });
    
    // Subscribe to RX messages via WebSocket (port 9003)
    Effect::new(move |_| {
        use leptos::web_sys::WebSocket;
        use leptos::wasm_bindgen::closure::Closure;
        use leptos::wasm_bindgen::JsCast;
        
        // Get the hostname from the current page's location
        let host = leptos::web_sys::window()
            .and_then(|w| w.location().hostname().ok())
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let ws_url = format!("ws://{}:9003", host);
        
        let ws = match WebSocket::new(&ws_url) {
            Ok(ws) => ws,
            Err(e) => {
                leptos::logging::warn!("Failed to connect to RX WebSocket: {:?}", e);
                return;
            }
        };
        
        let set_rx_messages_clone = set_rx_messages.clone();
        let onmessage_callback = Closure::wrap(Box::new(move |e: leptos::web_sys::MessageEvent| {
            if let Ok(txt) = e.data().dyn_into::<leptos::wasm_bindgen::JsValue>() {
                if let Some(msg_str) = txt.as_string() {
                    if !msg_str.is_empty() {
                        leptos::logging::log!("RX WebSocket: {}", msg_str);
                        if msg_str == "initialized" {
                            leptos::logging::log!("Flowgraph initialized! Auto-refreshing page...");
                            // Reload page when flowgraph finishes initialization
                            if let Some(window) = leptos::web_sys::window() {
                                let _ = window.location().reload();
                            }
                        } else if msg_str == "reload" {
                            leptos::logging::log!("Received reload signal from backend (no page reload)");
                            // Here you can trigger a signal update or refetch logic instead of reloading the page
                        } else {
                            set_rx_messages_clone.update(|msgs| {
                                msgs.push(msg_str);
                                if msgs.len() > 50 {
                                    msgs.remove(0);
                                }
                            });
                        }
                    }
                }
            }
        }) as Box<dyn FnMut(_)>);
        ws.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
        onmessage_callback.forget();
    });

    let send_message = move |_ev| {
        let text = tx_input.get();
        if !text.is_empty() {
            let pmt = Pmt::Blob(text.as_bytes().to_vec());
            let mut fg_handle = fg_handle.clone();
            let text_clone = text.clone();
            
            spawn_local(async move {
                // Send to FlowgraphController (block 0) which forwards to MAC
                leptos::logging::log!("Sending message via FlowgraphController (block 0)");
                match fg_handle.call(0, "tx", pmt).await {
                    Ok(_) => {
                        leptos::logging::log!("Message sent successfully");
                    }
                    Err(e) => {
                        leptos::logging::error!("Failed to send message: {:?}", e);
                    }
                }
            });
            
            // Add to TX messages display
            set_tx_messages.update(|msgs| {
                msgs.push(format!("[Manual] {}", text_clone));
                // Keep only last 50 messages
                if msgs.len() > 50 {
                    msgs.remove(0);
                }
            });
            
            set_tx_input(String::new());
            set_status_msg(format!("Sent: {}", text));
            
            // Clear status after 3 seconds using set_timeout
            let set_status_msg_clone = set_status_msg.clone();
            set_timeout(
                move || {
                    set_status_msg_clone(String::new());
                },
                std::time::Duration::from_secs(3),
            );
        }
    };

    let toggle_auto_send = move |_| {
        set_auto_send_active.update(|active| *active = !*active);
        if !auto_send_active.get() {
            set_auto_send_count(0);
        }
    };

    view! {
        <div class="h-full flex flex-col">
            <div class="flex justify-between items-center mb-4">
                <h2 class="text-lg text-white">"MAC Console"</h2>
                <div class="flex items-center gap-4">
                    <button
                        class=move || {
                            if auto_send_active.get() {
                                "bg-green-600 hover:bg-green-700 text-white px-4 py-2 rounded flex items-center gap-2"
                            } else {
                                "bg-gray-600 hover:bg-gray-700 text-white px-4 py-2 rounded flex items-center gap-2"
                            }
                        }
                        on:click=toggle_auto_send
                    >
                        {move || {
                            if auto_send_active.get() {
                                view! {
                                    <>
                                        <div class="w-2 h-2 bg-white rounded-full animate-pulse"></div>
                                        {format!("Auto-send ON ({})", auto_send_count.get())}
                                    </>
                                }.into_any()
                            } else {
                                view! { <>"Auto-send OFF"</> }.into_any()
                            }
                        }}
                    </button>
                </div>
            </div>
            
            // TX Messages Display
            <div class="flex-1 mb-4 flex flex-col">
                <h3 class="text-white mb-2">"Transmitted Messages:"</h3>
                <div class="flex-1 bg-black border border-gray-600 rounded p-2 overflow-y-auto font-mono text-sm">
                    {move || {
                        let messages = tx_messages.get();
                        
                        if messages.is_empty() {
                            view! {
                                <div class="text-gray-500">"No messages sent yet..."</div>
                            }.into_any()
                        } else {
                            view! {
                                <div>
                                    {messages.iter().enumerate().map(|(i, msg)| {
                                        view! {
                                            <div class="text-blue-400 mb-1">
                                                <span class="text-gray-400">{format!("[{}] ", i)}</span>
                                                {msg.clone()}
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()}
                                </div>
                            }.into_any()
                        }
                    }}
                </div>
            </div>
            
            // RX Messages Display
            <div class="flex-1 mb-4 flex flex-col">
                <h3 class="text-white mb-2">"Received Messages (Decoder):"</h3>
                <div class="flex-1 bg-black border border-gray-600 rounded p-2 overflow-y-auto font-mono text-sm">
                    {move || {
                        let messages = rx_messages.get();
                        if messages.is_empty() {
                            view! {
                                <div class="text-gray-500">"No messages received yet..."</div>
                            }.into_any()
                        } else {
                            view! {
                                <div>
                                    {messages.iter().enumerate().map(|(i, msg)| {
                                        view! {
                                            <div class="text-green-400 mb-1">
                                                <span class="text-gray-400">{format!("[{}] ", i)}</span>
                                                {msg.clone()}
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()}
                                </div>
                            }.into_any()
                        }
                    }}
                </div>
            </div>
            
            // TX Message Input
            <div class="flex-shrink-0">
                <h3 class="text-white mb-2">"Send Message:"</h3>
                <div class="flex flex-row gap-2">
                    <textarea
                        prop:value=tx_input
                        class="flex-1 bg-gray-800 text-white border border-gray-600 rounded p-2 font-mono text-sm"
                        rows="3"
                        placeholder="Type your message here..."
                        on:input=move |ev| {
                            let val = event_target_value(&ev);
                            set_tx_input(val);
                        }
                    />
                    <button
                        class="bg-blue-600 hover:bg-blue-700 text-white px-4 py-2 rounded"
                        on:click=send_message
                    >
                        "Send"
                    </button>
                </div>
                <div class="text-gray-400 text-sm mt-1">
                    {move || {
                        let status = status_msg.get();
                        if status.is_empty() {
                            "Click Send button to transmit to MAC".to_string()
                        } else {
                            status
                        }
                    }}
                </div>
            </div>
        </div>
    }
}

#[component]
pub fn Wlan(
    fg_handle: FlowgraphHandle,
    #[prop(optional)] _key: Option<u32>,
) -> impl IntoView {
    let fg_desc = {
        let fg_handle = fg_handle.clone();
        LocalResource::new(move || {
            let mut fg_handle = fg_handle.clone();
            async move {
                leptos::logging::log!("Fetching flowgraph description for mermaid diagram");
                if let Ok(desc) = fg_handle.description().await {
                    return Some(desc);
                }
                None
            }
        })
    };

    let (width, set_width) = signal(2.0f32);

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
                    ("5 MHz".to_string(), Pmt::F64(5e6)),
                    ("10 MHz".to_string(), Pmt::F64(10e6)),
                    ("40 MHz".to_string(), Pmt::F64(20e6)),
                ] label_class="p-2" />
            </div>
            <div class="basis-1/3">
                <span class="text-white m-2">WLAN Channel</span>
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
            <div class="basis-1/3">
                <input type="range" min="0" max="80" value="60" class="align-middle"
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
                <span class="text-white p-2 m-2" node_ref=gain_label>"gain: 60 dB"</span>
            </div>
        </div>

        <div class="flex flex-row gap-4 m-4" style="height: 800px; max-height: 90vh">
            <div class="flex-1 border-2 border-slate-500 rounded-md">
                <ConstellationSinkDensity width=width />
            </div>
            <div class="flex-1 border-2 border-slate-500 rounded-md p-4 overflow-y-auto">
                <MacConsole fg_handle=fg_handle.clone() />
            </div>
        </div>

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
pub fn FlowgraphSelector(
    rt_handle: RuntimeHandle,
    #[prop(optional)] on_switch: Option<Rc<dyn Fn()>>,
) -> impl IntoView {
    let (flowgraphs, set_flowgraphs) = signal(Vec::<String>::new());
    let (selected, set_selected) = signal(String::new());
    let (status, set_status) = signal(String::new());
    
    // Load available flowgraphs - hardcoded list since WASM can't access filesystem
    // To add new flowgraphs, add them to this list
    Effect::new(move |_| {
        let fgs = vec![
            "flowgraphs/control_only.toml",
            "flowgraphs/nullstream.toml",
            "flowgraphs/wifi_loopback.toml",
            "flowgraphs/wifi_rx.toml",
            "flowgraphs/wifi_tx.toml",
            "flowgraphs/wifi_tx_bis.toml",
            "flowgraphs/zigbee_rx.toml",
            "flowgraphs/zigbee_rx_v2.toml",
            "flowgraphs/zigbee_rx_v3.toml",
            "flowgraphs/zigbee_trx.toml",
            "flowgraphs/zigbee_tx.toml",
            "flowgraphs/zigbee_tx_v2.toml",
        ].into_iter().map(|s| s.to_string()).collect::<Vec<_>>();
        
        if !fgs.is_empty() {
            set_selected(fgs[0].clone());
        }
        set_flowgraphs(fgs);
    });
    
    let switch_flowgraph = move |_| {
        let fg_path = selected.get();
        if !fg_path.is_empty() {
            set_status(format!("Switching to {}...", fg_path));
            
            let rt = rt_handle.clone();
            let fg_clone = fg_path.clone();
            let callback = on_switch.clone();
            spawn_local(async move {
                // Get latest flowgraph handle
                let fg_handle_opt = if let Ok(fg_ids) = rt.get_flowgraphs().await {
                    if let Some(latest_id) = fg_ids.last() {
                        rt.get_flowgraph(*latest_id).await.ok()
                    } else {
                        None
                    }
                } else {
                    None
                };
                
                match fg_handle_opt {
                    Some(mut fg_handle) => {
                        // Send PMT message to FlowgraphController (always at block 0)
                        use futuresdr::runtime::Pmt;
                        let pmt = Pmt::String(fg_clone.clone());
                        
                        match fg_handle.call(0, "control", pmt).await {
                            Ok(_) => {
                                set_status(format!("✓ Switching to {}", fg_clone));
                                // Notify parent component that switch happened
                                if let Some(ref cb) = callback {
                                    cb();
                                }
                                // No window reload here; handled by RX WebSocket
                            }
                            Err(e) => {
                                set_status(format!("✗ Error: {}", e));
                            }
                        }
                    }
                    None => {
                        set_status(format!("✗ Error getting latest flowgraph"));
                    }
                }
            });
        }
    };
    
    view! {
        <div class="border-2 border-slate-500 rounded-md m-4 p-4">
            <h3 class="text-white mb-2">"Flowgraph Selector"</h3>
            <div class="flex gap-2 items-center">
                <select
                    class="flex-1 bg-gray-800 text-white border border-gray-600 rounded p-2"
                    on:change=move |ev| {
                        set_selected(event_target_value(&ev));
                    }
                >
                    {move || {
                        flowgraphs.get().iter().map(|fg| {
                            let is_selected = fg == &selected.get();
                            view! {
                                <option value=fg.clone() selected=is_selected>
                                    {fg.clone()}
                                </option>
                            }
                        }).collect::<Vec<_>>()
                    }}
                </select>
                <button
                    class="bg-green-600 hover:bg-green-700 text-white px-4 py-2 rounded"
                    on:click=switch_flowgraph
                >
                    "Switch"
                </button>
            </div>
            <div class="text-gray-400 text-sm mt-2">
                {move || status.get()}
            </div>
        </div>
    }
}

#[component]
pub fn Gui() -> impl IntoView {
    // Get the hostname from the current page's location for remote access
    let host = leptos::web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let rt_url = format!("http://{}:1337", host);
    
    let rt_handle = RuntimeHandle::from_url(&rt_url);
    let rt_handle_clone = rt_handle.clone();
    
    // Signal to track flowgraph switches
    let (fg_version, set_fg_version) = signal(0u32);
    
    // Simple reload button handler
    let handle_reload = move |_| {
        leptos::logging::log!("Manual reload triggered");
        if let Some(window) = leptos::web_sys::window() {
            let _ = window.location().reload();
        }
    };

    let fg_handle = LocalResource::new(move || {
        let rt_handle = rt_handle_clone.clone();
        // Track fg_version to make this reactive
        let _version = fg_version.get();
        
        async move {
            // Get list of all flowgraphs and use the latest one (highest ID)
            if let Ok(fg_ids) = rt_handle.get_flowgraphs().await {
                if let Some(latest_id) = fg_ids.last() {
                    if let Ok(fg) = rt_handle.get_flowgraph(*latest_id).await {
                        leptos::logging::log!("Connected to flowgraph {:?}", latest_id);
                        return Some(fg);
                    }
                }
            }
            // Fallback to ID 0
            if let Ok(fg) = rt_handle.get_flowgraph(FlowgraphId(0)).await {
                Some(fg)
            } else {
                None
            }
        }
    });
    
    // Callback to trigger refetch when flowgraph switches
    let on_switch: Rc<dyn Fn()> = Rc::new(move || {
        leptos::logging::log!("Flowgraph switched, refetching handle...");
        set_fg_version.update(|v| *v += 1);
    });

    view! {
        <h1 class="text-xl text-white m-4">FutureSDR Radio Frontend</h1>
        <div class="m-4 flex gap-2">
            <FlowgraphSelector rt_handle=rt_handle.clone() on_switch=on_switch />
            <button
                class="bg-blue-600 hover:bg-blue-700 text-white px-4 py-2 rounded"
                on:click=handle_reload
            >
                "Reload Page"
            </button>
        </div>
        {move || {
            let version = fg_version.get();
            match fg_handle.get() {
                Some(wrapped) => match wrapped {
                    Some(handle) => {
                        // Use version as key to force recreation of Wlan component when flowgraph changes
                        leptos::logging::log!("Rendering Wlan component with version {}", version);
                        view! { <Wlan fg_handle=handle _key=version /> }.into_any()
                    },
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
