use crate::intent::AppIntent;

/// What Kika is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// The local working tree and index.
    Workspace,
    /// A remote pull/merge request.
    Review { number: u64 },
}

/// The single source of truth for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    pub context: Context,
    pub size: (u16, u16),
    pub should_quit: bool,
}

impl AppState {
    pub fn new(context: Context) -> Self {
        Self {
            context,
            size: (0, 0),
            should_quit: false,
        }
    }

    /// Apply an intent. Pure: no I/O, no rendering.
    pub fn update(&mut self, intent: AppIntent) {
        match intent {
            AppIntent::Quit => self.should_quit = true,
            AppIntent::Resize { width, height } => self.size = (width, height),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_sets_should_quit() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Quit);
        assert!(state.should_quit);
    }

    #[test]
    fn resize_records_size() {
        let mut state = AppState::new(Context::Workspace);
        state.update(AppIntent::Resize {
            width: 120,
            height: 40,
        });
        assert_eq!(state.size, (120, 40));
    }
}
