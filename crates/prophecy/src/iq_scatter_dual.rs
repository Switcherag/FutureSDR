use futures::StreamExt;
use gloo_net::websocket::Message;
use gloo_net::websocket::futures::WebSocket;
use leptos::html::Canvas;
use leptos::logging::*;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use std::cell::RefCell;
use std::rc::Rc;
use web_sys::CanvasRenderingContext2d;
use web_sys::HtmlCanvasElement;

/// Square IQ scatter plot that overlays two complex streams coming from
/// two WebSockets (raw bytes of interleaved f32 pairs, as emitted by
/// `WebsocketPmtSink` for `Pmt::VecCF32`).
///
/// Stream `a` renders in `color_a` (default white — data symbols).
/// Stream `b` renders in `color_b` (default red — preamble symbols).
/// The canvas is kept square by the parent container (use `aspect-ratio: 1 / 1`).
#[component]
pub fn IqScatterDual(
    #[prop(into)] websocket_a: String,
    #[prop(into)] websocket_b: String,
    #[prop(into, default = "rgba(255,255,255,0.85)".to_string())] color_a: String,
    #[prop(into, default = "rgba(255,64,64,0.9)".to_string())] color_b: String,
) -> impl IntoView {
    let canvas_ref = NodeRef::<Canvas>::new();
    let buf_a: Rc<RefCell<Vec<(f32, f32)>>> = Rc::new(RefCell::new(Vec::new()));
    let buf_b: Rc<RefCell<Vec<(f32, f32)>>> = Rc::new(RefCell::new(Vec::new()));

    fn decode(b: &[u8]) -> Vec<(f32, f32)> {
        let n = b.len() / 8;
        let f = unsafe { std::slice::from_raw_parts(b.as_ptr() as *const f32, n * 2) };
        (0..n).map(|i| (f[2 * i], f[2 * i + 1])).collect()
    }

    {
        let buf = buf_a.clone();
        spawn_local(async move {
            match WebSocket::open(&websocket_a) {
                Ok(mut ws) => {
                    while let Some(msg) = ws.next().await {
                        if let Ok(Message::Bytes(b)) = msg {
                            *buf.borrow_mut() = decode(&b);
                        }
                    }
                    log!("IqScatterDual(a): closed");
                }
                Err(e) => log!("IqScatterDual(a) open failed: {:?}", e),
            }
        });
    }
    {
        let buf = buf_b.clone();
        spawn_local(async move {
            match WebSocket::open(&websocket_b) {
                Ok(mut ws) => {
                    while let Some(msg) = ws.next().await {
                        if let Ok(Message::Bytes(b)) = msg {
                            *buf.borrow_mut() = decode(&b);
                        }
                    }
                    log!("IqScatterDual(b): closed");
                }
                Err(e) => log!("IqScatterDual(b) open failed: {:?}", e),
            }
        });
    }

    // Redraw loop: rAF via gloo-timers isn't quite right; use a 30 Hz timer.
    {
        let buf_a = buf_a.clone();
        let buf_b = buf_b.clone();
        let color_a = color_a.clone();
        let color_b = color_b.clone();
        spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(33).await;
                let Some(canvas) = canvas_ref.get() else { continue };
                let canvas: HtmlCanvasElement = canvas.unchecked_into();
                let w = canvas.client_width().max(1) as u32;
                let h = canvas.client_height().max(1) as u32;
                let side = w.min(h);
                if canvas.width() != side {
                    canvas.set_width(side);
                }
                if canvas.height() != side {
                    canvas.set_height(side);
                }
                let Ok(Some(ctx_raw)) = canvas.get_context("2d") else { continue };
                let ctx: CanvasRenderingContext2d = ctx_raw.unchecked_into();

                let sf = side as f64;
                ctx.set_fill_style_str("#000");
                ctx.fill_rect(0.0, 0.0, sf, sf);

                let a = buf_a.borrow().clone();
                let b = buf_b.borrow().clone();
                let mut m: f32 = 0.2;
                for (x, y) in a.iter().chain(b.iter()) {
                    m = m.max(x.abs()).max(y.abs());
                }
                let scale = (side as f32 * 0.48) / m;
                let cx = side as f32 * 0.5;
                let cy = side as f32 * 0.5;

                ctx.set_stroke_style_str("#444");
                ctx.set_line_width(1.0);
                ctx.begin_path();
                ctx.move_to(0.0, cy as f64);
                ctx.line_to(sf, cy as f64);
                ctx.move_to(cx as f64, 0.0);
                ctx.line_to(cx as f64, sf);
                ctx.stroke();

                let draw = |pts: &[(f32, f32)], color: &str| {
                    ctx.set_fill_style_str(color);
                    for (x, y) in pts {
                        let px = (cx + x * scale) as f64;
                        let py = (cy - y * scale) as f64;
                        ctx.fill_rect(px - 1.0, py - 1.0, 2.0, 2.0);
                    }
                };
                draw(&a, &color_a);
                draw(&b, &color_b);
            }
        });
    }

    view! {
        <canvas node_ref=canvas_ref
                style="width: 100%; height: 100%; display: block; background: #000" />
    }
}
