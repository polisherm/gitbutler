use std::{sync::Arc, time::Duration};

use anyhow::Result;
use gitbutler_core::{
    assets,
    projects::ProjectId,
    virtual_branches::{self, VirtualBranches},
};
use governor::{
    clock::QuantaClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter,
};
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use super::events;
use crate::events as app_events;

#[derive(Clone)]
pub struct Handler {
    inner: Arc<Mutex<InnerHandler>>,
    limit: Arc<RateLimiter<NotKeyed, InMemoryState, QuantaClock>>,
}

impl TryFrom<&AppHandle> for Handler {
    type Error = anyhow::Error;

    fn try_from(value: &AppHandle) -> std::result::Result<Self, Self::Error> {
        if let Some(handler) = value.try_state::<Handler>() {
            Ok(handler.inner().clone())
        } else {
            let vbranches = value
                .state::<virtual_branches::Controller>()
                .inner()
                .clone();
            let proxy = value.state::<assets::Proxy>().inner().clone();
            let inner = InnerHandler::new(vbranches, proxy);
            let handler = Handler::new(inner);
            value.manage(handler.clone());
            Ok(handler)
        }
    }
}

impl Handler {
    fn new(inner: InnerHandler) -> Self {
        let quota = Quota::with_period(Duration::from_millis(100)).expect("valid quota");
        Self {
            inner: Arc::new(Mutex::new(inner)),
            limit: Arc::new(RateLimiter::direct(quota)),
        }
    }

    pub async fn handle(&self, project_id: &ProjectId) -> Result<Vec<events::Event>> {
        if self.limit.check().is_err() {
            Ok(vec![])
        } else if let Ok(handler) = self.inner.try_lock() {
            handler.handle(project_id).await
        } else {
            Ok(vec![])
        }
    }
}

struct InnerHandler {
    vbranch_controller: virtual_branches::Controller,
    assets_proxy: assets::Proxy,
}

impl InnerHandler {
    fn new(vbranch_controller: virtual_branches::Controller, assets_proxy: assets::Proxy) -> Self {
        Self {
            vbranch_controller,
            assets_proxy,
        }
    }

    pub async fn handle(&self, project_id: &ProjectId) -> Result<Vec<events::Event>> {
        match self
            .vbranch_controller
            .list_virtual_branches(project_id)
            .await
        {
            Ok((branches, _, skipped_files)) => {
                let branches = self.assets_proxy.proxy_virtual_branches(branches).await;
                Ok(vec![events::Event::Emit(
                    app_events::Event::virtual_branches(
                        project_id,
                        &VirtualBranches {
                            branches,
                            skipped_files,
                        },
                    ),
                )])
            }
            Err(error) => {
                if error.is::<virtual_branches::errors::VerifyError>() {
                    Ok(vec![])
                } else {
                    Err(error.context("failed to list virtual branches").into())
                }
            }
        }
    }
}
