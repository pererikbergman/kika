# ADR-0002: Nested intents, one enum per feature

- Status: Accepted
- Date: 2026-10-13
- Scope: how `AppIntent` is built and dispatched. Behaviour, key bindings, binding ids and order, and the config format do not change.

## Context

ADR-0001 moved every feature into its own folder under `src/features/`, but kept `AppIntent` as one flat enum in `src/intent.rs`. Every feature still adds its variants there and an arm per variant to the one `match` in `update()`. A feature change therefore still touches two core hubs, and parallel jobs still collide in them.

ADR-0001 rejected nesting "for now". The 349 bindings built `AppIntent` values in one `static` registry, so nesting would have rewritten every binding, menu item and test in one go, for no behaviour gain. The move is done, so this ADR revisits that choice.

## Decision

1. **Shape.** Each feature owns `pub(crate) enum <Feature>Intent` in `src/features/<f>/intent.rs`, deriving `Debug, Clone, PartialEq, Eq`. `AppIntent` keeps its core variants plus one tuple variant per feature, `AppIntent::<Feature>(<Feature>Intent)`. A sub-feature's enum is nested in its parent's enum, for example `CommitsIntent::Reflog(ReflogIntent)`, so core keeps one arm per top-level feature. This follows the dependency rules: a parent may import its children. The parent's `update` dispatches to the child's (`commits::update` matches `CommitsIntent::Reflog(i)` and calls `reflog::update`). `From` chains through the parent: the child provides `From<ReflogIntent> for CommitsIntent` and the parent `From<CommitsIntent> for AppIntent`, so `ReflogIntent::X.into()` gives a `CommitsIntent`, and code that needs an `AppIntent` writes the full path. Intents that many features share, such as `OpenFilter`, `ListEscape`, `PopPopup` and `Quit`, stay core variants, and a feature's bindings keep using them.
2. **Update seam.** Each feature has `pub(crate) fn update(state: &mut AppState, intent: <Feature>Intent) -> Vec<Effect>` in `features/<f>/update.rs`. It is a free function, so it cannot clash with handler names such as `update_submodule`. The core arm is one line, for example `AppIntent::CommandLog(i) => return command_log::update(self, i),`. The feature's `update` is pure, as before. Features whose handlers return effects keep this signature: an arm that was `=> return self.handler()` in core becomes `=> return state.handler()` in the feature's `update`, and the rest fall through to `Vec::new()`. A feature's `update` reaches a destructive `GitOp` only through a confirm question (`src/confirm_question.rs`), never by returning the run effect directly.
3. **Bindings and menus.** Tuple-variant constructors are `const`, so a `static` `Binding` or `MenuItem` writes `intent: AppIntent::CommandLog(CommandLogIntent::Open)`. This needs no `LazyLock` and no change to `Binding`. Each top-level feature also provides `From<<Feature>Intent> for AppIntent` (sub-features chain through the parent, see point 1). It is not `const`, so statics never use it.
4. **Tests.** Tests write either the full path, `AppIntent::CommandLog(CommandLogIntent::Next)`, or `CommandLogIntent::Next.into()`. Asserts on `binding.intent` compare with the full nested value.
5. **Sibling intents.** A feature never builds `AppIntent::<Sibling>(..)`, because that imports a sibling, which ADR-0001's dependency rules forbid (see also its rule 2). When feature A must make feature B act, A returns an `Effect` or a core intent, or the core refresh plan (`refresh.rs`, run by `reload.rs`) reloads B. If B's intent is the only way, the core arm or core `Runtime` builds it, or the intent stays a core variant.
6. **`Effect` stays flat.** It is never built in statics, and `Runtime::run` already sends each arm to `features/<f>/effects.rs`. Nesting it gains no seam and churns the e2e and effect tests. It can be revisited separately.
7. **Migration (expand, then contract).** The Command log is the pilot and the expand step (#389). #390 nests the commits subtree, #391 the branches subtree and #392 the remaining features. #393 deletes the last flat feature variants and adds an Outcome section here. Until then, flat and nested variants live side by side. From now on, a new intent for a feature goes in that feature's enum if it has one. Each variant keeps its doc comment when it moves.
8. **Why ADR-0001's objection no longer holds.** Bindings now live in per-feature `keys.rs` chunks, so each feature's slice rewrites only its own chunk, menu items and tests, not the whole registry at once. Binding ids and order are untouched, and the golden test `registry_order_is_pinned` guards them.

## Consequences

- Adding an intent to a feature touches only its folder: `intent.rs`, `update.rs` and, if it is bound, `keys.rs`. `src/intent.rs` and `update()` in `src/model.rs` change only when a feature is added.
- `update()` shrinks to the core arms plus one arm per top-level feature.
- Bindings, menu items and tests that name a feature intent get a longer path. The ids, keys, order and descriptions do not change, so `docs/configuration.md` and `registry_order_is_pinned` stay as they are.
- During the migration, readers meet both styles: a feature with an `intent.rs` is nested, the others are still flat.
- A sub-feature's intent is two levels deep (`AppIntent::Commits(CommitsIntent::Reflog(...))`). #390 and #391 may revisit this if the depth hurts readability.
