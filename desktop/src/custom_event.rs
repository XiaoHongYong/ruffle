//! Custom event type for desktop ruffle

use ruffle_core::events::PlayerNotification;
use ruffle_frontend_utils::content::ContentDescriptor;
use std::path::PathBuf;

use crate::gui::{DialogDescriptor, PathOrUrlFieldHandle};
use crate::player::{LaunchOptions, PlayerRunnable};

pub enum OpenType {
    File,
    Directory,
}

/// What happens when the user clicks a file in the folder browser.
pub enum FolderBrowserSelection {
    /// Open the clicked file in the main player window.
    Open(Box<LaunchOptions>),
    /// Fill a path/URL form field with the clicked file (and its directory root).
    FillField(PathOrUrlFieldHandle),
}

/// User-defined events.
pub enum RuffleEvent {
    /// Indicates that a task is ready to be polled.
    TaskPoll(PlayerRunnable),

    /// Indicates that an asynchronous SWF metadata load has been completed.
    OnMetadata(ruffle_core::swf::HeaderExt),

    /// The user requested to pick and then open a file.
    BrowseAndOpen(Box<LaunchOptions>, OpenType),

    /// The user requested to open a movie.
    Open(ContentDescriptor, Box<LaunchOptions>),

    /// A file was dropped onto the window (Windows `WM_DROPFILES` path).
    OpenDroppedFile(PathBuf),

    /// Show the folder browser window for a previously picked directory.
    ShowFolderBrowser(PathBuf, FolderBrowserSelection),

    /// The user requested to close the current SWF.
    CloseFile,

    /// The user requested to enter full screen.
    EnterFullScreen,

    /// The user requested to exit full screen.
    ExitFullScreen,

    /// The user requested to exit Ruffle.
    ExitRequested,

    /// The user selected an item in the right-click context menu.
    ContextMenuItemClicked(usize),

    /// The movie wants to open a dialog.
    OpenDialog(DialogDescriptor),

    /// Ruffle core has a notification to handle.
    PlayerNotification(PlayerNotification),

    /// Export Ruffle Bundle from currently playing content and open save dialog.
    ExportBundle,
}
