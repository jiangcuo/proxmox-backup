//! Proxmox Server/Service framework
//!
//! This code provides basic primitives to build our REST API
//! services. We want async IO, so this is built on top of
//! tokio/hyper.

use anyhow::Error;
use serde_json::Value;

use proxmox_sys::fs::{create_path, CreateOptions};

use pbs_buildcfg;

pub mod jobstate;

mod verify_job;
pub use verify_job::*;

mod prune_job;
pub use prune_job::*;

mod gc_job;
pub use gc_job::*;

mod email_notifications;
pub use email_notifications::*;

mod report;
pub use report::*;

pub mod ticket;

pub mod auth;

pub mod pull;

pub(crate) async fn reload_proxy_certificate() -> Result<(), Error> {
    let proxy_pid = proxmox_rest_server::read_pid(pbs_buildcfg::PROXMOX_BACKUP_PROXY_PID_FN)?;
    let sock = proxmox_rest_server::ctrl_sock_from_pid(proxy_pid);
    let _: Value = proxmox_rest_server::send_raw_command(sock, "{\"command\":\"reload-certificate\"}\n")
        .await?;
    Ok(())
}

pub(crate) async fn notify_datastore_removed() -> Result<(), Error> {
    let proxy_pid = proxmox_rest_server::read_pid(pbs_buildcfg::PROXMOX_BACKUP_PROXY_PID_FN)?;
    let sock = proxmox_rest_server::ctrl_sock_from_pid(proxy_pid);
    let _: Value = proxmox_rest_server::send_raw_command(sock, "{\"command\":\"datastore-removed\"}\n")
        .await?;
    Ok(())
}

/// Create the base run-directory.
///
/// This exists to fixate the permissions for the run *base* directory while allowing intermediate
/// directories after it to have different permissions.
pub fn create_run_dir() -> Result<(), Error> {
    let backup_user = pbs_config::backup_user()?;
    let opts = CreateOptions::new()
        .owner(backup_user.uid)
        .group(backup_user.gid);
    let _: bool = create_path(pbs_buildcfg::PROXMOX_BACKUP_RUN_DIR_M!(), None, Some(opts))?;
    Ok(())
}

pub fn create_state_dir() -> Result<(), Error> {
    let backup_user = pbs_config::backup_user()?;
    let opts = CreateOptions::new()
        .owner(backup_user.uid)
        .group(backup_user.gid);
    create_path(pbs_buildcfg::PROXMOX_BACKUP_STATE_DIR_M!(), None, Some(opts))?;
    Ok(())
}
