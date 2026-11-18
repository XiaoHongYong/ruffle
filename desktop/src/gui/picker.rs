use super::text;
use crate::preferences::GlobalPreferences;
use rfd::AsyncFileDialog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use winit::{event_loop::EventLoopProxy, window::Window};

use crate::custom_event::RuffleEvent;

#[derive(Clone)]
pub struct FilePicker {
    data: Arc<FilePickerData>,
}

struct FilePickerData {
    event_loop: EventLoopProxy<RuffleEvent>,
    parent: Weak<Window>,
    picking: AtomicBool,
    preferences: GlobalPreferences,
}

impl FilePicker {
    pub fn new(
        parent: Weak<Window>,
        preferences: GlobalPreferences,
        event_loop: EventLoopProxy<RuffleEvent>,
    ) -> Self {
        Self {
            data: Arc::new(FilePickerData {
                event_loop,
                parent,
                picking: AtomicBool::new(false),
                preferences,
            }),
        }
    }

    pub fn event_loop(&self) -> &EventLoopProxy<RuffleEvent> {
        &self.data.event_loop
    }

    pub fn parent_window(&self) -> Weak<Window> {
        self.data.parent.clone()
    }

    pub fn show_dialog<F, O>(&self, mut dialog: AsyncFileDialog, f: F) -> Option<O>
    where
        F: FnOnce(AsyncFileDialog) -> O,
    {
        if let Some(parent) = self.data.parent.upgrade() {
            dialog = dialog.set_parent(&parent);
        }

        if self.data.picking.swap(true, Ordering::SeqCst) {
            // Already picking
            return None;
        }
        let result = f(dialog);
        self.data.picking.store(false, Ordering::SeqCst);
        Some(result)
    }

    pub async fn pick_ruffle_file(&self, dir: Option<PathBuf>) -> Option<PathBuf> {
        let locale = &self.data.preferences.language();
        let mut dialog = AsyncFileDialog::new()
            .add_filter(
                text(locale, "file-picker-filter-supported"),
                &["swf", "spl", "ruf"],
            )
            .add_filter(text(locale, "file-picker-filter-swf"), &["swf"])
            .add_filter(text(locale, "file-picker-filter-spl"), &["spl"])
            .add_filter(text(locale, "file-picker-filter-ruf"), &["ruf"])
            .add_filter(text(locale, "file-picker-filter-all"), &["*"])
            .set_title(text(locale, "file-picker-title-open-file"));

        if let Some(dir) = dir {
            dialog = dialog.set_directory(dir);
        }

        if let Some(result) = self.show_dialog(dialog, |d| d.pick_file()) {
            result.await.map(|h| h.into())
        } else {
            None
        }
    }

    pub async fn pick_ruffle_directory(&self, dir: Option<PathBuf>) -> Option<PathBuf> {
        let locale = &self.data.preferences.language();
        let mut dialog =
            AsyncFileDialog::new().set_title(text(locale, "file-picker-title-open-directory"));

        if let Some(ref dir) = dir {
            dialog = dialog.set_directory(dir);
        }

        Some(self.show_dialog(dialog, |d| d.pick_folder())?.await?.into())
    }
}
