use crate::custom_event::{FolderBrowserSelection, RuffleEvent};
use crate::gui::theme::ThemePreference;
use crate::gui::{text, text_with_args};
use crate::preferences::GlobalPreferences;
use egui::{Context, FontDefinitions, ScrollArea, Ui, ViewportId};
use fluent_templates::fluent_bundle::FluentValue;
use ruffle_frontend_utils::content::ContentDescriptor;
use ruffle_render_wgpu::descriptors::Descriptors;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use walkdir::WalkDir;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::{Icon, Theme, Window, WindowAttributes, WindowId};

const PLAYABLE_EXTENSIONS: &[&str] = &["swf", "spl", "ruf"];

/// Independent OS window listing playable files in a directory.
pub struct FolderBrowserWindow {
    window: Arc<Window>,
    egui_winit: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    size: PhysicalSize<u32>,
    descriptors: Arc<Descriptors>,
    directory: PathBuf,
    files: Vec<(PathBuf, String)>,
    selected: Option<PathBuf>,
    selection: FolderBrowserSelection,
    event_loop: EventLoopProxy<RuffleEvent>,
    preferences: GlobalPreferences,
    last_update: Instant,
    repaint_after: Duration,
    should_close: bool,
}

impl FolderBrowserWindow {
    pub fn create(
        event_loop: &ActiveEventLoop,
        descriptors: Arc<Descriptors>,
        directory: PathBuf,
        selection: FolderBrowserSelection,
        proxy: EventLoopProxy<RuffleEvent>,
        preferences: GlobalPreferences,
        fonts: FontDefinitions,
    ) -> anyhow::Result<Self> {
        let locale = preferences.language();
        let dir_name = directory
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| directory.to_string_lossy().into_owned());
        let mut args = HashMap::new();
        args.insert(Cow::Borrowed("path"), FluentValue::from(dir_name));
        let title = text_with_args(&locale, "folder-browser-title", &args).into_owned();

        let icon_bytes = include_bytes!("../../assets/favicon-32.rgba");
        let icon =
            Icon::from_rgba(icon_bytes.to_vec(), 32, 32).expect("App icon should be correct");

        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut window_attributes = WindowAttributes::default()
            .with_title(title)
            .with_window_icon(Some(icon))
            .with_inner_size(LogicalSize::new(480.0, 560.0))
            .with_min_inner_size(LogicalSize::new(320.0, 240.0));

        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            window_attributes = window_attributes.with_name("rs.ruffle.Ruffle", "folder-browser");
        }

        let window = Arc::new(event_loop.create_window(window_attributes)?);
        let size = window.inner_size();

        let surface = descriptors
            .wgpu_instance
            .create_surface(window.clone())
            .map_err(|e| anyhow::anyhow!("Failed to create folder browser surface: {e}"))?;

        let preferred_formats = [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ];
        let supported_formats = surface.get_capabilities(&descriptors.adapter).formats;
        let surface_format = preferred_formats
            .iter()
            .find(|format| supported_formats.contains(format))
            .copied()
            .or_else(|| supported_formats.first().copied())
            .ok_or_else(|| anyhow::anyhow!("Folder browser surface has no texture formats"))?;

        surface.configure(
            &descriptors.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                color_space: wgpu::SurfaceColorSpace::Auto,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: Default::default(),
                desired_maximum_frame_latency: 2,
                alpha_mode: Default::default(),
                view_formats: Default::default(),
            },
        );

        let egui_ctx = Context::default();
        egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        egui_ctx.set_fonts(fonts);
        apply_theme(&egui_ctx, &preferences, window.theme());

        let mut egui_winit = egui_winit::State::new(
            egui_ctx,
            ViewportId::from_hash_of("folder_browser"),
            window.as_ref(),
            None,
            None,
            None,
        );
        egui_winit.set_max_texture_side(descriptors.limits.max_texture_dimension_2d as usize);

        let egui_renderer = egui_wgpu::Renderer::new(
            &descriptors.device,
            surface_format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                predictable_texture_filtering: false,
            },
        );

        let files = collect_playable_files(&directory);

        let browser = Self {
            window,
            egui_winit,
            egui_renderer,
            surface,
            surface_format,
            size,
            descriptors,
            directory,
            files,
            selected: None,
            selection,
            event_loop: proxy,
            preferences,
            last_update: Instant::now(),
            repaint_after: Duration::ZERO,
            should_close: false,
        };
        browser.window.request_redraw();
        Ok(browser)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn needs_render(&self) -> bool {
        Instant::now().duration_since(self.last_update) >= self.repaint_after
    }

    pub fn request_redraw_if_needed(&self) {
        if self.needs_render() {
            self.window.request_redraw();
        }
    }

    /// Handle a window event. Returns `true` when the browser should be closed.
    pub fn handle_event(&mut self, event: &WindowEvent) -> bool {
        if matches!(event, WindowEvent::CloseRequested) {
            self.should_close = true;
            return true;
        }

        if let WindowEvent::Resized(size) = event {
            self.resize(*size);
        }

        if let WindowEvent::ThemeChanged(theme) = event {
            apply_theme(
                self.egui_winit.egui_ctx(),
                &self.preferences,
                Some(*theme),
            );
            self.window.request_redraw();
        }

        let response = self.egui_winit.on_window_event(&self.window, event);
        if response.repaint {
            self.window.request_redraw();
        }

        self.should_close
    }

    pub fn render(&mut self) {
        if self.size.width == 0 || self.size.height == 0 {
            return;
        }

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(surface_texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(surface_texture) => surface_texture,
            state @ (wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Outdated) => {
                tracing::warn!(
                    "Folder browser surface unavailable: {:?}, reconfiguring",
                    state
                );
                self.reconfigure_surface();
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                tracing::warn!("Folder browser surface acquisition timed out");
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Validation => {
                panic!("wgpu: Folder browser surface validation error");
            }
        };

        let raw_input = self.egui_winit.take_egui_input(&self.window);
        let mut full_output = {
            // Split borrows: `run_ui` needs `&egui_winit` via `egui_ctx()`, while
            // the UI callback mutates the rest of `self`.
            let egui_winit = &mut self.egui_winit;
            let directory = &self.directory;
            let files = &self.files;
            let selected = &mut self.selected;
            let should_close = &mut self.should_close;
            let selection = &self.selection;
            let event_loop = &self.event_loop;
            let preferences = &self.preferences;

            egui_winit.egui_ctx().run_ui(raw_input, |ui| {
                render_folder_browser_ui(
                    ui,
                    directory,
                    files,
                    selected,
                    should_close,
                    selection,
                    event_loop,
                    preferences,
                );
            })
        };

        self.repaint_after = full_output
            .viewport_output
            .get(&ViewportId::from_hash_of("folder_browser"))
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::MAX);
        self.last_update = Instant::now();

        self.egui_winit
            .handle_platform_output(&self.window, full_output.platform_output);

        let clipped_primitives = self
            .egui_winit
            .egui_ctx()
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        let scale_factor = self.window.scale_factor() as f32;
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.size.width, self.size.height],
            pixels_per_point: scale_factor,
        };

        let mut encoder =
            self.descriptors
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("folder browser egui encoder"),
                });

        for (id, image_deltas) in full_output.textures_delta.set.drain() {
            for image_delta in &image_deltas {
                self.egui_renderer.update_texture(
                    &self.descriptors.device,
                    &self.descriptors.queue,
                    id,
                    image_delta,
                );
            }
        }

        let mut command_buffers = self.egui_renderer.update_buffers(
            &self.descriptors.device,
            &self.descriptors.queue,
            &mut encoder,
            &clipped_primitives,
            &screen_descriptor,
        );

        {
            let surface_view = surface_texture.texture.create_view(&Default::default());
            let mut render_pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.1,
                                g: 0.1,
                                b: 0.1,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    label: Some("folder_browser_egui_render"),
                    ..Default::default()
                })
                .forget_lifetime();

            self.egui_renderer
                .render(&mut render_pass, &clipped_primitives, &screen_descriptor);
        }

        command_buffers.push(encoder.finish());
        self.descriptors.queue.submit(command_buffers);

        for id in full_output.textures_delta.free.drain() {
            self.egui_renderer.free_texture(&id);
        }

        self.window.pre_present_notify();
        self.descriptors.queue.present(surface_texture);
    }

    pub fn should_close(&self) -> bool {
        self.should_close
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.size = size;
            self.reconfigure_surface();
        } else {
            self.size = size;
        }
    }

    fn reconfigure_surface(&self) {
        if self.size.width == 0 || self.size.height == 0 {
            return;
        }
        self.surface.configure(
            &self.descriptors.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.surface_format,
                color_space: wgpu::SurfaceColorSpace::Auto,
                width: self.size.width,
                height: self.size.height,
                present_mode: Default::default(),
                desired_maximum_frame_latency: 2,
                alpha_mode: Default::default(),
                view_formats: Default::default(),
            },
        );
    }
}

fn render_folder_browser_ui(
    ui: &mut Ui,
    directory: &Path,
    files: &[(PathBuf, String)],
    selected: &mut Option<PathBuf>,
    should_close: &mut bool,
    selection: &FolderBrowserSelection,
    event_loop: &EventLoopProxy<RuffleEvent>,
    preferences: &GlobalPreferences,
) {
    let locale = preferences.language();
    let mut open_path: Option<PathBuf> = None;

    egui::CentralPanel::default().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading(text(&locale, "folder-browser-heading"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(text(&locale, "folder-browser-close")).clicked() {
                    *should_close = true;
                }
            });
        });
        ui.label(directory.to_string_lossy().as_ref());
        ui.separator();

        if files.is_empty() {
            ui.label(text(&locale, "folder-browser-empty"));
            return;
        }

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.with_layout(ui.layout().with_cross_justify(true), |ui| {
                    for (path, label) in files {
                        let is_selected = selected.as_ref() == Some(path);
                        if ui.selectable_label(is_selected, label.as_str()).clicked() {
                            open_path = Some(path.clone());
                        }
                    }
                });
            });
    });

    if let Some(path) = open_path {
        *selected = Some(path.clone());
        match selection {
            FolderBrowserSelection::Open(launch_options) => {
                if let Some(desc) =
                    ContentDescriptor::new_local(&path, Some(directory.to_path_buf()))
                {
                    let _ = event_loop
                        .send_event(RuffleEvent::Open(desc, launch_options.clone()));
                } else {
                    tracing::warn!(
                        "Could not open selected folder browser file: {}",
                        path.display()
                    );
                }
            }
            FolderBrowserSelection::FillField(handle) => {
                handle.set_from_directory_selection(directory.to_path_buf(), path);
            }
        }
    }
}

fn collect_playable_files(directory: &Path) -> Vec<(PathBuf, String)> {
    WalkDir::new(directory)
        .sort_by(|a, b| compare_paths(a.path(), b.path()))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| {
                    PLAYABLE_EXTENSIONS
                        .iter()
                        .any(|allowed| ext.eq_ignore_ascii_case(allowed))
                })
        })
        .filter_map(|e| {
            let path = e.path().to_path_buf();
            let relative = path.strip_prefix(directory).ok()?;
            let label = relative.to_string_lossy().to_string();
            Some((path, label))
        })
        .collect()
}

fn compare_paths(a: &Path, b: &Path) -> Ordering {
    match (a.is_dir(), b.is_dir()) {
        (true, true) | (false, false) => a
            .to_string_lossy()
            .to_lowercase()
            .cmp(&b.to_string_lossy().to_lowercase()),
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
    }
}

fn apply_theme(egui_ctx: &Context, preferences: &GlobalPreferences, window_theme: Option<Theme>) {
    let dark = match preferences.theme_preference() {
        ThemePreference::Dark => true,
        ThemePreference::Light => false,
        ThemePreference::System => !matches!(window_theme, Some(Theme::Light)),
    };
    egui_ctx.set_visuals(if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    });
}
