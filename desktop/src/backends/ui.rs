use crate::cli::OpenUrlMode;
use crate::custom_event::RuffleEvent;
use crate::gui::dialogs::message_dialog::MessageDialogConfiguration;
use crate::gui::{DialogDescriptor, FilePicker, LocalizableText};
use crate::preferences::GlobalPreferences;
use anyhow::{Error, Result, anyhow};
use chrono::{DateTime, Utc};
use egui_winit::clipboard::Clipboard;
use fontdb::{FaceInfo, Family};
use rfd::{
    AsyncFileDialog, FileHandle, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel,
};
use ruffle_core::backend::ui::{
    DialogResultFuture, FileDialogResult, FileDialogSelection, FileFilter, FontDefinition,
    FullscreenError, LanguageIdentifier, MouseCursor, MultiDialogResultFuture,
    MultiFileDialogResult, UiBackend,
};
use ruffle_core::font::{FontAtlases, FontFileData, FontQuery};
use std::fs::File;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use url::Url;
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::HasDisplayHandle;
use winit::window::{Fullscreen, Window};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceFontRenderer {
    Embedded,
    Freetype,
}

impl DeviceFontRenderer {
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceFontRenderer::Embedded => "embedded",
            DeviceFontRenderer::Freetype => "freetype",
        }
    }
}

impl FromStr for DeviceFontRenderer {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "embedded" => Ok(DeviceFontRenderer::Embedded),
            "freetype" => Ok(DeviceFontRenderer::Freetype),
            _ => Err(()),
        }
    }
}

pub struct DesktopFileSelection {
    handle: FileHandle,
    md: Option<std::fs::Metadata>,
    contents: Vec<u8>,
}

impl DesktopFileSelection {
    /// Create a new [`DesktopFileSelection`] from a given file handle.
    pub async fn new(handle: FileHandle) -> Self {
        async fn read_file(path: &Path) -> (Option<std::fs::Metadata>, Vec<u8>) {
            let file = match tokio::fs::File::open(path).await {
                Ok(file) => file,
                Err(err) => {
                    let path = path.to_string_lossy();
                    tracing::error!("Error opening file {path}: {err}");
                    return (None, Vec::new());
                }
            };
            let metadata = match file.metadata().await {
                Ok(metadata) => Some(metadata),
                Err(err) => {
                    let path = path.to_string_lossy();
                    tracing::error!("Error reading metadata of file {path}: {err}");
                    None
                }
            };

            let mut contents = Vec::new();
            let mut file = file;
            if let Err(err) = file.read_to_end(&mut contents).await {
                contents.clear();
                let path = path.to_string_lossy();
                tracing::error!("Error reading file {path}: {err}");
            }
            (metadata, contents)
        }

        let (md, contents) = read_file(handle.path()).await;

        Self {
            handle,
            md,
            contents,
        }
    }
}

impl FileDialogSelection for DesktopFileSelection {
    fn creation_time(&self) -> Option<DateTime<Utc>> {
        self.md
            .as_ref()
            .and_then(|md| md.created().ok())
            .map(DateTime::<Utc>::from)
    }

    fn modification_time(&self) -> Option<DateTime<Utc>> {
        self.md
            .as_ref()
            .and_then(|md| md.modified().ok())
            .map(DateTime::<Utc>::from)
    }

    fn file_name(&self) -> String {
        self.handle.file_name()
    }

    fn size(&self) -> Option<u64> {
        self.md.as_ref().map(|md| md.len())
    }

    fn file_type(&self) -> Option<String> {
        self.handle
            .path()
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| ".".to_owned() + x)
    }

    fn contents(&self) -> &[u8] {
        &self.contents
    }

    fn write_and_refresh(&mut self, data: &[u8]) {
        let _ = std::fs::write(self.handle.path(), data);
        self.md = std::fs::metadata(self.handle.path()).ok();
        self.contents = std::fs::read(self.handle.path()).unwrap_or_default();
    }
}

pub struct DesktopUiBackend {
    // It's important that `clipboard`` gets dropped before `window`, dropping
    // them the other way around causes a segfault inside `smithay_clipboard`.
    // See:
    // - https://github.com/emilk/egui/issues/7660
    // - https://github.com/emilk/egui/issues/7743
    clipboard: Clipboard,
    window: Arc<Window>,

    event_loop: EventLoopProxy<RuffleEvent>,
    cursor_visible: bool,
    preferences: GlobalPreferences,
    preferred_cursor: MouseCursor,
    font_database: Rc<fontdb::Database>,
    file_picker: FilePicker,

    // It's non-trivial to invalidate all fonts and change the renderer, so
    // do not allow changing it in runtime.
    device_font_renderer: DeviceFontRenderer,

    // Shared atlas pages that every device font loaded by this backend packs
    // its glyphs into, instead of each font getting its own.
    font_atlases: FontAtlases,
}

impl DesktopUiBackend {
    pub fn new(
        window: Arc<Window>,
        event_loop: EventLoopProxy<RuffleEvent>,
        font_database: Rc<fontdb::Database>,
        preferences: GlobalPreferences,
        file_picker: FilePicker,
    ) -> Result<Self, Error> {
        // The window handle is only relevant to linux/wayland
        // If it fails it'll fallback to x11 or wlr-data-control
        let clipboard = Clipboard::new(window.display_handle().ok().map(|handle| handle.as_raw()));
        Ok(Self {
            window,
            event_loop,
            cursor_visible: true,
            clipboard,
            preferred_cursor: MouseCursor::Arrow,
            font_database,
            file_picker,
            device_font_renderer: preferences
                .device_font_renderer()
                .unwrap_or(DeviceFontRenderer::Embedded),
            preferences,
            font_atlases: FontAtlases::new(),
        })
    }

    fn show_open_dialog<F, O>(&self, filters: &[FileFilter], f: F) -> Option<O>
    where
        F: FnOnce(AsyncFileDialog) -> O,
    {
        let mut dialog = AsyncFileDialog::new();

        for filter in filters {
            let extensions = filter.extensions_for_dialog(cfg!(target_os = "macos"));
            dialog = dialog.add_filter(&filter.description, &extensions);
        }

        self.file_picker.show_dialog(dialog, f)
    }

    pub fn cursor(&self) -> egui::CursorIcon {
        if self.cursor_visible {
            match self.preferred_cursor {
                MouseCursor::Arrow => egui::CursorIcon::Default,
                MouseCursor::Hand => egui::CursorIcon::PointingHand,
                MouseCursor::IBeam => egui::CursorIcon::Text,
                MouseCursor::Grab => egui::CursorIcon::Grab,
            }
        } else {
            egui::CursorIcon::None
        }
    }
}

impl UiBackend for DesktopUiBackend {
    fn mouse_visible(&self) -> bool {
        self.cursor_visible
    }

    fn set_mouse_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    fn set_mouse_cursor(&mut self, cursor: MouseCursor) {
        self.preferred_cursor = cursor;
    }

    fn clipboard_content(&mut self) -> String {
        self.clipboard.get().unwrap_or_default()
    }

    fn set_clipboard_content(&mut self, content: String) {
        self.clipboard.set_text(content);
    }

    fn set_fullscreen(&mut self, is_full: bool) -> Result<(), FullscreenError> {
        self.window.set_fullscreen(if is_full {
            Some(Fullscreen::Borderless(None))
        } else {
            None
        });
        Ok(())
    }

    fn display_root_movie_download_failed_message(&self, _invalid_swf: bool, _fetch_error: String) {
        let _ = self
            .event_loop
            .send_event(RuffleEvent::OpenDialog(DialogDescriptor::ShowMessage(
                MessageDialogConfiguration::new(
                    LocalizableText::LocalizedText("message-dialog-root-movie-load-error-title"),
                    LocalizableText::LocalizedText(
                        "message-dialog-root-movie-load-error-description",
                    ),
                ),
            )));
    }

    fn message(&self, message: &str) {
        let _ = self
            .event_loop
            .send_event(RuffleEvent::OpenDialog(DialogDescriptor::ShowMessage(
                MessageDialogConfiguration::new(
                    LocalizableText::NonLocalizedText("Ruffle".into()),
                    LocalizableText::NonLocalizedText(message.to_string().into()),
                ),
            )));
    }

    fn display_unsupported_video(&self, url: Url) {
        if url.scheme() == "javascript" {
            tracing::warn!(
                "SWF tried to run a script on desktop, but javascript calls are not allowed"
            );
            return;
        }

        let open_url_mode = self.preferences.open_url_mode();
        if open_url_mode == OpenUrlMode::Confirm {
            let message = format!("The SWF file wants to open the website {url}");
            // TODO: Add a checkbox with a GUI toolkit
            let confirm = MessageDialog::new()
                .set_title("Open website?")
                .set_level(MessageLevel::Info)
                .set_description(message)
                .set_buttons(MessageButtons::OkCancel)
                .show()
                == MessageDialogResult::Ok;
            if !confirm {
                tracing::info!("SWF tried to open a website, but the user declined the request");
                return;
            }
        } else if open_url_mode == OpenUrlMode::Deny {
            tracing::warn!("SWF tried to open a website, but opening a website is not allowed");
            return;
        }

        // If the user confirmed or if in Allow mode, open the website

        // TODO: This opens local files in the browser while flash opens them
        // in the default program for the respective filetype.
        // This especially includes mailto links. Ruffle opens the browser which opens
        // the preferred program while flash opens the preferred program directly.

        match webbrowser::open(url.as_str()) {
            Ok(_output) => {}
            Err(e) => tracing::error!("Could not open URL {}: {}", url, e),
        };
    }

    fn load_device_font(&self, query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
        let name = &query.name;
        let is_bold = query.is_bold;
        let is_italic = query.is_italic;

        let weight = if is_bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        };
        let style = if is_italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        };

        // Try the requested name first, then well-known CJK aliases (e.g. Windows
        // "宋体" → macOS "Songti SC"). Register under the original query name so
        // later lookups still succeed.
        let mut candidate_names = vec![name.as_str()];
        candidate_names.extend(cjk_device_font_aliases(name));

        let mut matched: Option<(String, fontdb::ID)> = None;
        for candidate in candidate_names {
            if let Some((canon, id)) =
                resolve_fontdb_family(&self.font_database, candidate, weight, style)
            {
                if canon.as_str() != name.as_str() {
                    tracing::info!(
                        "Device font \"{name}\" matched to \"{canon}\" (via \"{candidate}\")"
                    );
                }
                matched = Some((canon, id));
                break;
            }
        }

        // It'd be nice if we can get the full list of candidates... Feature request?
        if let Some((_canon, id)) = matched
            && let Some(face) = self.font_database.face(id)
        {
            tracing::info!(
                "Loading device font \"{}\" for \"{name}\" (italic: {is_italic}, bold: {is_bold})",
                face.post_script_name
            );

            match load_fontdb_font(
                name.to_string(),
                face,
                self.device_font_renderer,
                &self.font_atlases,
            ) {
                Ok(font_definition) => register(font_definition),
                Err(error) => tracing::error!("Error loading font from fontdb: {error}"),
            }
        }
    }

    #[allow(unused_variables)]
    fn sort_device_fonts(
        &self,
        query: &FontQuery,
        register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        // Prefer a locally installed equivalent of Windows/Flash CJK names so
        // fontconfig does not fall through to Latin-only faces (e.g. Verdana).
        let sort_query = resolve_cjk_sort_query(query, &self.font_database);

        cfg_select! {
            all(unix, feature = "fontconfig") => {
                let mut fonts = fontconfig::sort_device_fonts(
                    &sort_query,
                    register,
                    self.device_font_renderer,
                    &self.font_atlases,
                )
                .inspect_err(|err| tracing::error!("Cannot sort device fonts: {err}"))
                .unwrap_or_default();

                // Ensure CJK-capable faces are present for glyph fallback even when
                // fontconfig's first matches lack East Asian coverage.
                append_cjk_fontdb_fallbacks(
                    query,
                    &self.font_database,
                    self.device_font_renderer,
                    &self.font_atlases,
                    register,
                    &mut fonts,
                );
                fonts
            }
            _ => {
                let mut fonts = Vec::new();
                append_cjk_fontdb_fallbacks(
                    query,
                    &self.font_database,
                    self.device_font_renderer,
                    &self.font_atlases,
                    register,
                    &mut fonts,
                );
                fonts
            }
        }
    }

    // Unused on desktop
    fn open_virtual_keyboard(&self) {}

    fn close_virtual_keyboard(&self) {}

    fn language(&self) -> LanguageIdentifier {
        self.preferences.language()
    }

    fn display_file_open_dialog(&mut self, filters: Vec<FileFilter>) -> Option<DialogResultFuture> {
        let result = self.show_open_dialog(&filters, |d| d.pick_file())?;

        Some(Box::pin(async move {
            Ok(if let Some(handle) = result.await {
                FileDialogResult::Selection(Box::new(DesktopFileSelection::new(handle).await))
            } else {
                FileDialogResult::Canceled
            })
        }))
    }

    fn display_file_open_dialog_multiple(
        &mut self,
        filters: Vec<FileFilter>,
    ) -> Option<MultiDialogResultFuture> {
        let result = self.show_open_dialog(&filters, |d| d.pick_files())?;

        Some(Box::pin(async move {
            Ok(if let Some(handles) = result.await {
                let selections = futures::future::join_all(handles.into_iter().map(|h| async {
                    Box::new(DesktopFileSelection::new(h).await) as Box<dyn FileDialogSelection>
                }))
                .await;

                MultiFileDialogResult::Selection(selections)
            } else {
                MultiFileDialogResult::Canceled
            })
        }))
    }

    fn display_file_save_dialog(
        &mut self,
        file_name: String,
        title: String,
    ) -> Option<DialogResultFuture> {
        // Select the location to save the file to
        let dialog = AsyncFileDialog::new()
            .set_title(&title)
            .set_file_name(&file_name);

        let result = self.file_picker.show_dialog(dialog, |d| d.save_file())?;

        Some(Box::pin(async move {
            Ok(if let Some(handle) = result.await {
                FileDialogResult::Selection(Box::new(DesktopFileSelection::new(handle).await))
            } else {
                FileDialogResult::Canceled
            })
        }))
    }

    fn close_file_dialog(&mut self) {}
}

fn load_font_from_file(
    path: &Path,
    name: String,
    index: u32,
    is_bold: bool,
    is_italic: bool,
    device_font_renderer: DeviceFontRenderer,
    #[allow(unused_variables)] atlases: &FontAtlases,
) -> Result<FontDefinition<'static>> {
    match device_font_renderer {
        #[cfg(all(target_os = "linux", feature = "freetype"))]
        DeviceFontRenderer::Freetype => {
            use ruffle_frontend_utils::backends::ui::FreetypeFontRenderer;

            Ok(FontDefinition::ExternalRenderer {
                name,
                is_bold,
                is_italic,
                font_renderer: Box::new(FreetypeFontRenderer::new(path, index, atlases)?),
            })
        }
        _ => {
            let file = File::open(path)
                .map_err(|e| anyhow!("Couldn't open font file at {path:?}: {e}"))?;

            // SAFETY: We have to assume that the font file won't change.
            // This assumption is realistic, as we're using system fonts only.
            // However, we never store other references to this data, and we reparse
            // the whole file each time we're accessing any font data.
            // Realistically, when the underlying file or memory region changes,
            // we can expect Ruffle to crash due to SIGBUS or errors when parsing.
            let mmap = unsafe { memmap2::Mmap::map(&file) };

            let mmap = mmap.map_err(|e| anyhow!("Failed to mmap font file at {path:?}: {e}"))?;
            let data = FontFileData::new(mmap);
            Ok(FontDefinition::FontFile {
                name,
                is_bold,
                is_italic,
                data,
                index,
            })
        }
    }
}

fn load_fontdb_font(
    name: String,
    face: &FaceInfo,
    device_font_renderer: DeviceFontRenderer,
    atlases: &FontAtlases,
) -> Result<FontDefinition<'static>> {
    let is_bold = face.weight > fontdb::Weight::NORMAL;
    let is_italic = face.style != fontdb::Style::Normal;

    match &face.source {
        fontdb::Source::File(path) => load_font_from_file(
            path,
            name,
            face.index,
            is_bold,
            is_italic,
            device_font_renderer,
            atlases,
        ),

        fontdb::Source::Binary(bin) | fontdb::Source::SharedFile(_, bin) => {
            Ok(FontDefinition::FontFile {
                name,
                is_bold,
                is_italic,
                data: FontFileData::new_shared(bin.clone()),
                index: face.index,
            })
        }
    }
}

/// Flash/Windows CJK family names → platform fonts commonly installed elsewhere.
fn cjk_device_font_aliases(name: &str) -> &'static [&'static str] {
    match name {
        // Song / Ming
        "宋体" | "SimSun" | "NSimSun" | "新宋体" => &[
            "Songti SC",
            "STSong",
            "Noto Serif CJK SC",
            "Source Han Serif SC",
        ],
        "宋体-简" | "宋體-簡" => &["Songti SC", "STSong"],
        "宋体-繁" | "宋體-繁" => &["Songti TC", "STSong"],
        // Hei / Gothic
        "黑体" | "SimHei" => &[
            "Heiti SC",
            "STHeiti",
            "PingFang SC",
            "Hiragino Sans GB",
            "Microsoft YaHei",
            "Noto Sans CJK SC",
            "Source Han Sans SC",
        ],
        "微软雅黑" | "Microsoft YaHei" => &[
            "PingFang SC",
            "Heiti SC",
            "Microsoft YaHei UI",
            "Noto Sans CJK SC",
            "Source Han Sans SC",
        ],
        // Kai
        "楷体" | "楷体_GB2312" | "KaiTi" | "STKaiti" => &[
            "Kaiti SC",
            "STKaiti",
            "KaiTi",
            "Noto Serif CJK SC",
        ],
        // Fangsong
        "仿宋" | "仿宋_GB2312" | "FangSong" => &["STFangsong", "FangSong", "Songti SC"],
        // YouYuan / LiSu / STXihei — approximate with available gothic/serif faces
        "幼圆" | "YouYuan" => &["PingFang SC", "Heiti SC", "Hiragino Sans GB", "Microsoft YaHei"],
        "隶书" | "LiSu" => &["STXingkai", "Kaiti SC", "STKaiti", "Songti SC"],
        "华文细黑" | "STXihei" => &["STHeiti", "Heiti SC", "PingFang SC", "Hiragino Sans GB"],
        "华文黑体" => &["STHeiti", "Heiti SC", "PingFang SC"],
        "华文楷体" | "华文宋体" | "华文仿宋" => &["STKaiti", "Kaiti SC", "Songti SC", "STSong"],
        _ => &[],
    }
}

fn cjk_glyph_fallback_families() -> &'static [&'static str] {
    &[
        "PingFang SC",
        "Heiti SC",
        "STHeiti",
        "Hiragino Sans GB",
        "Songti SC",
        "STSong",
        "Microsoft YaHei",
        "Microsoft JhengHei",
        "Noto Sans CJK SC",
        "Noto Sans CJK TC",
        "Source Han Sans SC",
        "WenQuanYi Zen Hei",
        "Arial Unicode MS",
    ]
}

/// Case-insensitive family resolve + weight/style query (fontdb is case-sensitive).
fn resolve_fontdb_family(
    font_database: &fontdb::Database,
    name: &str,
    weight: fontdb::Weight,
    style: fontdb::Style,
) -> Option<(String, fontdb::ID)> {
    let lowercase_name = name.to_lowercase();
    let canonical = font_database.faces().find_map(|face| {
        face.families
            .iter()
            .find(|(fam, _)| fam.to_lowercase() == lowercase_name)
            .map(|(fam, _)| fam.clone())
    })?;
    let id = font_database.query(&fontdb::Query {
        families: &[Family::Name(&canonical)],
        weight,
        style,
        ..Default::default()
    })?;
    Some((canonical, id))
}

fn resolve_cjk_sort_query(query: &FontQuery, font_database: &fontdb::Database) -> FontQuery {
    let weight = if query.is_bold {
        fontdb::Weight::BOLD
    } else {
        fontdb::Weight::NORMAL
    };
    let style = if query.is_italic {
        fontdb::Style::Italic
    } else {
        fontdb::Style::Normal
    };

    if resolve_fontdb_family(font_database, &query.name, weight, style).is_some() {
        return query.clone();
    }

    for alias in cjk_device_font_aliases(&query.name) {
        if let Some((canon, _)) = resolve_fontdb_family(font_database, alias, weight, style) {
            tracing::info!(
                "Sorting device fonts for \"{}\" using installed alias \"{canon}\"",
                query.name
            );
            return FontQuery::new(query.font_type, canon, query.is_bold, query.is_italic);
        }
    }

    query.clone()
}

fn append_cjk_fontdb_fallbacks(
    query: &FontQuery,
    font_database: &fontdb::Database,
    device_font_renderer: DeviceFontRenderer,
    atlases: &FontAtlases,
    register: &mut dyn FnMut(FontDefinition),
    fonts: &mut Vec<FontQuery>,
) {
    let weight = if query.is_bold {
        fontdb::Weight::BOLD
    } else {
        fontdb::Weight::NORMAL
    };
    let style = if query.is_italic {
        fontdb::Style::Italic
    } else {
        fontdb::Style::Normal
    };

    for family in cjk_glyph_fallback_families() {
        let Some((canon, id)) = resolve_fontdb_family(font_database, family, weight, style) else {
            continue;
        };

        // Skip duplicates already provided by fontconfig / earlier aliases.
        if fonts
            .iter()
            .any(|existing| existing.lowercase_name == canon.to_lowercase())
        {
            continue;
        }

        let Some(face) = font_database.face(id) else {
            continue;
        };
        let is_bold = face.weight > fontdb::Weight::NORMAL;
        let is_italic = face.style != fontdb::Style::Normal;

        match load_fontdb_font(canon.clone(), face, device_font_renderer, atlases) {
            Ok(definition) => {
                register(definition);
                fonts.push(FontQuery::new(
                    query.font_type,
                    canon,
                    is_bold,
                    is_italic,
                ));
            }
            Err(error) => {
                tracing::error!("Error loading CJK fallback font \"{family}\": {error}");
            }
        }
    }
}

#[cfg(all(unix, feature = "fontconfig"))]
mod fontconfig {
    use crate::backends::ui::{DeviceFontRenderer, load_font_from_file};
    use ruffle_core::backend::ui::FontDefinition;
    use ruffle_core::font::{FontAtlases, FontQuery};
    use std::path::Path;

    #[derive(Debug, thiserror::Error)]
    pub enum FontconfigError {
        #[error("Malformed font family")]
        MalformedFontFamily,
        #[error("Internal fontconfig error: {0}")]
        Internal(#[from] fontconfig::FontconfigError),
    }

    pub fn sort_device_fonts(
        query: &FontQuery,
        register: &mut dyn FnMut(FontDefinition),
        device_font_renderer: DeviceFontRenderer,
        atlases: &FontAtlases,
    ) -> Result<Vec<FontQuery>, FontconfigError> {
        use fontconfig::{FontFormat, Pattern};
        use std::sync::LazyLock;

        static FONTCONFIG: LazyLock<Option<fontconfig::Fontconfig>> =
            LazyLock::new(fontconfig::Fontconfig::new);

        let Some(fc) = FONTCONFIG.as_ref() else {
            return Ok(Vec::new());
        };

        let Ok(family) = std::ffi::CString::new(query.name.as_str()) else {
            return Err(FontconfigError::MalformedFontFamily);
        };

        let mut pattern: Pattern<'static> = Pattern::new(fc)?;

        pattern.add_string(fontconfig::FC_FAMILY, family.as_c_str())?;

        if query.is_bold {
            pattern.add_integer(fontconfig::FC_WEIGHT, fontconfig::FC_WEIGHT_BOLD)?;
        }
        if query.is_italic {
            pattern.add_integer(fontconfig::FC_SLANT, fontconfig::FC_SLANT_ITALIC)?;
        }

        let font_set = pattern.sort_fonts(fontconfig::UnicodeCoverage::Trim)?;

        let mut font_queries = Vec::new();
        for font in font_set.iter() {
            let is_ttf = font
                .format()
                .is_ok_and(|f| matches!(f, FontFormat::TrueType));
            if !is_ttf {
                if let Ok(name) = font.name() {
                    tracing::info!(
                        "Skipping font '{name}' because it's not a TTF (it doesn't have a name)"
                    );
                }
                continue;
            }

            let (
                Ok(name), //
                Ok(filename),
                Ok(index),
                Ok(weight),
                Ok(slant),
            ) = (
                font.name(),
                font.filename(),
                font.face_index(),
                font.weight(),
                font.slant(),
            )
            else {
                continue;
            };

            let Ok(index) = index.try_into() else {
                continue;
            };

            let is_bold = weight >= fontconfig::FC_WEIGHT_BOLD;
            let is_italic = slant >= fontconfig::FC_SLANT_ITALIC;

            match load_font_from_file(
                Path::new(filename),
                name.to_string(),
                index,
                is_bold,
                is_italic,
                device_font_renderer,
                atlases,
            ) {
                Ok(definition) => register(definition),
                Err(err) => {
                    tracing::error!("Error loading font from fontconfig: {err}");
                    continue;
                }
            }

            let query = FontQuery::new(query.font_type, name.to_string(), is_bold, is_italic);
            font_queries.push(query);
        }

        Ok(font_queries)
    }
}
