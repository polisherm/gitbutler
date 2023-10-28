use gblib::git;

pub fn temp_dir() -> std::path::PathBuf {
    tempfile::tempdir()
        .expect("failed to create temp dir")
        .into_path()
}

pub struct TestProject {
    local_repository: git::Repository,
    remote_repository: git::Repository,
}

impl Default for TestProject {
    fn default() -> Self {
        let path = temp_dir();
        let local_repository =
            git::Repository::init(path.clone()).expect("failed to init repository");
        let mut index = local_repository.index().expect("failed to get index");
        let oid = index.write_tree().expect("failed to write tree");
        let signature = git::Signature::now("test", "test@email.com").unwrap();
        local_repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "Initial commit",
                &local_repository
                    .find_tree(oid)
                    .expect("failed to find tree"),
                &[],
            )
            .expect("failed to commit");

        let remote_path = temp_dir();
        let remote_repository = git::Repository::init_opts(
            remote_path,
            git2::RepositoryInitOptions::new()
                .bare(true)
                .external_template(false),
        )
        .expect("failed to init repository");

        {
            let mut remote = local_repository
                .remote(
                    "origin",
                    remote_repository
                        .path()
                        .to_str()
                        .expect("failed to convert path to str"),
                )
                .expect("failed to add remote");
            remote
                .push(&["refs/heads/master:refs/heads/master"], None)
                .expect("failed to push");
        }

        Self {
            local_repository,
            remote_repository,
        }
    }
}

impl TestProject {
    pub fn path(&self) -> &std::path::Path {
        self.local_repository.workdir().unwrap()
    }

    pub fn push(&self) {
        let mut origin = self.local_repository.find_remote("origin").unwrap();
        origin
            .push(&["refs/heads/master:refs/heads/master"], None)
            .unwrap();
    }

    pub fn reset_hard(&self, oid: git::Oid) {
        let commit = self.local_repository.find_commit(oid).unwrap();
        self.local_repository
            .reset(&commit, git2::ResetType::Hard, None)
            .unwrap();
    }

    /// fetch remote into local
    pub fn fetch(&self) {
        let mut remote = self.local_repository.find_remote("origin").unwrap();
        remote
            .fetch(&["+refs/heads/*:refs/remotes/origin/*"], None)
            .unwrap();
    }

    /// works like if we'd open and merge a PR on github. does not update local.
    pub fn merge(&self, branch_name: &git::BranchName) {
        let branch_name: git::BranchName = format!("refs/heads/{}", branch_name.branch())
            .parse()
            .unwrap();
        let branch = self.remote_repository.find_branch(&branch_name).unwrap();
        let branch_commit = branch.peel_to_commit().unwrap();

        let master_branch = {
            let name: git::BranchName = "refs/heads/master".parse().unwrap();
            self.remote_repository.find_branch(&name).unwrap()
        };
        let master_branch_commit = master_branch.peel_to_commit().unwrap();

        let merge_base = {
            let oid = self
                .remote_repository
                .merge_base(branch_commit.id(), master_branch_commit.id())
                .unwrap();
            self.remote_repository.find_commit(oid).unwrap()
        };
        let merge_tree = {
            let mut index = self
                .remote_repository
                .merge_trees(
                    &merge_base.tree().unwrap(),
                    &master_branch.peel_to_tree().unwrap(),
                    &branch.peel_to_tree().unwrap(),
                )
                .unwrap();
            let oid = index.write_tree_to(&self.remote_repository).unwrap();
            self.remote_repository.find_tree(oid).unwrap()
        };

        self.remote_repository
            .commit(
                Some("refs/heads/master"),
                &branch_commit.author(),
                &branch_commit.committer(),
                &format!("Merge pull request from {}", branch_name),
                &merge_tree,
                &[&master_branch_commit, &branch_commit],
            )
            .unwrap();
    }

    pub fn find_commit(&self, oid: git::Oid) -> Result<git::Commit, git::Error> {
        self.local_repository.find_commit(oid)
    }

    /// takes all changes in the working directory and commits them into local
    pub fn commit_all(&self, message: &str) -> git::Oid {
        let mut index = self.local_repository.index().expect("failed to get index");
        index
            .add_all(["."], git2::IndexAddOption::DEFAULT, None)
            .expect("failed to add all");
        index.write().expect("failed to write index");
        let oid = index.write_tree().expect("failed to write tree");
        let signature = git::Signature::now("test", "test@email.com").unwrap();
        self.local_repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &self
                    .local_repository
                    .find_tree(oid)
                    .expect("failed to find tree"),
                &[&self
                    .local_repository
                    .find_commit(
                        self.local_repository
                            .refname_to_id("HEAD")
                            .expect("failed to get head"),
                    )
                    .expect("failed to find commit")],
            )
            .expect("failed to commit")
    }

    pub fn references(&self) -> Vec<git::Reference> {
        self.local_repository
            .references()
            .expect("failed to get references")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to read references")
    }
}