use crate::engine::executor::StepContext;
use crate::engine::instance::WorkflowInstance;
use crate::workflow::target_paths::TargetPathConfig;

pub(super) fn seed_target_paths(context: &mut StepContext, instance: &WorkflowInstance) {
    let target_paths = TargetPathConfig::from_repo_config(&instance.config.repo);
    let repo_root = context.work_dir().clone();
    let project_dir = target_paths.project_dir(&repo_root);
    let artifact_base_dir = target_paths.artifact_base_dir(&repo_root);
    let diff_base_dir = target_paths.diff_base_dir(&repo_root);
    context.set("repo_root", &repo_root.to_string_lossy());
    context.set(
        "project_subdir",
        &path_value(target_paths.project_subdir.as_deref()),
    );
    context.set("project_dir", &project_dir.to_string_lossy());
    context.set("artifact_base_dir", &artifact_base_dir.to_string_lossy());
    context.set(
        "diff_path_base",
        &path_value(target_paths.diff_path_base.as_deref()),
    );
    context.set("diff_path_base_dir", &diff_base_dir.to_string_lossy());
    context.set(
        "diff_path_normalization",
        match target_paths.diff_path_normalization {
            crate::workflow::schema::DiffPathNormalization::RepoRelative => "repo_relative",
            crate::workflow::schema::DiffPathNormalization::BaseRelative => "base_relative",
        },
    );
}

fn path_value(path: Option<&std::path::Path>) -> String {
    path.map_or_else(String::new, |path| path.to_string_lossy().into_owned())
}
