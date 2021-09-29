//! Client side interface to the proxmox backup server
//!
//! This library implements the client side to access the backups
//! server using https.

use anyhow::Error;
use openssl::pkey::{PKey, Private};

use pbs_api_types::{Authid, Userid};
use pbs_tools::ticket::Ticket;
use pbs_tools::cert::CertInfo;

pub mod catalog_shell;
pub mod dynamic_index;
pub mod pxar;
pub mod tools;

mod merge_known_chunks;
pub mod pipe_to_stream;

mod http_client;
pub use http_client::*;

mod vsock_client;
pub use vsock_client::*;

mod task_log;
pub use task_log::*;

mod backup_reader;
pub use backup_reader::*;

mod backup_writer;
pub use backup_writer::*;

mod remote_chunk_reader;
pub use remote_chunk_reader::*;

mod pxar_backup_stream;
pub use pxar_backup_stream::*;

mod backup_repo;
pub use backup_repo::*;

mod backup_specification;
pub use backup_specification::*;

mod chunk_stream;
pub use chunk_stream::{ChunkStream, FixedChunkStream};

pub const PROXMOX_BACKUP_TCP_KEEPALIVE_TIME: u32 = 120;

/// Connect to localhost:8007 as root@pam
///
/// This automatically creates a ticket if run as 'root' user.
pub fn connect_to_localhost(auth_key: Option<&PKey<Private>>) -> Result<HttpClient, Error> {
    let options = if let Some(auth_key) = auth_key {
        let ticket = Ticket::new("PBS", Userid::root_userid())?
            .sign(auth_key, None)?;
        let fingerprint = CertInfo::new()?.fingerprint()?;
        HttpClientOptions::new_non_interactive(ticket, Some(fingerprint))
    } else {
        HttpClientOptions::new_interactive(None, None)
    };

    HttpClient::new("localhost", 8007, Authid::root_auth_id(), options)
}
