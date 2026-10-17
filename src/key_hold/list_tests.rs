//! Holding keys for the lists other than Files: Branches (Local, Remotes,
//! Tags), Commits (Log, Reflog) and Stash. Each test starts the user
//! reload with `r` and lands each list with its own load.

use super::*;
use crate::effect::Tag;
use crate::handoff::HandOff;
use crate::model::Focus;
use crate::op::GitOp;
use crate::popup::Popup;
use crate::test_support::*;
use crate::watch::WatchEvent;
use kika_git::branches::Branch;
use kika_git::log::LogPage;
use kika_git::refs::RefsSnapshot;
use kika_git::remotes::Remote;
use kika_git::tags::TagRef;

/// Start a user reload, as `r` does, and return the loads it started.
fn refresh(state: &mut AppState) -> Vec<Effect> {
    state.update(AppIntent::RefreshStatus)
}

/// Every git op `effects` run, attached or not.
fn ops(effects: &[Effect]) -> Vec<GitOp> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::RunOp { op, .. } | Effect::HandOff(HandOff::Attached { op, .. }) => {
                Some(op.clone())
            }
            _ => None,
        })
        .collect()
}

/// What the screen and the effects say, for "never names" checks.
fn said(state: &AppState, effects: &[Effect]) -> String {
    format!("{effects:?} {:?} {:?}", state.popups, state.input)
}

fn land_branches(state: &mut AppState, effects: &[Effect], rows: Vec<Branch>) -> Vec<Effect> {
    state.update(branches_loaded(branches_tag(effects), rows))
}

fn land_remotes(state: &mut AppState, effects: &[Effect], rows: Vec<Remote>) -> Vec<Effect> {
    state.update(remotes_loaded(remotes_tag(effects), rows))
}

fn land_tags(state: &mut AppState, effects: &[Effect], rows: Vec<TagRef>) -> Vec<Effect> {
    state.update(tags_loaded(tags_tag(effects), rows))
}

fn land_log(state: &mut AppState, effects: &[Effect], page: LogPage) -> Vec<Effect> {
    let (tag, _, _) = commits_load(effects);
    state.update(commits_loaded(tag, 0, page))
}

fn stashes_tag(effects: &[Effect]) -> Tag {
    let found = effects.iter().find_map(|e| match e {
        Effect::LoadStashes { tag } => Some(*tag),
        _ => None,
    });
    found.unwrap_or_else(|| panic!("expected LoadStashes in {effects:?}"))
}

fn reflog_load(effects: &[Effect]) -> Tag {
    let found = effects.iter().find_map(|e| match e {
        Effect::LoadReflog { tag } => Some(*tag),
        _ => None,
    });
    found.unwrap_or_else(|| panic!("expected LoadReflog in {effects:?}"))
}

/// `with_branches` (`main`, `topic`, `wip`) with the cursor on `topic`.
fn on_topic() -> AppState {
    let mut state = remote_ready();
    press_char(&mut state, 'j');
    assert_eq!(selected_name(&state), "topic");
    state
}

fn main_and_wip() -> Vec<Branch> {
    let mut rows = three_branches();
    rows.remove(1);
    rows
}

#[test]
fn branches_a_held_checkout_never_checks_out_a_branch_the_reload_dropped() {
    let mut state = on_topic();
    let loads = refresh(&mut state);

    assert_eq!(press_char(&mut state, ' '), vec![], "held");
    let effects = land_branches(&mut state, &loads, main_and_wip());

    // The cursor stayed in place, on `wip`, and the key acted on that.
    assert_eq!(ops(&effects), [GitOp::Checkout("wip".into())]);
    assert!(!state.key_hold.has_keys());
}

#[test]
fn branches_a_held_checkout_runs_once_on_the_branch_still_listed() {
    let mut state = on_topic();
    let loads = refresh(&mut state);

    assert_eq!(press_char(&mut state, ' '), vec![], "held");
    let effects = land_branches(&mut state, &loads, three_branches());
    assert_eq!(ops(&effects), [GitOp::Checkout("topic".into())]);

    // Nothing waits any more: the other lists landing replay nothing.
    let mut later = land_remotes(&mut state, &loads, vec![]);
    later.extend(land_tags(&mut state, &loads, vec![]));
    later.extend(land_log(&mut state, &loads, page(0, 0, true)));
    later.extend(state.update(stashes_loaded(stashes_tag(&loads), Ok(vec![]))));
    later.extend(land_status(&mut state, &loads, vec![]));
    assert_eq!(ops(&later), []);
    assert!(!state.key_hold.has_keys());
}

#[test]
fn remotes_a_held_checkout_never_checks_out_a_remote_branch_the_reload_dropped() {
    // origin/topic, with a local `topic`: space checks that out at once.
    let mut state = on_remote_row(3);
    state.set_busy(0);
    let loads = refresh(&mut state);

    assert_eq!(press_char(&mut state, ' '), vec![], "held");
    let mut remotes = three_remotes();
    remotes[1] = git_remote("origin", true, &["main"]);
    let effects = land_remotes(&mut state, &loads, remotes);

    assert_eq!(ops(&effects), []);
    assert!(!said(&state, &effects).contains("topic"), "{effects:?}");
    assert!(!state.key_hold.has_keys());
}

#[test]
fn tags_a_held_delete_asks_about_the_tag_now_selected() {
    let mut state = with_branches();
    let three = vec![tag_ref("v3", 'c'), tag_ref("v2", 'b'), tag_ref("v1", 'a')];
    reload_tags(&mut state, three.clone());
    press_char(&mut state, ']');
    press_char(&mut state, ']');
    press_char(&mut state, 'j');
    assert_eq!(state.key_focus(), KeyScope::Tags);
    let loads = refresh(&mut state);

    assert_eq!(press_char(&mut state, 'd'), vec![], "held");
    assert_eq!(state.popups, [], "nothing asked yet");
    let effects = land_tags(&mut state, &loads, vec![three[0].clone(), three[2].clone()]);

    let asked = said(&state, &effects);
    assert!(matches!(state.popups.last(), Some(Popup::Question(_))));
    assert!(asked.contains("v1"), "{asked}");
    assert!(!asked.contains("v2"), "{asked}");
}

#[test]
fn log_a_held_checkout_never_names_a_commit_the_reload_dropped() {
    let mut state = with_log(5);
    state.commits.list.selected = 2;
    let loads = refresh(&mut state);

    let held = press_char(&mut state, ' ');
    assert_eq!(held, vec![], "held");
    assert_eq!(state.popups, []);
    let mut rows = hex_page(5, true);
    rows.rows.remove(2);
    let effects = land_log(&mut state, &loads, rows);

    let asked = said(&state, &effects);
    assert!(!asked.contains(&oid(2)), "{asked}");
    assert!(
        asked.contains(&oid(3)),
        "acted on the row now shown: {asked}"
    );
}

#[test]
fn reflog_a_held_move_moves_over_the_reloaded_rows() {
    // second, first, root; the cursor on second.
    let mut state = undoable();
    let tag = reflog_load(&press_char(&mut state, ']'));
    state.update(reflog_loaded(tag, commit_reflog('c')));
    assert_eq!(state.key_focus(), KeyScope::Reflog);
    let loads = refresh(&mut state);
    let tag = reflog_load(&loads);

    press_char(&mut state, 'j');
    assert_eq!(state.commits.reflog.list.selected, 0, "held");
    // `second` went: the cursor stays at the top, on `first`, and j
    // moves from there. Moved early, it would have followed `first`.
    let mut reflog = commit_reflog('c');
    reflog.entries.remove(0);
    state.update(reflog_loaded(tag, reflog));

    assert_eq!(state.commits.reflog.list.selected, 1);
}

#[test]
fn stash_a_held_apply_never_applies_a_stash_the_reload_dropped() {
    let mut state = on_stash_one();
    let loads = refresh(&mut state);

    assert_eq!(press_char(&mut state, ' '), vec![], "held");
    let after = vec![
        stash_entry(0, "a0", "WIP on main: 1234 zero"),
        stash_entry(1, "a2", "On main: two"),
    ];
    let effects = state.update(stashes_loaded(stashes_tag(&loads), Ok(after)));

    assert_eq!(ops(&effects), [GitOp::StashApply(stash_ref(1, "a2"))]);
}

#[test]
fn stash_a_held_apply_runs_once_on_the_stash_still_listed() {
    let mut state = on_stash_one();
    let loads = refresh(&mut state);

    press_char(&mut state, ' ');
    let effects = state.update(stashes_loaded(stashes_tag(&loads), Ok(three_stashes())));
    assert_eq!(ops(&effects), [GitOp::StashApply(stash_ref(1, "a1"))]);

    let later = land_status(&mut state, &loads, three_files());
    assert_eq!(ops(&later), []);
    assert!(!state.key_hold.has_keys());
}

#[test]
fn background_list_loads_hold_no_keys() {
    // The watch needs a repository to watch.
    let mut state = on_stash_one().with_location(Some("/r/a".into()), "/r/a".into());
    let first = match watch(&mut state, WatchEvent::PollRefs).as_slice() {
        [Effect::SnapshotRefs { tag }] => *tag,
        other => panic!("expected SnapshotRefs, got {other:?}"),
    };
    state.update(AppIntent::RefsLoaded {
        tag: first,
        result: Ok(RefsSnapshot::of(b"a")),
    });
    let moved = match watch(&mut state, WatchEvent::PollRefs).as_slice() {
        [Effect::SnapshotRefs { tag }] => *tag,
        other => panic!("expected SnapshotRefs, got {other:?}"),
    };
    let loads = state.update(AppIntent::RefsLoaded {
        tag: moved,
        result: Ok(RefsSnapshot::of(b"b")),
    });
    // The branch list, the log and the stash list are all loading.
    branches_tag(&loads);
    commits_load(&loads);
    stashes_tag(&loads);

    assert_eq!(state.key_focus(), KeyScope::Stash);
    assert!(!state.holds_keys());
    for focus in [Focus::Branches, Focus::Commits] {
        let name = format!("{focus:?}");
        state.focus = focus;
        assert!(!state.holds_keys(), "{name}");
    }
    state.focus = Focus::Stash;
    assert_eq!(
        ops(&press_char(&mut state, ' ')),
        [GitOp::StashApply(stash_ref(1, "a1"))],
        "acted at once"
    );
}

#[test]
fn a_watch_load_that_supersedes_the_user_reload_carries_the_hold() {
    let mut state = on_topic();
    let loads = refresh(&mut state);
    press_char(&mut state, ' ');

    // The watch reloads the branches before the user's load lands, which
    // makes that one stale.
    let newer = state.background_branches();
    assert_eq!(
        land_branches(&mut state, &loads, three_branches()),
        vec![],
        "stale"
    );
    assert!(state.key_hold.has_keys(), "still held");

    let effects = land_branches(&mut state, &newer, main_and_wip());
    assert_eq!(ops(&effects), [GitOp::Checkout("wip".into())]);
    assert!(!state.key_hold.has_keys());
}

#[test]
fn a_failed_list_load_drops_the_held_keys() {
    let mut state = on_stash_one();
    let loads = refresh(&mut state);
    press_char(&mut state, ' ');

    let effects = state.update(stashes_loaded(stashes_tag(&loads), Err("boom".into())));

    assert_eq!(ops(&effects), []);
    assert_eq!(state.notice, Some(KEYS_DROPPED.into()));
    assert!(!state.key_hold.has_keys());
    // A later key acts at once.
    assert_eq!(press_char(&mut state, 'k'), vec![]);
    assert_eq!(state.stash.list.selected, 0, "moved at once");
}

#[test]
fn held_keys_never_answer_a_branch_delete_question() {
    // `d` then `y` on topic; the reload drops it and the cursor lands on
    // wip, which the user never selected.
    let mut state = on_topic();
    let loads = refresh(&mut state);
    press_char(&mut state, 'd');
    press_char(&mut state, 'y');

    let effects = land_branches(&mut state, &loads, main_and_wip());

    assert_eq!(ops(&effects), []);
    assert!(matches!(state.popups.last(), Some(Popup::Question(_))));
    assert_eq!(state.notice, Some(KEYS_DROPPED.into()));
    assert!(!state.key_hold.has_keys());
}

#[test]
fn held_keys_never_answer_a_stash_drop_question() {
    let mut state = on_stash_one();
    let loads = refresh(&mut state);
    press_char(&mut state, 'd');
    press_char(&mut state, 'y');

    let after = vec![
        stash_entry(0, "a0", "WIP on main: 1234 zero"),
        stash_entry(1, "a2", "On main: two"),
    ];
    let effects = state.update(stashes_loaded(stashes_tag(&loads), Ok(after)));

    assert_eq!(ops(&effects), []);
    assert_eq!(state.key_focus(), KeyScope::DropStash);
    assert_eq!(state.notice, Some(KEYS_DROPPED.into()));
    assert!(!state.key_hold.has_keys());
}

#[test]
fn a_replayed_tab_switch_onto_a_list_still_loading_keeps_the_rest_held() {
    let mut state = remote_ready();
    reload_remotes(&mut state, three_remotes());
    let loads = refresh(&mut state);
    press_char(&mut state, ']');
    press_char(&mut state, 'j');

    land_branches(&mut state, &loads, three_branches());
    assert_eq!(state.key_focus(), KeyScope::Remotes, "the ] replayed");
    assert!(state.key_hold.has_keys(), "j waits for the remotes");
    assert_eq!(state.key_hold.waiting_on, Some(HeldList::Remotes));

    land_remotes(&mut state, &loads, three_remotes());
    assert_eq!(state.branches.remotes.list.selected, 1);
    assert!(!state.key_hold.has_keys());
}

#[test]
fn switching_repository_clears_the_armed_lists() {
    let mut state = on_stash_one();
    refresh(&mut state);
    press_char(&mut state, ' ');

    state.switch_repo();

    assert_eq!(state.key_hold, KeyHold::default());
    assert_eq!(state.notice, None, "cleared quietly");
}
