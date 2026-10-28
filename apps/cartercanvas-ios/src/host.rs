//! Runs `PhotocraftApp` on the host's `CAMetalLayer`: one egui pass per display-link tick when
//! something changed, painted with egui-wgpu.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::App as _;
use eframe::egui_wgpu::{self, RenderState, WgpuConfiguration, WgpuSetup};
use eframe::wgpu;
use egui::{Pos2, Rect, ViewportId, ViewportInfo, vec2};
use photocraft_engine::Session;
use photocraft_ui_egui::theme::ThemeKind;
use photocraft_ui_egui::{FileDialogAnswer, PhotocraftApp};

use crate::callbacks::{Callbacks, DialogKind};
use crate::input::InputState;
use crate::services::{self, Dirs, OsInbox, PendingDialogs};

/// The view's size and scale, from the host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width_px: u32,
    pub height_px: u32,
    /// Device pixels per point (`UIScreen.scale`, or the layer's `contentsScale`).
    pub scale: f32,
    /// Safe-area insets in points (notch, home indicator, rounded corners).
    pub insets: [f32; 4],
}

impl Viewport {
    fn size_points(&self) -> egui::Vec2 {
        let s = if self.scale.is_finite() && self.scale > 0.0 { self.scale } else { 1.0 };
        vec2(self.width_px as f32 / s, self.height_px as f32 / s)
    }
}

/// Wakeups asked for between frames, from any thread (background jobs finishing, timers).
#[derive(Default)]
struct Wakeup {
    at: Option<Instant>,
}

/// What a frame did, for the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameResult {
    /// A frame was drawn.
    pub drew: bool,
    /// A text field has keyboard focus, so the on-screen keyboard should show.
    pub wants_keyboard: bool,
}

pub struct Host {
    ctx: egui::Context,
    app: PhotocraftApp,
    frame: eframe::Frame,
    layer: *mut c_void,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    rs: RenderState,
    viewport: Viewport,
    input: InputState,
    epoch: Option<f64>,
    wakeup: Arc<Mutex<Wakeup>>,
    needs_frame: bool,
    callbacks: Callbacks,
    dialogs: PendingDialogs,
    inbox: OsInbox,
    keyboard_visible: bool,
    focused: bool,
    surface_lost: bool,
    /// Screenshot requests waiting for a frame that could be drawn.
    screenshots: Vec<egui::ScreenshotCallback>,
    /// Folders the engine asked to show in the Files app (Reveal).
    shell_urls: std::sync::mpsc::Receiver<String>,
}

impl Host {
    /// Set up wgpu on `layer`, the engine, and the app. `layer` must be a `CAMetalLayer` that
    /// outlives the host.
    /// `control` turns on the localhost JSON-lines control server (port, token) for automation.
    pub fn new(layer: *mut c_void, viewport: Viewport, dirs: Dirs, callbacks: Callbacks, control: Option<(u16, String)>) -> Result<Self, String> {
        photocraft_ui_egui::type_tool::warm_fonts();
        let mut config = WgpuConfiguration::default();
        if let WgpuSetup::CreateNew(create) = &mut config.wgpu_setup {
            create.instance_descriptor.backends = wgpu::Backends::METAL;
            create.power_preference = wgpu::PowerPreference::HighPerformance;
        }
        photocraft_ui_egui::gpu_canvas::use_adapter_limits(&mut config.wgpu_setup);
        let instance = pollster::block_on(config.wgpu_setup.new_instance());
        let surface = crate::ffi::surface_from_layer(&instance, layer)?;
        let rs = pollster::block_on(RenderState::create(&config, &instance, Some(&surface), egui_wgpu::RendererOptions::default())).map_err(|e| e.to_string())?;
        let surface_config = surface_configuration(&surface, &rs, viewport)?;
        surface.configure(&rs.device, &surface_config);
        log::info!("wgpu adapter: {:?}, surface format {:?}", rs.adapter.get_info().name, rs.target_format);

        let ctx = egui::Context::default();
        ctx.set_embed_viewports(true);
        let wakeup: Arc<Mutex<Wakeup>> = Arc::default();
        let wake = wakeup.clone();
        ctx.set_request_repaint_callback(move |info| {
            let at = Instant::now() + info.delay.min(Duration::from_secs(3600));
            let mut w = wake.lock().unwrap_or_else(PoisonError::into_inner);
            w.at = Some(w.at.map_or(at, |old| old.min(at)));
        });

        let dialogs = PendingDialogs::default();
        let inbox = OsInbox::default();
        let services = services::ios(&callbacks, &dirs, &dialogs, &inbox);
        PhotocraftApp::install_type_reset_hooks(&ctx);
        PhotocraftApp::setup_context(&ctx, ThemeKind::Pro);
        let mut app = PhotocraftApp::new(Session::new(), services);
        app.background_jobs = true;
        app.set_theme(&ctx, ThemeKind::Pro);
        app.perf.gpu_info.set_adapter(&rs.adapter.get_info());
        app.perf.gpu_info.preference = "metal".into();
        app.perf.gpu_info.selected = "metal".into();
        app.set_wgpu(rs.clone());
        if let Some((port, token)) = control {
            app = app.with_control(crate::control_server::start(port, token, ctx.clone()));
        }

        let mut frame = eframe::Frame::_new_kittest();
        frame.wgpu_render_state = Some(rs.clone());
        let input = InputState::new(app.stylus.feed.clone());
        Ok(Self {
            ctx,
            app,
            frame,
            layer,
            surface,
            surface_config,
            rs,
            viewport,
            input,
            epoch: None,
            wakeup,
            needs_frame: true,
            callbacks,
            dialogs,
            inbox,
            keyboard_visible: false,
            focused: true,
            surface_lost: false,
            screenshots: Vec::new(),
            shell_urls: photocraft_engine::shell_open::connect(),
        })
    }

    pub fn input(&mut self) -> &mut InputState {
        self.needs_frame = true;
        &mut self.input
    }

    pub fn resize(&mut self, viewport: Viewport) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.surface_config.width = viewport.width_px.max(1);
            self.surface_config.height = viewport.height_px.max(1);
            self.surface.configure(&self.rs.device, &self.surface_config);
            self.needs_frame = true;
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.needs_frame = true;
    }

    /// The host's answer to file dialog `id`: chosen paths, or `None` for Cancel.
    pub fn answer_dialog(&mut self, id: u64, kind: DialogKind, paths: Option<Vec<String>>) {
        self.dialogs.answer(id, services::dialog_answer(kind, paths));
        self.needs_frame = true;
    }

    /// A file dialog the host couldn't show.
    pub fn dialog_failed(&mut self, id: u64, message: String) {
        self.dialogs.answer(id, Some(FileDialogAnswer::Unavailable(message)));
        self.needs_frame = true;
    }

    /// Files the OS handed us (Files app, share sheet).
    pub fn open_paths(&mut self, paths: Vec<String>) {
        self.inbox.open(paths);
        self.ctx.request_repaint();
        self.needs_frame = true;
    }

    pub fn has_unsaved_work(&self) -> bool {
        self.app.has_unsaved_work()
    }

    /// The app is going to the background: finish pending autosaves so a kill loses nothing.
    pub fn will_background(&mut self) {
        if let Some(flush) = self.app.services.flush_autosaves.as_mut() {
            let done = flush();
            log::info!("background: {} autosave(s) flushed", done.len());
        }
        self.focused = false;
        self.needs_frame = true;
    }

    /// Memory is short: drop what can be rebuilt (GPU caches, thumbnails, egui's text caches).
    pub fn memory_warning(&mut self) {
        log::warn!("memory warning");
        self.ctx.memory_mut(|m| m.caches = Default::default());
        self.ctx.forget_all_images();
        self.ctx.request_repaint();
        self.needs_frame = true;
    }

    pub fn shutdown(&mut self) {
        self.app.on_exit();
    }

    fn due(&mut self, now: Instant) -> bool {
        let mut w = self.wakeup.lock().unwrap_or_else(PoisonError::into_inner);
        match w.at {
            Some(at) if at <= now => {
                w.at = None;
                true
            }
            _ => false,
        }
    }

    /// One display-link tick at `timestamp` (seconds, `CACurrentMediaTime`, the clock of
    /// `UITouch.timestamp`). Runs an egui pass and paints only when something changed.
    pub fn frame(&mut self, timestamp: f64) -> FrameResult {
        let now = Instant::now();
        let due = self.due(now);
        if !(self.needs_frame || due || self.input.waiting() || self.surface_lost) {
            return FrameResult { drew: false, wants_keyboard: self.keyboard_visible };
        }
        self.needs_frame = false;
        if self.surface_lost {
            self.recreate_surface();
        }
        let epoch = *self.epoch.get_or_insert(timestamp);
        let mut raw = self.raw_input(timestamp, timestamp - epoch);
        self.app.raw_input_hook(&self.ctx, &mut raw);
        let ctx = self.ctx.clone();
        let (app, frame) = (&mut self.app, &mut self.frame);
        let output = ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx(), frame);
            app.ui(ui, frame);
        });
        self.platform_output(&output.platform_output);
        let mut screenshots = std::mem::take(&mut self.screenshots);
        for vp in output.viewport_output.values() {
            for command in &vp.commands {
                match command {
                    egui::ViewportCommand::Screenshot(callback) => screenshots.push(callback.clone()),
                    egui::ViewportCommand::RequestPaste => self.callbacks.request_paste(),
                    _ => {}
                }
            }
        }
        if let Some(vp) = output.viewport_output.get(&ViewportId::ROOT) {
            if vp.repaint_delay.is_zero() {
                self.needs_frame = true;
            } else if vp.repaint_delay < Duration::from_secs(3600) {
                let at = now + vp.repaint_delay;
                let mut w = self.wakeup.lock().unwrap_or_else(PoisonError::into_inner);
                w.at = Some(w.at.map_or(at, |old| old.min(at)));
            }
        }
        while let Ok(url) = self.shell_urls.try_recv() {
            if let Err(e) = self.callbacks.open_url(&url) {
                log::warn!("show in Files: {e}");
            }
        }
        for kind in std::mem::take(&mut self.input.haptics) {
            self.callbacks.haptic(kind);
        }
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let mut textures = output.textures_delta;
        self.paint(output.pixels_per_point, &primitives, &mut textures, screenshots);
        FrameResult { drew: true, wants_keyboard: self.keyboard_visible }
    }

    fn raw_input(&mut self, now: f64, time: f64) -> egui::RawInput {
        let size = self.viewport.size_points();
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let mut raw = egui::RawInput { screen_rect: Some(rect), ..Default::default() };
        let [top, left, bottom, right] = self.viewport.insets;
        raw.safe_area_insets = Some(egui::SafeAreaInsets(egui::epaint::MarginF32 { left, right, top, bottom }));
        raw.viewports.insert(
            ViewportId::ROOT,
            ViewportInfo {
                native_pixels_per_point: Some(self.viewport.scale),
                inner_rect: Some(rect),
                outer_rect: Some(rect),
                monitor_size: Some(size),
                focused: Some(self.focused),
                fullscreen: Some(true),
                ..Default::default()
            },
        );
        raw.max_texture_side = Some(self.rs.device.limits().max_texture_dimension_2d as usize);
        raw.time = Some(time);
        raw.predicted_dt = 1.0 / 120.0;
        raw.events = self.input.take_events(now);
        raw.focused = self.focused;
        raw.system_theme = Some(egui::Theme::Dark);
        raw
    }

    fn platform_output(&mut self, out: &egui::PlatformOutput) {
        for command in &out.commands {
            match command {
                egui::OutputCommand::CopyText(text) => self.callbacks.clipboard_set_text(text),
                egui::OutputCommand::CopyImage(image) => {
                    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                    if let Err(e) = self.callbacks.clipboard_set_image(image.size[0] as u32, image.size[1] as u32, &rgba) {
                        log::warn!("copy image: {e}");
                    }
                }
                egui::OutputCommand::OpenUrl(open) => {
                    if let Err(e) = self.callbacks.open_url(&open.url) {
                        log::warn!("open {}: {e}", open.url);
                    }
                }
                egui::OutputCommand::TextSelectionSettled(_) => {}
            }
        }
        let wants = out.ime.is_some();
        if wants != self.keyboard_visible {
            self.keyboard_visible = wants;
            self.callbacks.set_keyboard_visible(wants);
        }
    }

    fn recreate_surface(&mut self) {
        match crate::ffi::surface_from_layer(&self.rs.instance, self.layer) {
            Ok(surface) => {
                surface.configure(&self.rs.device, &self.surface_config);
                self.surface = surface;
                self.surface_lost = false;
            }
            Err(e) => log::error!("recreate surface: {e}"),
        }
    }

    fn paint(&mut self, ppp: f32, primitives: &[egui::ClippedPrimitive], textures: &mut egui::TexturesDelta, screenshots: Vec<egui::ScreenshotCallback>) {
        let rs = &self.rs;
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [self.surface_config.width, self.surface_config.height], pixels_per_point: ppp };
        let mut encoder = rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("cartercanvas frame") });
        let user_buffers = {
            let mut renderer = rs.renderer.write();
            for (id, deltas) in textures.set.drain() {
                for delta in deltas {
                    renderer.update_texture(&rs.device, &rs.queue, id, &delta);
                }
            }
            renderer.update_buffers(&rs.device, &rs.queue, &mut encoder, primitives, &screen)
        };
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => Some(f),
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                self.surface.configure(&rs.device, &self.surface_config);
                Some(f)
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface_lost = true;
                None
            }
            other => {
                log::warn!("surface: {other:?}");
                self.surface.configure(&rs.device, &self.surface_config);
                self.needs_frame = true;
                None
            }
        };
        let Some(frame) = frame else {
            rs.queue.submit(user_buffers.into_iter().chain([encoder.finish()]));
            self.screenshots.extend(screenshots);
            self.needs_frame = true;
            return;
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let renderer = rs.renderer.read();
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.035, g: 0.016, b: 0.024, a: 1.0 }), store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass.forget_lifetime(), primitives, &screen);
        }
        let can_copy = self.surface_config.usage.contains(wgpu::TextureUsages::COPY_SRC);
        if !screenshots.is_empty() && !can_copy {
            log::error!("screenshot: this GPU can't copy the screen");
        }
        let readback = (!screenshots.is_empty() && can_copy).then(|| Readback::copy(&rs.device, &mut encoder, &frame.texture));
        rs.queue.submit(user_buffers.into_iter().chain([encoder.finish()]));
        if let Some(readback) = readback {
            match readback.image(&rs.device, frame.texture.format()) {
                Ok(image) => {
                    let image = Arc::new(image);
                    for callback in screenshots {
                        callback.complete(image.clone());
                    }
                    self.ctx.request_repaint();
                }
                Err(e) => log::error!("screenshot: {e}"),
            }
        }
        rs.queue.present(frame);
        let mut renderer = rs.renderer.write();
        for id in textures.free.drain() {
            renderer.free_texture(&id);
        }
    }
}

fn surface_configuration(surface: &wgpu::Surface<'_>, rs: &RenderState, viewport: Viewport) -> Result<wgpu::SurfaceConfiguration, String> {
    let mut config = surface
        .get_default_config(&rs.adapter, viewport.width_px.max(1), viewport.height_px.max(1))
        .ok_or("the Metal layer isn't supported by this GPU adapter")?;
    config.format = rs.target_format;
    config.view_formats = vec![rs.target_format];
    config.present_mode = wgpu::PresentMode::Fifo;
    config.desired_maximum_frame_latency = 1;
    // Screenshots (the control channel's, View › Screenshot) copy the presented frame.
    let caps = surface.get_capabilities(&rs.adapter);
    config.usage = wgpu::TextureUsages::RENDER_ATTACHMENT | (caps.usages & wgpu::TextureUsages::COPY_SRC);
    Ok(config)
}

/// A frame copied into a mappable buffer, rows padded to wgpu's copy alignment.
struct Readback {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_row: u32,
}

impl Readback {
    fn copy(device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) -> Self {
        let (width, height) = (texture.width(), texture.height());
        let padded_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_row), rows_per_image: Some(height) },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        Self { buffer, width, height, padded_row }
    }

    /// Wait for the copy and convert it to an egui image (the surface is BGRA or RGBA, 8 bits).
    fn image(self, device: &wgpu::Device, format: wgpu::TextureFormat) -> Result<egui::ColorImage, String> {
        let slice = self.buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        let bgra = matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
        let mut rgba = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        {
            let data = slice.get_mapped_range().map_err(|e| e.to_string())?;
            for row in data.chunks_exact(self.padded_row as usize) {
                for px in row[..self.width as usize * 4].chunks_exact(4) {
                    if bgra {
                        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                    } else {
                        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                    }
                }
            }
        }
        self.buffer.unmap();
        Ok(egui::ColorImage::from_rgba_unmultiplied([self.width as usize, self.height as usize], &rgba))
    }
}
