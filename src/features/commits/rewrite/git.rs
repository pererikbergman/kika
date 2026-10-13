//! The interactive rebase every log rewrite runs: its argv, with the
//! plan handed to the sequence editor in the environment.

use crate::git::rebase_plan::{RebasePlan, encode};
use crate::git::runner::GitCommand;
use crate::sequence_editor::PLAN_ENV;

/// The plan goes to the sequence editor, which `handoff` names; the
/// base is hex digits only. The keys are newer config that an older
/// git ignores; they keep the todo list to what the sequence editor
/// accepts. `--update-refs` (git 2.38) or the key turning it off
/// always wins over the user's `rebase.updateRefs`. Comments in the
/// todo use `#`, which the sequence editor reads them by, whatever
/// the user's `core.commentChar` or `core.commentString`; the
/// command line's `-c` wins over every config file. A stopped
/// rebase's todo is read with the user's own prefix, as git will. Its
/// hooks and editor see the user's env, with only the plan added.
pub(crate) fn rebase_command(plan: &RebasePlan) -> GitCommand {
    let mut cmd = GitCommand::new("-c").user_env();
    if !plan.update_refs {
        cmd = cmd.arg("rebase.updateRefs=false").arg("-c");
    }
    cmd = cmd
        .arg("rebase.rebaseMerges=false")
        .arg("-c")
        .arg("core.commentChar=#")
        .arg("rebase")
        .arg("-i");
    if plan.update_refs {
        cmd = cmd.arg("--update-refs");
    }
    cmd.arg("--autostash")
        .arg("--no-autosquash")
        .arg("--keep-empty")
        .arg(plan.base.as_str())
        .env(PLAN_ENV, encode(&plan.steps))
}

#[cfg(test)]
mod tests {
    use crate::op::{GitOp, op_command};
    use crate::test_support::argv;

    #[test]
    fn an_interactive_rebase_passes_only_the_plan_in_its_env() {
        let plan = crate::git::rebase_plan::sample();
        let cmd = op_command(&GitOp::InteractiveRebase(plan.clone()));
        assert_eq!(
            argv(&cmd),
            [
                "-c",
                "rebase.updateRefs=false",
                "-c",
                "rebase.rebaseMerges=false",
                "-c",
                "core.commentChar=#",
                "rebase",
                "-i",
                "--autostash",
                "--no-autosquash",
                "--keep-empty",
                plan.base.as_str(),
            ]
        );
        let envs: Vec<_> = cmd
            .envs()
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();
        let encoded = format!("drop {}\npick {}\n", "b".repeat(40), "a".repeat(40));
        assert_eq!(envs, [("KIKA_REBASE_PLAN", encoded.as_str())]);
        assert_eq!(cmd.input(), None);
        assert_eq!(cmd.subcommand(), "rebase");
    }

    #[test]
    fn an_interactive_rebase_runs_in_the_user_s_env_plus_the_plan() {
        use crate::git::runner::EnvProfile;
        let plan = crate::git::rebase_plan::sample();
        let cmd = op_command(&GitOp::InteractiveRebase(plan.clone()));
        assert_eq!(cmd.env_profile(), EnvProfile::UserEnv);
        let encoded = crate::git::rebase_plan::encode(&plan.steps);
        assert_eq!(
            cmd.process_env(),
            [("KIKA_REBASE_PLAN".into(), encoded.into())]
        );
    }

    #[test]
    fn with_update_refs_the_rebase_passes_the_flag_and_not_the_key() {
        let mut plan = crate::git::rebase_plan::sample();
        plan.update_refs = true;
        let cmd = op_command(&GitOp::InteractiveRebase(plan.clone()));
        assert_eq!(
            argv(&cmd),
            [
                "-c",
                "rebase.rebaseMerges=false",
                "-c",
                "core.commentChar=#",
                "rebase",
                "-i",
                "--update-refs",
                "--autostash",
                "--no-autosquash",
                "--keep-empty",
                plan.base.as_str(),
            ]
        );
        assert_eq!(cmd.subcommand(), "rebase");
    }
}
