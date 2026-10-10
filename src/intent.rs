use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Everything that can change the model: user input and async task results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppIntent {
    Quit,
    Resize { width: u16, height: u16 },
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
    fn unmapped_key_is_dropped() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(AppIntent::from_event(event), None);
    }
}
