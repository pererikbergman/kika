use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::effect::Tag;
use crate::git::FileEntry;
use crate::git::diff::DiffChunk;

/// Everything that can change the model: user input and async task results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppIntent {
    Quit,
    Resize {
        width: u16,
        height: u16,
    },
    /// Reload `git status`. Also issued once at startup.
    RefreshStatus,
    /// A status load finished; `tag` says which one.
    StatusLoaded {
        tag: Tag,
        result: Result<Vec<FileEntry>, String>,
    },
    /// Move the file selection down or up.
    SelectNext,
    SelectPrev,
    ScrollDiff(Scroll),
    /// A chunk of the diff stream for `tag`. A stream reports more than once.
    DiffLoaded {
        tag: Tag,
        result: Result<DiffChunk, String>,
    },
    /// The diff for `tag` has taken long enough to show "Loading…".
    DiffSlow {
        tag: Tag,
    },
}

/// How far to scroll the diff pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    LineDown,
    LineUp,
    PageDown,
    PageUp,
}

impl AppIntent {
    /// Map a raw terminal event to an intent. Unmapped events are dropped.
    pub fn from_event(event: Event) -> Option<Self> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => Self::from_key(key),
            Event::Resize(width, height) => Some(Self::Resize { width, height }),
            _ => None,
        }
    }

    fn from_key(key: KeyEvent) -> Option<Self> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => Some(Self::Quit),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(Self::Quit),
            (KeyCode::Char('r'), KeyModifiers::NONE) => Some(Self::RefreshStatus),
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => Some(Self::SelectNext),
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => Some(Self::SelectPrev),
            // Terminals send uppercase letters with SHIFT, so any modifiers match.
            (KeyCode::Char('J'), _) => Some(Self::ScrollDiff(Scroll::LineDown)),
            (KeyCode::Char('K'), _) => Some(Self::ScrollDiff(Scroll::LineUp)),
            (KeyCode::PageDown, _) => Some(Self::ScrollDiff(Scroll::PageDown)),
            (KeyCode::PageUp, _) => Some(Self::ScrollDiff(Scroll::PageUp)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_maps_to_quit() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(AppIntent::from_event(event), Some(AppIntent::Quit));
    }

    #[test]
    fn ctrl_c_maps_to_quit() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(AppIntent::from_event(event), Some(AppIntent::Quit));
    }

    #[test]
    fn esc_maps_to_quit() {
        let event = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(AppIntent::from_event(event), Some(AppIntent::Quit));
    }

    #[test]
    fn r_maps_to_refresh_status() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(AppIntent::from_event(event), Some(AppIntent::RefreshStatus));
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Option<AppIntent> {
        AppIntent::from_event(Event::Key(KeyEvent::new(code, modifiers)))
    }

    #[test]
    fn j_k_and_arrows_move_the_selection() {
        let none = KeyModifiers::NONE;
        assert_eq!(key(KeyCode::Char('j'), none), Some(AppIntent::SelectNext));
        assert_eq!(key(KeyCode::Down, none), Some(AppIntent::SelectNext));
        assert_eq!(key(KeyCode::Char('k'), none), Some(AppIntent::SelectPrev));
        assert_eq!(key(KeyCode::Up, none), Some(AppIntent::SelectPrev));
    }

    #[test]
    fn shifted_j_k_and_page_keys_scroll_the_diff() {
        let shift = KeyModifiers::SHIFT;
        let none = KeyModifiers::NONE;
        let scroll = |s| Some(AppIntent::ScrollDiff(s));
        assert_eq!(key(KeyCode::Char('J'), shift), scroll(Scroll::LineDown));
        assert_eq!(key(KeyCode::Char('K'), shift), scroll(Scroll::LineUp));
        assert_eq!(key(KeyCode::Char('J'), none), scroll(Scroll::LineDown));
        assert_eq!(key(KeyCode::PageDown, none), scroll(Scroll::PageDown));
        assert_eq!(key(KeyCode::PageUp, none), scroll(Scroll::PageUp));
    }

    #[test]
    fn unmapped_key_is_dropped() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(AppIntent::from_event(event), None);
    }
}
