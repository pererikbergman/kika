//! The Tags feature's `update` handlers: moving through and loading the
//! Tags tab, making a tag from the Commits panel, and deleting and pushing
//! the selected tag.

use super::keys;
use super::{PendingTag, TagsIntent};
use crate::confirm_question::{Answer, ConfirmQuestion, Then};
use crate::effect::{Effect, Tag};
use crate::features::branches::BranchesTab;
use crate::key_hold::HeldList;
use crate::keymap::{self, KeyScope};
use crate::model::{AppState, Context};
use crate::op::GitOp;
use crate::popup::{LineInput, Popup, PromptKind, RangeKind};
use kika_git::commit_files::CommitId;
use kika_git::remote::RemoteOp;
use kika_git::tags::{TagRef, validate_tag_name};

/// The Tags tab's half of `update`, which `branches::update` passes
/// `BranchesIntent::Tags` to. A free function, so its name cannot clash
/// with handlers.
pub(crate) fn update(state: &mut AppState, intent: TagsIntent) -> Vec<Effect> {
    match intent {
        TagsIntent::Loaded { tag, result } => return state.tags_loaded(tag, result),
        TagsIntent::Next => return state.move_tag(1),
        TagsIntent::Prev => return state.move_tag(-1),
        TagsIntent::MoveBy(delta) => return state.move_tag(delta),
        TagsIntent::New => state.ask_tag_name(false),
        TagsIntent::NewAnnotated => state.ask_tag_name(true),
        TagsIntent::Named(text) => return state.tag_named(text),
        TagsIntent::Messaged(text) => return state.tag_messaged(&text),
        TagsIntent::Delete if state.branches.tags.range_anchor.is_some() => {
            state.ask_delete_range(RangeKind::Tags)
        }
        TagsIntent::Delete => state.ask_delete_tag(),
        TagsIntent::ConfirmDelete => return state.confirm_delete_tag(),
        TagsIntent::Push => return state.push_tag(),
    }
    Vec::new()
}

/// Tags: made from the Commits panel, deleted and pushed from the Tags
/// tab. The commit, and the tag asked about, are taken when the prompt or
/// question opens, so nothing that moves underneath can change them.
impl AppState {
    pub(crate) fn move_tag(&mut self, delta: isize) -> Vec<Effect> {
        if self.branches.tags.move_by(delta) {
            self.load_branch_log(true)
        } else {
            vec![]
        }
    }

    /// Show the tags, and on their tab load the selected tag's log afresh,
    /// as for the local list.
    pub(crate) fn tags_loaded(
        &mut self,
        tag: Tag,
        result: Result<Vec<TagRef>, String>,
    ) -> Vec<Effect> {
        self.user_done(tag);
        let listed = result.is_ok();
        if !self.branches.tags.loaded(tag, self.generation, result) {
            return vec![];
        }
        let mut effects = if self.branches.tab == BranchesTab::Tags {
            self.load_branch_log(tag.user)
        } else {
            vec![]
        };
        effects.extend(self.list_landed(HeldList::Tags, listed));
        effects
    }

    /// Ask for the new tag's name, keeping the selected commit as its
    /// target.
    pub(crate) fn ask_tag_name(&mut self, annotated: bool) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = keys::tag_commit_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(target) = self
            .commits
            .list
            .rows
            .get(self.commits.list.selected)
            .and_then(|row| CommitId::parse(&row.oid))
        else {
            return;
        };
        self.branches.pending_tag = Some(PendingTag {
            target,
            annotated,
            name: None,
        });
        self.popups.push(Popup::Prompt {
            kind: PromptKind::TagName,
            input: LineInput::default(),
        });
    }

    /// Check the typed name: a bad one keeps the prompt open with the
    /// reason; a good one makes a lightweight tag, or asks for the
    /// annotated tag's message.
    pub(crate) fn tag_named(&mut self, text: String) -> Vec<Effect> {
        let Some(pending) = self.branches.pending_tag.as_mut() else {
            return vec![];
        };
        let text = text.trim().to_string();
        if let Err(reason) = validate_tag_name(&text) {
            self.notice = Some(reason.into());
            self.popups.push(Popup::Prompt {
                kind: PromptKind::TagName,
                input: LineInput::with_text(&text),
            });
            return vec![];
        }
        if pending.annotated {
            pending.name = Some(text);
            self.popups.push(Popup::Prompt {
                kind: PromptKind::TagMessage,
                input: LineInput::default(),
            });
            return vec![];
        }
        let target = pending.target.clone();
        self.branches.pending_tag = None;
        self.create_tag(GitOp::CreateTag {
            name: text,
            target,
            message: None,
        })
    }

    /// Make the annotated tag named before, once its message has words.
    pub(crate) fn tag_messaged(&mut self, text: &str) -> Vec<Effect> {
        let Some(PendingTag {
            name: Some(name),
            target,
            ..
        }) = self.branches.pending_tag.clone()
        else {
            return vec![];
        };
        if text.trim().is_empty() {
            self.notice = Some("An annotated tag needs a message".into());
            self.popups.push(Popup::Prompt {
                kind: PromptKind::TagMessage,
                input: LineInput::with_text(text),
            });
            return vec![];
        }
        self.branches.pending_tag = None;
        self.create_tag(GitOp::CreateTag {
            name,
            target,
            message: Some(text.to_string()),
        })
    }

    pub(crate) fn create_tag(&mut self, op: GitOp) -> Vec<Effect> {
        if self.op_is_running() {
            self.notice = Some(keymap::OP_RUNNING.into());
            return vec![];
        }
        self.start_op(Ok(Some(op)))
    }

    /// Ask before deleting the selected tag, here only.
    pub(crate) fn ask_delete_tag(&mut self) {
        if self.context != Context::Workspace {
            return;
        }
        if let Some(reason) = keys::delete_tag_disabled(self) {
            self.notice = Some(reason);
            return;
        }
        let Some(tag) = self.branches.tags.selected_row() else {
            return;
        };
        let op = GitOp::DeleteTag {
            name: tag.name.clone(),
            oid: tag.ref_oid.clone(),
        };
        let question = delete_tag_question(tag.name.display(), op);
        self.popups.push(Popup::Question(question));
    }

    /// Delete the tag the open question names, and only if one is open.
    pub(crate) fn confirm_delete_tag(&mut self) -> Vec<Effect> {
        self.answer_question(KeyScope::DeleteTag, Answer::YOnly)
    }

    /// Push the selected tag to the remote Kika chooses; the network only
    /// on this key.
    pub(crate) fn push_tag(&mut self) -> Vec<Effect> {
        if self.context != Context::Workspace {
            return vec![];
        }
        if let Some(reason) = keymap::push_tag_disabled(self) {
            self.notice = Some(reason);
            return vec![];
        }
        let (Some(tag), Ok(remote)) = (
            self.branches.tags.selected_row(),
            keymap::push_tag_remote(self),
        ) else {
            return vec![];
        };
        let tag = tag.name.clone();
        self.start_remote(RemoteOp::PushTag { remote, tag })
    }

    /// Closing a tag's name or message prompt forgets the tag.
    pub(crate) fn tags_popup_closed(&mut self, closed: &Popup) {
        if matches!(
            closed,
            Popup::Prompt {
                kind: PromptKind::TagName | PromptKind::TagMessage,
                ..
            }
        ) {
            self.branches.pending_tag = None;
        }
    }
}

/// "Delete this tag?", carrying the op chosen when it was asked, so a
/// reload before the answer cannot change the target. `name` is the tag's
/// display name, already terminal-safe.
pub(crate) fn delete_tag_question(name: String, op: GitOp) -> ConfirmQuestion {
    ConfirmQuestion {
        scope: KeyScope::DeleteTag,
        title: "Delete tag".into(),
        text: format!("Delete tag {name}? It is not deleted on any remote. (y/n)"),
        answer: Answer::YOnly,
        then: Then::Op(op),
        range: false,
        head: None,
    }
}

#[cfg(test)]
mod tests {
    use crate::confirm_question::Then;
    use crate::features::branches::BranchesIntent;
    use crate::features::branches::BranchesTab;
    use crate::features::branches::tags::TagsIntent;
    use crate::intent::AppIntent;
    use crate::keymap::{self, KeyScope};
    use crate::model::{AppState, Context, Focus};
    use crate::op::GitOp;
    use crate::popup::{Popup, PromptKind};
    use crate::test_support::{
        cid, finished, log_load, on_commit, op_of, press, press_char, reload_tags, shift, tag_ref,
        tags_tag, two_tags, type_text, with_branches,
    };
    use crossterm::event::KeyCode;

    /// `with_branches`, with `two_tags` loaded and the Tags tab shown.
    fn on_tags() -> AppState {
        let mut state = with_branches();
        reload_tags(&mut state, two_tags());
        press_char(&mut state, ']');
        press_char(&mut state, ']');
        assert_eq!(state.branches.tab, BranchesTab::Tags);
        state
    }

    #[test]
    fn move_by_moves_the_tag_cursor_clamped_with_one_log_load() {
        let mut state = on_tags();
        let by = |delta| AppIntent::Branches(TagsIntent::MoveBy(delta).into());
        let (_, refname) = log_load(&state.update(by(5))).unwrap();
        assert_eq!(refname, b"refs/tags/v1", "clamped at the end");
        assert_eq!(state.update(by(1)), vec![], "no move, no load");
        let (_, refname) = log_load(&state.update(by(-5))).unwrap();
        assert_eq!(refname, b"refs/tags/v2", "clamped at the start");
    }

    #[test]
    fn the_tags_tab_shows_the_selected_tags_log_and_moves_with_j_and_k() {
        let mut state = with_branches();
        reload_tags(&mut state, two_tags());
        press_char(&mut state, ']');
        let (_, refname) = log_load(&press_char(&mut state, ']')).unwrap();
        assert_eq!(refname, b"refs/tags/v2");
        let (tag, refname) = log_load(&press_char(&mut state, 'j')).unwrap();
        assert_eq!(refname, b"refs/tags/v1");
        assert!(tag.user);
        assert_eq!(press_char(&mut state, 'j'), vec![], "at the end");
        log_load(&press(&mut state, KeyCode::Up)).unwrap();
        assert_eq!(
            state.branches.tags.selected_row().unwrap().name,
            "v2".into()
        );
        // A reload while the tab is shown reloads its log too.
        let effects = reload_tags(&mut state, two_tags());
        assert_eq!(log_load(&effects).unwrap().1, b"refs/tags/v2");
    }

    #[test]
    fn a_stale_or_other_repository_tags_load_is_dropped() {
        let mut state = with_branches();
        let tag = tags_tag(&state.update(AppIntent::RefreshStatus));
        let newer = tags_tag(&state.update(AppIntent::RefreshStatus));
        state.update(AppIntent::Branches(BranchesIntent::Tags(
            TagsIntent::Loaded {
                tag,
                result: Ok(two_tags()),
            },
        )));
        assert!(!state.branches.tags.loaded);
        state.generation += 1;
        state.update(AppIntent::Branches(BranchesIntent::Tags(
            TagsIntent::Loaded {
                tag: newer,
                result: Ok(two_tags()),
            },
        )));
        assert!(!state.branches.tags.loaded);
    }

    #[test]
    fn t_then_a_name_tags_the_commit_selected_when_t_was_pressed() {
        let mut state = on_commit();
        assert_eq!(press_char(&mut state, 't'), vec![]);
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::TagName,
                ..
            }]
        ));
        assert_eq!(state.prompt_title(PromptKind::TagName), "New tag on c1");
        // The selection moves while the user types.
        state.commits.list.selected = 2;
        type_text(&mut state, " v1.0 ");
        let effects = press(&mut state, KeyCode::Enter);
        let (_, op) = op_of(&effects);
        assert_eq!(
            op,
            GitOp::CreateTag {
                name: "v1.0".into(),
                target: cid("c1"),
                message: None,
            }
        );
        assert_eq!(state.popups, []);
        assert_eq!(state.branches.pending_tag, None);
    }

    #[test]
    fn shift_t_asks_a_name_then_a_message_and_refuses_an_empty_one() {
        let mut state = on_commit();
        shift(&mut state, 'T');
        type_text(&mut state, "v2\n");
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::TagMessage,
                ..
            }]
        ));
        assert_eq!(
            state.prompt_title(PromptKind::TagMessage),
            "Message of tag v2"
        );
        type_text(&mut state, "  ");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(
            state.notice,
            Some("An annotated tag needs a message".into())
        );
        assert!(matches!(
            state.popups.as_slice(),
            [Popup::Prompt {
                kind: PromptKind::TagMessage,
                ..
            }]
        ));
        press(&mut state, KeyCode::Backspace);
        press(&mut state, KeyCode::Backspace);
        type_text(&mut state, "-m Release two");
        let (_, op) = op_of(&press(&mut state, KeyCode::Enter));
        assert_eq!(
            op,
            GitOp::CreateTag {
                name: "v2".into(),
                target: cid("c1"),
                message: Some("-m Release two".into()),
            }
        );
        assert_eq!(state.popups, []);
        assert_eq!(state.branches.pending_tag, None);
    }

    #[test]
    fn an_invalid_tag_name_keeps_the_prompt_open_with_the_reason() {
        let mut state = on_commit();
        press_char(&mut state, 't');
        for (bad, reason) in [
            ("-x", "A tag name cannot start with -"),
            ("a..b", "A tag name cannot contain .. or @{"),
            ("HEAD", "A tag cannot be called @ or HEAD"),
        ] {
            type_text(&mut state, bad);
            assert_eq!(press(&mut state, KeyCode::Enter), vec![], "{bad}");
            assert_eq!(state.notice, Some(reason.into()), "{bad}");
            match state.popups.as_slice() {
                [Popup::Prompt { kind, input }] => {
                    assert_eq!(*kind, PromptKind::TagName);
                    assert_eq!(input.text, bad, "the text stays to fix");
                }
                other => panic!("expected the prompt kept open, got {other:?}"),
            }
            for _ in bad.chars() {
                press(&mut state, KeyCode::Backspace);
            }
        }
        assert!(state.branches.pending_tag.is_some());
        press(&mut state, KeyCode::Esc);
        assert_eq!(state.branches.pending_tag, None);
        assert_eq!(
            state.update(AppIntent::Branches(BranchesIntent::Tags(
                TagsIntent::Named("v1".into())
            ))),
            vec![]
        );
    }

    #[test]
    fn a_tag_answer_waits_for_a_running_op_and_no_commit_opens_nothing() {
        let mut state = on_commit();
        press_char(&mut state, 't');
        state.hold_op(99);
        type_text(&mut state, "v1");
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.notice, Some(keymap::OP_RUNNING.into()));

        let mut state = AppState::new(Context::Workspace);
        state.focus = Focus::Commits;
        assert_eq!(
            state.update(AppIntent::Branches(BranchesIntent::Tags(TagsIntent::New))),
            vec![]
        );
        assert_eq!(state.notice, Some("No commit".into()));
        assert_eq!(state.popups, []);
    }

    #[test]
    fn d_asks_and_only_y_deletes_the_tag_as_listed() {
        let mut state = on_tags();
        press_char(&mut state, 'd');
        assert_eq!(state.key_focus(), KeyScope::DeleteTag);
        let expected = GitOp::DeleteTag {
            name: "v2".into(),
            oid: "b".repeat(40),
        };
        match state.popups.as_slice() {
            [Popup::Question(q)] => {
                assert!(q.text.starts_with("Delete tag v2?"), "{}", q.text);
                assert_eq!(q.then, Then::Op(expected.clone()));
            }
            other => panic!("expected the question, got {other:?}"),
        }
        assert_eq!(press(&mut state, KeyCode::Enter), vec![]);
        assert_eq!(state.popups.len(), 1, "enter does not answer");
        // A reload under the question cannot change what it deletes.
        reload_tags(&mut state, vec![tag_ref("v9", 'c')]);
        let (tag, op) = op_of(&press_char(&mut state, 'y'));
        assert_eq!(op, expected);
        assert_eq!(state.popups, []);
        // The lists, tags among them, reload once it is done.
        tags_tag(&state.update(finished(tag, op, Ok(()))));

        for key in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut state = on_tags();
            press_char(&mut state, 'd');
            assert_eq!(press(&mut state, key), vec![]);
            assert_eq!(state.popups, [], "{key:?}");
            assert_eq!(
                state.update(AppIntent::Branches(BranchesIntent::Tags(
                    TagsIntent::ConfirmDelete
                ))),
                vec![]
            );
        }
    }

    #[test]
    fn closing_a_tag_prompt_forgets_the_tag() {
        use crate::features::branches::tags::PendingTag;
        use crate::popup::LineInput;
        for kind in [PromptKind::TagName, PromptKind::TagMessage] {
            let mut state = AppState::new(Context::Workspace);
            state.branches.pending_tag = Some(PendingTag {
                target: cid("c1"),
                annotated: true,
                name: None,
            });
            state.popups.push(Popup::Prompt {
                kind,
                input: LineInput::default(),
            });
            assert_eq!(state.update(AppIntent::PopPopup), vec![], "{kind:?}");
            assert_eq!(state.popups, [], "{kind:?}");
            assert_eq!(state.branches.pending_tag, None, "{kind:?}");
        }
    }
}
