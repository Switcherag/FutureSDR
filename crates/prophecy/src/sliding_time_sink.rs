use leptos::html::Canvas;
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;
use std::cell::RefCell;
use std::rc::Rc;
use web_sys::HtmlCanvasElement;
use web_sys::WebGl2RenderingContext as GL;
use web_sys::WebGlProgram;

use crate::ArrayView;

struct RenderState {
    canvas: HtmlCanvasElement,
    gl: GL,
    shader: WebGlProgram,
    /// Current write row in the circular texture.
    texture_offset: i32,
    /// Total number of rows in the circular texture.
    rows: usize,
    /// When true, write every sample as its own row (full temporal resolution).
    expand: bool,
}

/// A vertical scrolling time-sink that draws a **line plot**.
///
/// Each incoming `Vec<u8>` frame (f32 samples) is reduced to one texture row.
/// The display scrolls downward, exactly like `Waterfall`, so placing them
/// side-by-side gives a shared, aligned time axis.
///
/// * `min` / `max` – value range mapped to the x axis (in **dB**; the
///   shader applies `10·log10(sample)`).
/// * `data` – a Leptos `ReadSignal<Vec<u8>>` carrying the raw f32 bytes from
///   a `WebsocketSinkBuilder` (the reducer picks the **mean** of each frame).
#[component]
pub fn SlidingTimeSink(
    #[prop(into)] min: Signal<f32>,
    #[prop(into)] max: Signal<f32>,
    /// Number of history rows. Default 256 (matches Waterfall's SHADER_HEIGHT).
    #[prop(default = 256)]
    rows: usize,
    /// When true, map samples linearly instead of applying 10·log10.
    #[prop(default = false)]
    linear: bool,
    /// When true, write every sample as its own texture row for full temporal
    /// resolution (like a Python time-domain plot).  When false (default),
    /// each frame is reduced to a single peak value.
    #[prop(default = false)]
    expand: bool,
    data: ReadSignal<Vec<u8>>,
) -> impl IntoView {
    let canvas_ref = NodeRef::<Canvas>::new();

    Effect::new(move || {
        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let gl: GL = canvas
            .get_context("webgl2")
            .unwrap()
            .unwrap()
            .dyn_into()
            .unwrap();

        // ── Shaders ────────────────────────────────────────────────────
        let vert_code = r"
            attribute vec2 gTexCoord0;
            varying vec2 coord;
            void main() {
                gl_Position = vec4(gTexCoord0, 0.0, 1.0);
                coord = gTexCoord0;
            }
        ";
        let vert_shader = gl.create_shader(GL::VERTEX_SHADER).unwrap();
        gl.shader_source(&vert_shader, vert_code);
        gl.compile_shader(&vert_shader);

        // Line-plot fragment shader:
        // For each fragment we sample the current row AND the neighbouring row,
        // compute normalised power for both, then check whether the fragment's
        // x position is close to the linearly-interpolated line segment between
        // them.  This gives a smooth, anti-aliased line instead of a histogram.
        let frag_code = r"
            precision mediump float;
            varying vec2 coord;
            uniform float u_min;
            uniform float u_max;
            uniform float yoffset;
            uniform float u_rows;    // HISTORY_ROWS as float
            uniform float u_linear;
            uniform sampler2D history_data;

            vec3 color_map(float t) {
                const vec3 c0 = vec3(0.2777273272234177, 0.005407344544966578, 0.3340998053353061);
                const vec3 c1 = vec3(0.1050930431085774, 1.404613529898575, 1.384590162594685);
                const vec3 c2 = vec3(-0.3308618287255563, 0.214847559468213, 0.09509516302823659);
                const vec3 c3 = vec3(-4.634230498983486, -5.799100973351585, -19.33244095627987);
                const vec3 c4 = vec3(6.228269936347081, 14.17993336680509, 56.69055260068105);
                const vec3 c5 = vec3(4.776384997670288, -13.74514537774601, -65.35303263337234);
                const vec3 c6 = vec3(-5.435455855934631, 4.645852612178535, 26.3124352495832);
                return c0+t*(c1+t*(c2+t*(c3+t*(c4+t*(c5+t*c6)))));
            }

            float to_power(float raw) {
                float val;
                if (u_linear > 0.5) {
                    val = raw;
                } else {
                    float safe = max(raw, 1.0e-20);
                    val = 10.0 * log(safe) / log(10.0);
                }
                return clamp((val - u_min) / (u_max - u_min), 0.0, 1.0);
            }

            void main() {
                float norm_x = coord.x * 0.5 + 0.5;  // [0,1] left-to-right

                // Use the same scrolling texture coord scheme as Waterfall
                float t0 = coord.y * 0.5 - 0.5 + yoffset;

                // One discrete bar per row — no interpolation between rows
                float line_x = to_power(texture2D(history_data, vec2(0.5, t0)).r);

                // Distance from the line, in normalised-x units
                float dist = abs(norm_x - line_x);

                // Line thickness: ~4% of width for good visibility
                float thickness = 8.0 / 200.0;

                // Core + glow: hard core at half-thickness, soft falloff beyond
                float core  = smoothstep(thickness, thickness * 0.3, dist);
                float alpha = core;

                vec3 line_col = color_map(line_x);
                vec3 bg = vec3(0.05, 0.05, 0.1);
                gl_FragColor = vec4(mix(bg, line_col, alpha), 1.0);
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

        // Set constant uniforms
        {
            let loc = gl.get_uniform_location(&shader, "u_rows");
            gl.uniform1f(loc.as_ref(), rows as f32);
            let loc = gl.get_uniform_location(&shader, "u_linear");
            gl.uniform1f(loc.as_ref(), if linear { 1.0 } else { 0.0 });
        }

        // ── Texture (1 × rows, R32F) ───────────────────────────────────
        let texture = gl.create_texture().unwrap();
        gl.bind_texture(GL::TEXTURE_2D, Some(&texture));
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_WRAP_S, GL::REPEAT as i32);
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_WRAP_T, GL::REPEAT as i32);
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_MIN_FILTER, GL::NEAREST as i32);
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_MAG_FILTER, GL::NEAREST as i32);

        let init = vec![0.0f32; rows];
        let view = unsafe { f32::view(&init) };
        gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_array_buffer_view_and_src_offset(
            GL::TEXTURE_2D,
            0,
            GL::R32F as i32,
            1,            // width  = 1 column
            rows as i32,  // height = time rows
            0,
            GL::RED,
            GL::FLOAT,
            &view,
            0,
        )
        .unwrap();

        // ── Full-screen quad ───────────────────────────────────────────
        let verts: [f32; 8] = [-1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0];
        let vb = gl.create_buffer().unwrap();
        gl.bind_buffer(GL::ARRAY_BUFFER, Some(&vb));
        let v = unsafe { f32::view(&verts) };
        gl.buffer_data_with_array_buffer_view(GL::ARRAY_BUFFER, &v, GL::STATIC_DRAW);

        let idx: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let ib = gl.create_buffer().unwrap();
        gl.bind_buffer(GL::ELEMENT_ARRAY_BUFFER, Some(&ib));
        let vi = unsafe { u16::view(&idx) };
        gl.buffer_data_with_array_buffer_view(GL::ELEMENT_ARRAY_BUFFER, &vi, GL::STATIC_DRAW);

        let loc = gl.get_attrib_location(&shader, "gTexCoord0") as u32;
        gl.enable_vertex_attrib_array(loc);
        gl.vertex_attrib_pointer_with_i32(loc, 2, GL::FLOAT, false, 0, 0);

        // ── Keep min/max reactive ──────────────────────────────────────
        {
            let gl = gl.clone();
            let shader = shader.clone();
            let _ = RenderEffect::new(move |_| {
                let u_min = gl.get_uniform_location(&shader, "u_min");
                gl.uniform1f(u_min.as_ref(), min.get());
                let u_max = gl.get_uniform_location(&shader, "u_max");
                gl.uniform1f(u_max.as_ref(), max.get());
            });
        }

        let state = Rc::new(RefCell::new(RenderState {
            canvas,
            gl,
            shader,
            texture_offset: 0,
            rows,
            expand,
        }));
        request_animation_frame(render(state, data));
    });

    view! { <canvas node_ref=canvas_ref style="width: 100%; height: 100%" /> }
}

/// A vertical scrolling time-sink that draws each data point as a **white dot**.
///
/// Identical scroll behaviour to `SlidingTimeSink`, but each frame is reduced to
/// a single peak value that is rendered as a small white circle instead of a
/// coloured interpolated curve.
///
/// * `min` / `max` – value range mapped to the x axis (linear or log10).
/// * `data` – a Leptos `ReadSignal<Vec<u8>>` carrying raw f32 bytes.
#[component]
pub fn SlidingTimeSinkDots(
    #[prop(into)] min: Signal<f32>,
    #[prop(into)] max: Signal<f32>,
    #[prop(default = 256)]
    rows: usize,
    #[prop(default = false)]
    linear: bool,
    data: ReadSignal<Vec<u8>>,
) -> impl IntoView {
    let canvas_ref = NodeRef::<Canvas>::new();

    Effect::new(move || {
        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let gl: GL = canvas
            .get_context("webgl2")
            .unwrap()
            .unwrap()
            .dyn_into()
            .unwrap();

        let vert_code = r"
            attribute vec2 gTexCoord0;
            varying vec2 coord;
            void main() {
                gl_Position = vec4(gTexCoord0, 0.0, 1.0);
                coord = gTexCoord0;
            }
        ";
        let vert_shader = gl.create_shader(GL::VERTEX_SHADER).unwrap();
        gl.shader_source(&vert_shader, vert_code);
        gl.compile_shader(&vert_shader);

        // Dot fragment shader: renders a white point at the stored x position.
        let frag_code = r"
            precision mediump float;
            varying vec2 coord;
            uniform float u_min;
            uniform float u_max;
            uniform float yoffset;
            uniform float u_rows;
            uniform float u_linear;
            uniform sampler2D history_data;

            float to_power(float raw) {
                float val;
                if (u_linear > 0.5) {
                    val = raw;
                } else {
                    float safe = max(raw, 1.0e-20);
                    val = 10.0 * log(safe) / log(10.0);
                }
                return clamp((val - u_min) / (u_max - u_min), 0.0, 1.0);
            }

            void main() {
                float norm_x = coord.x * 0.5 + 0.5;
                float t0 = coord.y * 0.5 - 0.5 + yoffset;

                float dot_x = to_power(texture2D(history_data, vec2(0.5, t0)).r);

                // Horizontal distance to the dot centre
                float dx = abs(norm_x - dot_x);
                // Vertical distance to the row centre (in normalised coords)
                float dt = 1.0 / u_rows;
                float row_centre = floor((coord.y * 0.5 + 0.5) / dt + 0.5) * dt;
                float dy = abs((coord.y * 0.5 + 0.5) - row_centre);

                // Dot radius: enlarged for better visibility.
                // Keep a stronger minimum on-screen size so sparse traces remain visible.
                float r = max(dt * 3.0, 0.016);
                float dist = length(vec2(dx, dy));

                float alpha = 1.0 - smoothstep(r * 0.30, r, dist);

                vec3 bg  = vec3(0.05, 0.05, 0.1);
                vec3 dot_col = vec3(1.0, 1.0, 1.0);
                gl_FragColor = vec4(mix(bg, dot_col, alpha), 1.0);
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

        {
            let loc = gl.get_uniform_location(&shader, "u_rows");
            gl.uniform1f(loc.as_ref(), rows as f32);
            let loc = gl.get_uniform_location(&shader, "u_linear");
            gl.uniform1f(loc.as_ref(), if linear { 1.0 } else { 0.0 });
        }

        let texture = gl.create_texture().unwrap();
        gl.bind_texture(GL::TEXTURE_2D, Some(&texture));
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_WRAP_S, GL::REPEAT as i32);
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_WRAP_T, GL::REPEAT as i32);
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_MIN_FILTER, GL::NEAREST as i32);
        gl.tex_parameteri(GL::TEXTURE_2D, GL::TEXTURE_MAG_FILTER, GL::NEAREST as i32);

        let init = vec![0.0f32; rows];
        let view = unsafe { f32::view(&init) };
        gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_array_buffer_view_and_src_offset(
            GL::TEXTURE_2D, 0, GL::R32F as i32, 1, rows as i32,
            0, GL::RED, GL::FLOAT, &view, 0,
        ).unwrap();

        let verts: [f32; 8] = [-1.0, -1.0, -1.0, 1.0, 1.0, 1.0, 1.0, -1.0];
        let vb = gl.create_buffer().unwrap();
        gl.bind_buffer(GL::ARRAY_BUFFER, Some(&vb));
        let v = unsafe { f32::view(&verts) };
        gl.buffer_data_with_array_buffer_view(GL::ARRAY_BUFFER, &v, GL::STATIC_DRAW);

        let idx: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let ib = gl.create_buffer().unwrap();
        gl.bind_buffer(GL::ELEMENT_ARRAY_BUFFER, Some(&ib));
        let vi = unsafe { u16::view(&idx) };
        gl.buffer_data_with_array_buffer_view(GL::ELEMENT_ARRAY_BUFFER, &vi, GL::STATIC_DRAW);

        let loc = gl.get_attrib_location(&shader, "gTexCoord0") as u32;
        gl.enable_vertex_attrib_array(loc);
        gl.vertex_attrib_pointer_with_i32(loc, 2, GL::FLOAT, false, 0, 0);

        {
            let gl = gl.clone();
            let shader = shader.clone();
            let _ = RenderEffect::new(move |_| {
                let u_min = gl.get_uniform_location(&shader, "u_min");
                gl.uniform1f(u_min.as_ref(), min.get());
                let u_max = gl.get_uniform_location(&shader, "u_max");
                gl.uniform1f(u_max.as_ref(), max.get());
            });
        }

        let state = Rc::new(RefCell::new(RenderState {
            canvas,
            gl,
            shader,
            texture_offset: 0,
            rows,
            expand: false,
        }));
        request_animation_frame(render(state, data));
    });

    view! { <canvas node_ref=canvas_ref style="width: 100%; height: 100%" /> }
}

fn render(state: Rc<RefCell<RenderState>>, data: ReadSignal<Vec<u8>>) -> impl FnOnce() + 'static {
    move || {
        {
            let RenderState {
                canvas,
                gl,
                shader,
                texture_offset,
                rows,
                expand,
            } = &mut *state.borrow_mut();

            // Resize canvas to CSS size
            let dw = canvas.client_width() as u32;
            let dh = canvas.client_height() as u32;
            if canvas.width() != dw || canvas.height() != dh {
                canvas.set_width(dw);
                canvas.set_height(dh);
                gl.viewport(0, 0, dw as i32, dh as i32);
            }

            let bytes = &*data.read_untracked();
            if !bytes.is_empty() {
                let n_floats = bytes.len() / 4;
                let samples = unsafe {
                    std::slice::from_raw_parts(bytes.as_ptr() as *const f32, n_floats)
                };

                if *expand {
                    // Write every sample as its own texture row (full temporal
                    // resolution, matching a Python-style time-domain plot).
                    let n = n_floats as i32;
                    let remaining = *rows as i32 - *texture_offset;
                    if n <= remaining {
                        // Fits without wrapping.
                        let view = unsafe { f32::view(samples) };
                        gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_array_buffer_view_and_src_offset(
                            GL::TEXTURE_2D, 0, 0, *texture_offset, 1, n,
                            GL::RED, GL::FLOAT, &view, 0,
                        ).unwrap();
                    } else {
                        // Two writes: end of texture, then wrap to start.
                        let first = remaining as usize;
                        let view1 = unsafe { f32::view(&samples[..first]) };
                        gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_array_buffer_view_and_src_offset(
                            GL::TEXTURE_2D, 0, 0, *texture_offset, 1, remaining,
                            GL::RED, GL::FLOAT, &view1, 0,
                        ).unwrap();
                        let second = (n_floats - first) as i32;
                        let view2 = unsafe { f32::view(&samples[first..]) };
                        gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_array_buffer_view_and_src_offset(
                            GL::TEXTURE_2D, 0, 0, 0, 1, second,
                            GL::RED, GL::FLOAT, &view2, 0,
                        ).unwrap();
                    }
                    *texture_offset = (*texture_offset + n) % *rows as i32;
                } else {
                    // Reduce the incoming frame to a single peak value.
                    let peak: f32 = if n_floats > 0 {
                        samples.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                    } else {
                        0.0
                    };
                    let pixel = [peak];
                    let view = unsafe { f32::view(&pixel) };
                    gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_array_buffer_view_and_src_offset(
                        GL::TEXTURE_2D, 0, 0, *texture_offset, 1, 1,
                        GL::RED, GL::FLOAT, &view, 0,
                    ).unwrap();
                    *texture_offset = (*texture_offset + 1) % *rows as i32;
                }

                let loc = gl.get_uniform_location(shader, "yoffset");
                gl.uniform1f(loc.as_ref(), *texture_offset as f32 / *rows as f32);
            }

            gl.draw_elements_with_i32(GL::TRIANGLES, 6, GL::UNSIGNED_SHORT, 0);
        }
        request_animation_frame(render(state, data));
    }
}
