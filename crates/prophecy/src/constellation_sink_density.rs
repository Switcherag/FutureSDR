use futures::StreamExt;
use gloo_net::websocket::Message;
use gloo_net::websocket::futures::WebSocket;
use leptos::html::Canvas;
use leptos::logging::*;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::prelude::*;
use num_complex::Complex32;
use std::cell::RefCell;
use std::rc::Rc;
use web_sys::HtmlCanvasElement;
use web_sys::WebGl2RenderingContext as GL;

use crate::ArrayView;

pub const DEFAULT_BINS: usize = 256;

struct RenderState {
    canvas: HtmlCanvasElement,
    gl: GL,
    width: Signal<f32>,
    bins: usize,
    texture: Vec<f32>,
}

#[component]
/// Constellation Sink with configurable density resolution
///
/// See WLAN receiver for an example.
///
/// # Parameters
/// - `width`: The coordinate range for the constellation (e.g., 2.0 means -2 to +2)
/// - `bins`: Number of bins per dimension for the density map (default: 256). Higher = more detail.
/// - `decay`: Decay factor per sample (default: 0.999). Lower = faster fade.
/// - `intensity`: Intensity increment per sample hit (default: 0.1).
/// - `websocket`: WebSocket URL for receiving constellation data.
pub fn ConstellationSinkDensity(
    #[prop(into)] width: Signal<f32>,
    #[prop(optional, default = DEFAULT_BINS)] bins: usize,
    #[prop(optional, default = 0.999f32)] decay: f32,
    #[prop(optional, default = 0.1f32)] intensity: f32,
    #[prop(optional, into, default = "ws://127.0.0.1:9002".to_string())] websocket: String,
) -> impl IntoView {
    let data = Rc::new(RefCell::new(None));
    {
        let data = data.clone();
        spawn_local(async move {
            let mut ws = WebSocket::open(&websocket).unwrap();
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Bytes(b)) => {
                        *data.borrow_mut() = Some(b);
                    }
                    _ => {
                        log!("ConstellationSinkDensity: WebSocket {:?}", msg);
                    }
                }
            }
            log!("ConstellationSinkDensity: WebSocket Closed");
        });
    }

    let canvas_ref = NodeRef::<Canvas>::new();
    Effect::new(move || {
        if let Some(canvas) = canvas_ref.get() {
            let context_options = js_sys::Object::new();
            js_sys::Reflect::set(
                &context_options,
                &"premultipliedAlpha".into(),
                &JsValue::FALSE,
            )
            .expect("Cannot create context options");

            let gl: GL = canvas
                .get_context_with_context_options("webgl2", &context_options)
                .unwrap()
                .unwrap()
                .dyn_into()
                .unwrap();

            let vert_code = r"
                attribute vec2 texCoord;
                varying vec2 coord;

                void main(void) {
                    gl_Position = vec4(texCoord, 0, 1);
                    coord = texCoord;
                }
            ";

            let vert_shader = gl.create_shader(GL::VERTEX_SHADER).unwrap();
            gl.shader_source(&vert_shader, vert_code);
            gl.compile_shader(&vert_shader);

            let frag_code = r"
                precision mediump float;

                varying vec2 coord;
                uniform sampler2D sampler;

                // Rainbow colormap: sky blue (low) -> cyan -> green -> yellow -> orange -> red (high)
                vec3 color_map(float t) {
                    // Sky blue to red rainbow gradient
                    // t=0: sky blue (0.53, 0.81, 0.92)
                    // t=0.2: cyan (0.0, 1.0, 1.0)
                    // t=0.4: green (0.0, 1.0, 0.0)
                    // t=0.6: yellow (1.0, 1.0, 0.0)
                    // t=0.8: orange (1.0, 0.5, 0.0)
                    // t=1.0: red (1.0, 0.0, 0.0)
                    
                    vec3 sky_blue = vec3(0.53, 0.81, 0.92);
                    vec3 cyan = vec3(0.0, 1.0, 1.0);
                    vec3 green = vec3(0.0, 1.0, 0.0);
                    vec3 yellow = vec3(1.0, 1.0, 0.0);
                    vec3 orange = vec3(1.0, 0.5, 0.0);
                    vec3 red = vec3(1.0, 0.0, 0.0);
                    
                    if (t < 0.2) {
                        return mix(sky_blue, cyan, t / 0.2);
                    } else if (t < 0.4) {
                        return mix(cyan, green, (t - 0.2) / 0.2);
                    } else if (t < 0.6) {
                        return mix(green, yellow, (t - 0.4) / 0.2);
                    } else if (t < 0.8) {
                        return mix(yellow, orange, (t - 0.6) / 0.2);
                    } else {
                        return mix(orange, red, (t - 0.8) / 0.2);
                    }
                }

                void main(void) {
                    vec4 sample = texture2D(sampler, vec2(coord.x * 0.5 + 0.5, coord.y * 0.5 - 0.5));
                    float value = clamp(sample.r, 0.0, 1.0);
                    // Solid color (alpha = 1.0) when there's any sample, black background otherwise
                    float alpha = value > 0.001 ? 1.0 : 0.0;
                    gl_FragColor = vec4(color_map(value), alpha);
                }
            ";

            let frag_shader = gl.create_shader(GL::FRAGMENT_SHADER).unwrap();
            gl.shader_source(&frag_shader, frag_code);
            gl.compile_shader(&frag_shader);

            let shader = gl.create_program().unwrap();
            gl.attach_shader(&shader, &vert_shader);
            gl.attach_shader(&shader, &frag_shader);
            gl.link_program(&shader);
            gl.use_program(Some(&shader));

            let texture = gl.create_texture().unwrap();
            gl.bind_texture(GL::TEXTURE_2D, Some(&texture));
            gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_WRAP_S, GL::REPEAT as i32);
            gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_WRAP_T, GL::REPEAT as i32);
            gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_MIN_FILTER, GL::NEAREST as i32);
            gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_MAG_FILTER, GL::NEAREST as i32);

            let texture = vec![0.0f32; bins * bins];
            let view = unsafe { f32::view(&texture) };
            gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_array_buffer_view_and_src_offset(
                GL::TEXTURE_2D,
                0,
                GL::R32F as i32,
                bins as i32,
                bins as i32,
                0,
                GL::RED,
                GL::FLOAT,
                &view,
                0
            ).unwrap();

            let vertexes = [-1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0];
            let vertex_buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(GL::ARRAY_BUFFER, Some(&vertex_buffer));
            let view = unsafe { f32::view(&vertexes) };
            gl.buffer_data_with_array_buffer_view(GL::ARRAY_BUFFER, &view, GL::STATIC_DRAW);

            let indices = [0, 1, 2, 0, 2, 3];
            let indices_buffer = gl.create_buffer().unwrap();
            gl.bind_buffer(GL::ELEMENT_ARRAY_BUFFER, Some(&indices_buffer));
            let view = unsafe { u16::view(&indices) };
            gl.buffer_data_with_array_buffer_view(GL::ELEMENT_ARRAY_BUFFER, &view, GL::STATIC_DRAW);

            let loc = gl.get_attrib_location(&shader, "texCoord") as u32;
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_with_i32(loc, 2, GL::FLOAT, false, 0, 0);

            let state = Rc::new(RefCell::new(RenderState {
                canvas,
                gl,
                texture,
                width,
                bins,
            }));
            request_animation_frame(render(state, data.clone(), decay, intensity))
        }
    });

    view! { <canvas node_ref=canvas_ref style="width: 100%; height: 100%" /> }
}

fn render(
    state: Rc<RefCell<RenderState>>,
    data: Rc<RefCell<Option<Vec<u8>>>>,
    decay: f32,
    intensity: f32,
) -> impl FnOnce() + 'static {
    move || {
        {
            let RenderState {
                canvas,
                gl,
                texture,
                width,
                bins,
            } = &mut (*state.borrow_mut());
            let bins = *bins;

            let display_width = canvas.client_width() as u32;
            let display_height = canvas.client_height() as u32;

            let need_resize = canvas.width() != display_width || canvas.height() != display_height;

            if need_resize {
                canvas.set_width(display_width);
                canvas.set_height(display_height);
                gl.viewport(0, 0, display_width as i32, display_height as i32);
            }

            if let Some(bytes) = data.borrow_mut().take() {
                let samples = unsafe {
                    let s = bytes.len() / 8;
                    let p = bytes.as_ptr();
                    std::slice::from_raw_parts(p as *const Complex32, s)
                };

                let decay_factor = decay.powi(samples.len() as i32);
                texture.iter_mut().for_each(|v| *v *= decay_factor);

                let width = width.get_untracked();
                for s in samples.iter() {
                    let w = ((s.re + width) / (2.0 * width) * bins as f32).round() as i64;
                    if w >= 0 && w < bins as i64 {
                        let h = ((s.im + width) / (2.0 * width) * bins as f32).round() as i64;
                        if h >= 0 && h < bins as i64 {
                            texture[h as usize * bins + w as usize] += intensity;
                        }
                    }
                }

                let view = unsafe { f32::view(texture) };
                gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_array_buffer_view_and_src_offset(
                    GL::TEXTURE_2D,
                    0,
                    0,
                    0,
                    bins as i32,
                    bins as i32,
                    GL::RED,
                    GL::FLOAT,
                    &view,
                    0,
                )
                .unwrap();

                gl.draw_elements_with_i32(GL::TRIANGLES, 6, GL::UNSIGNED_SHORT, 0);
            }
        }
        request_animation_frame(render(state, data, decay, intensity))
    }
}
