use std::{collections::HashMap, path, sync::Arc};

use anyhow::Context;
use futures::future::join_all;
use tauri::AppHandle;
use tokio::sync::Semaphore;

use crate::{
    assets, gb_repository, keys,
    project_repository::{self, conflicts},
    projects, users,
};

pub struct Controller {
    local_data_dir: path::PathBuf,
    semaphores: Arc<tokio::sync::Mutex<HashMap<String, Semaphore>>>,

    assets_proxy: assets::Proxy,
    projects_storage: projects::Storage,
    users_storage: users::Storage,
    keys_storage: keys::Storage,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open project repository")]
    PushError(#[from] project_repository::Error),
    #[error("project is in a conflicted state")]
    Conflicting,
    #[error(transparent)]
    LockError(#[from] tokio::sync::AcquireError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl TryFrom<&AppHandle> for Controller {
    type Error = Error;

    fn try_from(value: &AppHandle) -> Result<Self, Self::Error> {
        let local_data_dir = value
            .path_resolver()
            .app_local_data_dir()
            .context("Failed to get local data dir")?;
        Ok(Self {
            local_data_dir,
            semaphores: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            assets_proxy: assets::Proxy::try_from(value)?,
            projects_storage: projects::Storage::from(value),
            users_storage: users::Storage::from(value),
            keys_storage: keys::Storage::from(value),
        })
    }
}

impl Controller {
    pub async fn create_commit(
        &self,
        project_id: &str,
        branch: &str,
        message: &str,
    ) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;

            super::commit(&gb_repository, &project_repository, branch, message)
        })
        .await?;

        Ok(())
    }

    pub async fn list_virtual_branches(
        &self,
        project_id: &str,
    ) -> Result<Vec<super::VirtualBranch>, Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        let branches = self
            .with_lock(project_id, || {
                let project_repository = project
                    .as_ref()
                    .try_into()
                    .context("failed to open project repository")?;
                let gb_repository = self.open_gb_repository(project_id)?;

                super::list_virtual_branches(&gb_repository, &project_repository)
            })
            .await?;

        Ok(branches)
    }

    pub async fn create_virtual_branch(
        &self,
        project_id: &str,
        create: &super::branch::BranchCreateRequest,
    ) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;

            if conflicts::is_resolving(&project_repository) {
                return Err(Error::Conflicting);
            }

            super::create_virtual_branch(&gb_repository, create)?;
            Ok(())
        })
        .await?;

        Ok(())
    }

    pub async fn create_virtual_branch_from_branch(
        &self,
        project_id: &str,
        branch: &project_repository::branch::Name,
    ) -> Result<String, Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        let branch_id = self
            .with_lock::<Result<String, Error>>(project_id, || {
                let project_repository = project
                    .as_ref()
                    .try_into()
                    .context("failed to open project repository")?;
                let gb_repository = self.open_gb_repository(project_id)?;

                let branch_id = super::create_virtual_branch_from_branch(
                    &gb_repository,
                    &project_repository,
                    branch,
                    None,
                )?;

                // also apply the branch
                super::apply_branch(&gb_repository, &project_repository, &branch_id)?;
                Ok(branch_id)
            })
            .await?;

        Ok(branch_id)
    }

    pub async fn get_base_branch_data(
        &self,
        project_id: &str,
    ) -> Result<Option<super::BaseBranch>, Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;
        let project_repository = project
            .as_ref()
            .try_into()
            .context("failed to open project repository")?;
        let gb_repository = self.open_gb_repository(project_id)?;
        let base_branch = super::get_base_branch_data(&gb_repository, &project_repository)?;
        if let Some(branch) = base_branch {
            Ok(Some(self.proxy_base_branch(branch).await))
        } else {
            Ok(None)
        }
    }

    pub async fn set_base_branch(
        &self,
        project_id: &str,
        target_branch: &str,
    ) -> Result<super::BaseBranch, Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        let target = self
            .with_lock(project_id, || {
                let project_repository = project
                    .as_ref()
                    .try_into()
                    .context("failed to open project repository")?;
                let gb_repository = self.open_gb_repository(project_id)?;

                super::set_base_branch(&gb_repository, &project_repository, target_branch)
            })
            .await?;

        let target = self.proxy_base_branch(target).await;

        Ok(target)
    }

    pub async fn update_base_branch(&self, project_id: &str) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;

            super::update_base_branch(&gb_repository, &project_repository)
        })
        .await?;

        Ok(())
    }

    pub async fn update_virtual_branch(
        &self,
        project_id: &str,
        branch_update: super::branch::BranchUpdateRequest,
    ) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;
            super::update_branch(&gb_repository, &project_repository, branch_update)
        })
        .await?;

        Ok(())
    }

    pub async fn delete_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;
            super::delete_branch(&gb_repository, &project_repository, branch_id)
        })
        .await?;

        Ok(())
    }

    pub async fn apply_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;
            super::apply_branch(&gb_repository, &project_repository, branch_id)
        })
        .await?;

        Ok(())
    }

    pub async fn unapply_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;
            super::unapply_branch(&gb_repository, &project_repository, branch_id)
        })
        .await?;

        Ok(())
    }

    pub async fn push_virtual_branch(
        &self,
        project_id: &str,
        branch_id: &str,
    ) -> Result<(), Error> {
        let project = self
            .projects_storage
            .get_project(project_id)
            .context("failed to get project")?
            .context("project not found")?;

        let private_key = self
            .keys_storage
            .get_or_create()
            .context("failed to get or create private key")?;

        self.with_lock(project_id, || {
            let project_repository = project
                .as_ref()
                .try_into()
                .context("failed to open project repository")?;
            let gb_repository = self.open_gb_repository(project_id)?;

            super::push(&project_repository, &gb_repository, branch_id, &private_key).map_err(|e| {
                match e {
                    super::PushError::Repository(e) => Error::PushError(e),
                    super::PushError::Other(e) => Error::Other(e),
                }
            })
        })
        .await?;

        Ok(())
    }

    async fn with_lock<T>(&self, project_id: &str, action: impl FnOnce() -> T) -> T {
        let mut semaphores = self.semaphores.lock().await;
        let semaphore = semaphores
            .entry(project_id.to_string())
            .or_insert_with(|| Semaphore::new(1));
        let _permit = semaphore.acquire().await;
        action()
    }

    fn open_gb_repository(&self, project_id: &str) -> Result<gb_repository::Repository, Error> {
        gb_repository::Repository::open(
            self.local_data_dir.clone(),
            project_id,
            self.projects_storage.clone(),
            self.users_storage.clone(),
        )
        .context("failed to open repository")
        .map_err(Error::Other)
    }

    async fn proxy_base_branch(&self, target: super::BaseBranch) -> super::BaseBranch {
        super::BaseBranch {
            recent_commits: join_all(
                target
                    .clone()
                    .recent_commits
                    .into_iter()
                    .map(|commit| async move {
                        super::VirtualBranchCommit {
                            author: super::Author {
                                gravatar_url: self
                                    .assets_proxy
                                    .proxy(&commit.author.gravatar_url)
                                    .await
                                    .unwrap_or_else(|e| {
                                        log::error!("failed to proxy gravatar url: {:#}", e);
                                        commit.author.gravatar_url
                                    }),
                                ..commit.author
                            },
                            ..commit
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .await,
            upstream_commits: join_all(
                target
                    .clone()
                    .upstream_commits
                    .into_iter()
                    .map(|commit| async move {
                        super::VirtualBranchCommit {
                            author: super::Author {
                                gravatar_url: self
                                    .assets_proxy
                                    .proxy(&commit.author.gravatar_url)
                                    .await
                                    .unwrap_or_else(|e| {
                                        log::error!("failed to proxy gravatar url: {:#}", e);
                                        commit.author.gravatar_url
                                    }),
                                ..commit.author
                            },
                            ..commit
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .await,
            ..target
        }
    }
}