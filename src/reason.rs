//! Reasons worked out at run time, such as one that names a branch, for
//! the bar and the keymap, which carry `&'static str`.

use std::collections::BTreeSet;
use std::sync::Mutex;

/// The most distinct texts kept. Each names a branch or a commit the user
/// acted on, so a session stays far below it; past it a plain reason
/// stands in rather than memory growing.
const MAX_KEPT: usize = 4096;

/// `text` as a `&'static str`. Each distinct text is allocated once and
/// kept for the rest of the run, so asking again, as every redraw of the
/// bar does, costs a lookup.
pub fn intern(text: String) -> &'static str {
    static KEPT: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = kept.get(text.as_str()) {
        return found;
    }
    if kept.len() >= MAX_KEPT {
        return "Not possible here";
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    kept.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_is_the_same_str() {
        let a = intern("Branch x sits here".to_string());
        let b = intern("Branch x sits here".to_string());
        assert_eq!(a, "Branch x sits here");
        assert!(std::ptr::eq(a, b));
    }
}
