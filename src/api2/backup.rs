use failure::*;
use futures::*;
use hyper::header::{HeaderValue, UPGRADE};
use hyper::http::request::Parts;
use hyper::{Body, Response, StatusCode};
use serde_json::{json, Value};

use proxmox::{sortable, identity};
use proxmox::api::list_subdirs_api_method;
use proxmox::api::{ApiFuture, ApiHandler, ApiMethod, Router, RpcEnvironment};
use proxmox::api::router::SubdirMap;
use proxmox::api::schema::*;

use crate::tools;
use crate::tools::wrapped_reader_stream::*;
use crate::server::{WorkerTask, H2Service};
use crate::backup::*;
use crate::api2::types::*;

mod environment;
use environment::*;

mod upload_chunk;
use upload_chunk::*;

pub const ROUTER: Router = Router::new()
    .upgrade(&API_METHOD_UPGRADE_BACKUP);

#[sortable]
pub const API_METHOD_UPGRADE_BACKUP: ApiMethod = ApiMethod::new(
    &ApiHandler::Async(&upgrade_to_backup_protocol),
    &ObjectSchema::new(
        concat!("Upgraded to backup protocol ('", PROXMOX_BACKUP_PROTOCOL_ID_V1!(), "')."),
        &sorted!([
            ("store", false, &StringSchema::new("Datastore name.").schema()),
            ("backup-type", false, &BACKUP_TYPE_SCHEMA),
            ("backup-id", false, &BACKUP_ID_SCHEMA),
            ("backup-time", false, &BACKUP_TIME_SCHEMA),
            ("debug", true, &BooleanSchema::new("Enable verbose debug logging.").schema()),
        ]),
    )
);

fn upgrade_to_backup_protocol(
    parts: Parts,
    req_body: Body,
    param: Value,
    _info: &ApiMethod,
    rpcenv: Box<dyn RpcEnvironment>,
) -> Result<ApiFuture, Error> {

    let debug = param["debug"].as_bool().unwrap_or(false);

    let store = tools::required_string_param(&param, "store")?.to_owned();
    let datastore = DataStore::lookup_datastore(&store)?;

    let backup_type = tools::required_string_param(&param, "backup-type")?;
    let backup_id = tools::required_string_param(&param, "backup-id")?;
    let backup_time = tools::required_integer_param(&param, "backup-time")?;

    let protocols = parts
        .headers
        .get("UPGRADE")
        .ok_or_else(|| format_err!("missing Upgrade header"))?
        .to_str()?;

    if protocols != PROXMOX_BACKUP_PROTOCOL_ID_V1!() {
        bail!("invalid protocol name");
    }

    if parts.version >=  http::version::Version::HTTP_2 {
        bail!("unexpected http version '{:?}' (expected version < 2)", parts.version);
    }

    let worker_id = format!("{}_{}_{}", store, backup_type, backup_id);

    let username = rpcenv.get_user().unwrap();
    let env_type = rpcenv.env_type();

    let backup_group = BackupGroup::new(backup_type, backup_id);
    let last_backup = BackupInfo::last_backup(&datastore.base_path(), &backup_group).unwrap_or(None);
    let backup_dir = BackupDir::new_with_group(backup_group, backup_time);

    if let Some(last) = &last_backup {
        if backup_dir.backup_time() <= last.backup_dir.backup_time() {
            bail!("backup timestamp is older than last backup.");
        }
    }

    let (path, is_new) = datastore.create_backup_dir(&backup_dir)?;
    if !is_new { bail!("backup directorty already exists."); }

    WorkerTask::spawn("backup", Some(worker_id), &username.clone(), true, move |worker| {
        let mut env = BackupEnvironment::new(
            env_type, username.clone(), worker.clone(), datastore, backup_dir);

        env.debug = debug;
        env.last_backup = last_backup;

        env.log(format!("starting new backup on datastore '{}': {:?}", store, path));

        let service = H2Service::new(env.clone(), worker.clone(), &BACKUP_API_ROUTER, debug);

        let abort_future = worker.abort_future();

        let env2 = env.clone();
        let env3 = env.clone();

        let req_fut = req_body
            .on_upgrade()
            .map_err(Error::from)
            .and_then(move |conn| {
                env3.debug("protocol upgrade done");

                let mut http = hyper::server::conn::Http::new();
                http.http2_only(true);
                // increase window size: todo - find optiomal size
                let window_size = 32*1024*1024; // max = (1 << 31) - 2
                http.http2_initial_stream_window_size(window_size);
                http.http2_initial_connection_window_size(window_size);

                http.serve_connection(conn, service)
                    .map_err(Error::from)
            });
        let abort_future = abort_future
            .map(|_| Err(format_err!("task aborted")));

        use futures::future::Either;
        future::select(req_fut, abort_future)
            .map(|res| match res {
                Either::Left((Ok(res), _)) => Ok(res),
                Either::Left((Err(err), _)) => Err(err),
                Either::Right((Ok(res), _)) => Ok(res),
                Either::Right((Err(err), _)) => Err(err),
            })
            .and_then(move |_result| async move {
                env.ensure_finished()?;
                env.log("backup finished sucessfully");
                Ok(())
            })
            .then(move |result| async move {
                if let Err(err) = result {
                    match env2.ensure_finished() {
                        Ok(()) => {}, // ignore error after finish
                        _ => {
                            env2.log(format!("backup failed: {}", err));
                            env2.log("removing failed backup");
                            env2.remove_backup()?;
                            return Err(err);
                        }
                    }
                }
                Ok(())
            })
    })?;

    let response = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(UPGRADE, HeaderValue::from_static(PROXMOX_BACKUP_PROTOCOL_ID_V1!()))
        .body(Body::empty())?;

    Ok(Box::new(futures::future::ok(response)))
}

pub const BACKUP_API_SUBDIRS: SubdirMap = &[
    (
        "blob", &Router::new()
            .upload(&API_METHOD_UPLOAD_BLOB)
    ),
    (
        "dynamic_chunk", &Router::new()
            .upload(&API_METHOD_UPLOAD_DYNAMIC_CHUNK)
    ),
    (
        "dynamic_close", &Router::new()
            .post(&API_METHOD_CLOSE_DYNAMIC_INDEX)
    ),
    (
        "dynamic_index", &Router::new()
            .download(&API_METHOD_DYNAMIC_CHUNK_INDEX)
            .post(&API_METHOD_CREATE_DYNAMIC_INDEX)
            .put(&API_METHOD_DYNAMIC_APPEND)
    ),
    (
        "finish", &Router::new()
            .post(
                &ApiMethod::new(
                    &ApiHandler::Sync(&finish_backup),
                    &ObjectSchema::new("Mark backup as finished.", &[])
                )
            )
    ),
    (
        "fixed_chunk", &Router::new()
            .upload(&API_METHOD_UPLOAD_FIXED_CHUNK)
    ),
    (
        "fixed_close", &Router::new()
            .post(&API_METHOD_CLOSE_FIXED_INDEX)
    ),
    (
        "fixed_index", &Router::new()
            .download(&API_METHOD_FIXED_CHUNK_INDEX)
            .post(&API_METHOD_CREATE_FIXED_INDEX)
            .put(&API_METHOD_FIXED_APPEND)
    ),
    (
        "speedtest", &Router::new()
            .upload(&API_METHOD_UPLOAD_SPEEDTEST)
    ),
];

pub const BACKUP_API_ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(BACKUP_API_SUBDIRS))
    .subdirs(BACKUP_API_SUBDIRS);

#[sortable]
pub const API_METHOD_CREATE_DYNAMIC_INDEX: ApiMethod = ApiMethod::new(
    &ApiHandler::Sync(&create_dynamic_index),
    &ObjectSchema::new(
        "Create dynamic chunk index file.",
        &sorted!([
            ("archive-name", false, &crate::api2::types::BACKUP_ARCHIVE_NAME_SCHEMA),
        ]),
    )
);

fn create_dynamic_index(
    param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let env: &BackupEnvironment = rpcenv.as_ref();

    let name = tools::required_string_param(&param, "archive-name")?.to_owned();

    let archive_name = name.clone();
    if !archive_name.ends_with(".didx") {
        bail!("wrong archive extension: '{}'", archive_name);
    }

    let mut path = env.backup_dir.relative_path();
    path.push(archive_name);

    let index = env.datastore.create_dynamic_writer(&path)?;
    let wid = env.register_dynamic_writer(index, name)?;

    env.log(format!("created new dynamic index {} ({:?})", wid, path));

    Ok(json!(wid))
}

#[sortable]
pub const API_METHOD_CREATE_FIXED_INDEX: ApiMethod = ApiMethod::new(
    &ApiHandler::Sync(&create_fixed_index),
    &ObjectSchema::new(
        "Create fixed chunk index file.",
        &sorted!([
            ("archive-name", false, &crate::api2::types::BACKUP_ARCHIVE_NAME_SCHEMA),
            ("size", false, &IntegerSchema::new("File size.")
             .minimum(1)
             .schema()
            ),
        ]),
    )
);

fn create_fixed_index(
    param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let env: &BackupEnvironment = rpcenv.as_ref();

    println!("PARAM: {:?}", param);

    let name = tools::required_string_param(&param, "archive-name")?.to_owned();
    let size = tools::required_integer_param(&param, "size")? as usize;

    let archive_name = name.clone();
    if !archive_name.ends_with(".fidx") {
        bail!("wrong archive extension: '{}'", archive_name);
    }

    let mut path = env.backup_dir.relative_path();
    path.push(archive_name);

    let chunk_size = 4096*1024; // todo: ??

    let index = env.datastore.create_fixed_writer(&path, size, chunk_size)?;
    let wid = env.register_fixed_writer(index, name, size, chunk_size as u32)?;

    env.log(format!("created new fixed index {} ({:?})", wid, path));

    Ok(json!(wid))
}

#[sortable]
pub const API_METHOD_DYNAMIC_APPEND: ApiMethod = ApiMethod::new(
    &ApiHandler::Sync(&dynamic_append),
    &ObjectSchema::new(
        "Append chunk to dynamic index writer.",
        &sorted!([
            (
                "wid",
                false,
                &IntegerSchema::new("Dynamic writer ID.")
                    .minimum(1)
                    .maximum(256)
                    .schema()
            ),
            (
                "digest-list",
                false,
                &ArraySchema::new("Chunk digest list.", &CHUNK_DIGEST_SCHEMA).schema()
            ),
            (
                "offset-list",
                false,
                &ArraySchema::new(
                    "Chunk offset list.",
                    &IntegerSchema::new("Corresponding chunk offsets.")
                        .minimum(0)
                        .schema()
                ).schema()
            ),
        ]),
    )
);

fn dynamic_append (
    param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let wid = tools::required_integer_param(&param, "wid")? as usize;
    let digest_list = tools::required_array_param(&param, "digest-list")?;
    let offset_list = tools::required_array_param(&param, "offset-list")?;

    if offset_list.len() != digest_list.len() {
        bail!("offset list has wrong length ({} != {})", offset_list.len(), digest_list.len());
    }

    let env: &BackupEnvironment = rpcenv.as_ref();

    env.debug(format!("dynamic_append {} chunks", digest_list.len()));

    for (i, item) in digest_list.iter().enumerate() {
        let digest_str = item.as_str().unwrap();
        let digest = proxmox::tools::hex_to_digest(digest_str)?;
        let offset = offset_list[i].as_u64().unwrap();
        let size = env.lookup_chunk(&digest).ok_or_else(|| format_err!("no such chunk {}", digest_str))?;

        env.dynamic_writer_append_chunk(wid, offset, size, &digest)?;

        env.debug(format!("sucessfully added chunk {} to dynamic index {} (offset {}, size {})", digest_str, wid, offset, size));
    }

    Ok(Value::Null)
}

#[sortable]
pub const API_METHOD_FIXED_APPEND: ApiMethod = ApiMethod::new(
    &ApiHandler::Sync(&fixed_append),
    &ObjectSchema::new(
        "Append chunk to fixed index writer.",
        &sorted!([
            (
                "wid",
                false,
                &IntegerSchema::new("Fixed writer ID.")
                    .minimum(1)
                    .maximum(256)
                    .schema()
            ),
            (
                "digest-list",
                false,
                &ArraySchema::new("Chunk digest list.", &CHUNK_DIGEST_SCHEMA).schema()
            ),
            (
                "offset-list",
                false,
                &ArraySchema::new(
                    "Chunk offset list.",
                    &IntegerSchema::new("Corresponding chunk offsets.")
                        .minimum(0)
                        .schema()
                ).schema()
            )
        ]),
    )
);

fn fixed_append (
    param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let wid = tools::required_integer_param(&param, "wid")? as usize;
    let digest_list = tools::required_array_param(&param, "digest-list")?;
    let offset_list = tools::required_array_param(&param, "offset-list")?;

    if offset_list.len() != digest_list.len() {
        bail!("offset list has wrong length ({} != {})", offset_list.len(), digest_list.len());
    }

    let env: &BackupEnvironment = rpcenv.as_ref();

    env.debug(format!("fixed_append {} chunks", digest_list.len()));

    for (i, item) in digest_list.iter().enumerate() {
        let digest_str = item.as_str().unwrap();
        let digest = proxmox::tools::hex_to_digest(digest_str)?;
        let offset = offset_list[i].as_u64().unwrap();
        let size = env.lookup_chunk(&digest).ok_or_else(|| format_err!("no such chunk {}", digest_str))?;

        env.fixed_writer_append_chunk(wid, offset, size, &digest)?;

        env.debug(format!("sucessfully added chunk {} to fixed index {} (offset {}, size {})", digest_str, wid, offset, size));
    }

    Ok(Value::Null)
}

#[sortable]
pub const API_METHOD_CLOSE_DYNAMIC_INDEX: ApiMethod = ApiMethod::new(
    &ApiHandler::Sync(&close_dynamic_index),
    &ObjectSchema::new(
        "Close dynamic index writer.",
        &sorted!([
            (
                "wid",
                false,
                &IntegerSchema::new("Dynamic writer ID.")
                    .minimum(1)
                    .maximum(256)
                    .schema()
            ),
            (
                "chunk-count",
                false,
                &IntegerSchema::new("Chunk count. This is used to verify that the server got all chunks.")
                    .minimum(1)
                    .schema()
            ),
            (
                "size",
                false,
                &IntegerSchema::new("File size. This is used to verify that the server got all data.")
                    .minimum(1)
                    .schema()
            ),
            ("csum", false, &StringSchema::new("Digest list checksum.").schema()),
        ]),
    )
);

fn close_dynamic_index (
    param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let wid = tools::required_integer_param(&param, "wid")? as usize;
    let chunk_count = tools::required_integer_param(&param, "chunk-count")? as u64;
    let size = tools::required_integer_param(&param, "size")? as u64;
    let csum_str = tools::required_string_param(&param, "csum")?;
    let csum = proxmox::tools::hex_to_digest(csum_str)?;

    let env: &BackupEnvironment = rpcenv.as_ref();

    env.dynamic_writer_close(wid, chunk_count, size, csum)?;

    env.log(format!("sucessfully closed dynamic index {}", wid));

    Ok(Value::Null)
}

#[sortable]
pub const API_METHOD_CLOSE_FIXED_INDEX: ApiMethod = ApiMethod::new(
    &ApiHandler::Sync(&close_fixed_index),
    &ObjectSchema::new(
        "Close fixed index writer.",
        &sorted!([
            (
                "wid",
                false,
                &IntegerSchema::new("Fixed writer ID.")
                    .minimum(1)
                    .maximum(256)
                    .schema()
            ),
            (
                "chunk-count",
                false,
                &IntegerSchema::new("Chunk count. This is used to verify that the server got all chunks.")
                    .minimum(1)
                    .schema()
            ),
            (
                "size",
                false,
                &IntegerSchema::new("File size. This is used to verify that the server got all data.")
                    .minimum(1)
                    .schema()
            ),
            ("csum", false, &StringSchema::new("Digest list checksum.").schema()),
        ]),
    )
);

fn close_fixed_index (
    param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let wid = tools::required_integer_param(&param, "wid")? as usize;
    let chunk_count = tools::required_integer_param(&param, "chunk-count")? as u64;
    let size = tools::required_integer_param(&param, "size")? as u64;
    let csum_str = tools::required_string_param(&param, "csum")?;
    let csum = proxmox::tools::hex_to_digest(csum_str)?;

    let env: &BackupEnvironment = rpcenv.as_ref();

    env.fixed_writer_close(wid, chunk_count, size, csum)?;

    env.log(format!("sucessfully closed fixed index {}", wid));

    Ok(Value::Null)
}

fn finish_backup (
    _param: Value,
    _info: &ApiMethod,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {

    let env: &BackupEnvironment = rpcenv.as_ref();

    env.finish_backup()?;
    env.log("sucessfully finished backup");

    Ok(Value::Null)
}

#[sortable]
pub const API_METHOD_DYNAMIC_CHUNK_INDEX: ApiMethod = ApiMethod::new(
    &ApiHandler::Async(&dynamic_chunk_index),
    &ObjectSchema::new(
        r###"
Download the dynamic chunk index from the previous backup.
Simply returns an empty list if this is the first backup.
"### ,
        &sorted!([
            ("archive-name", false, &crate::api2::types::BACKUP_ARCHIVE_NAME_SCHEMA)
        ]),
    )
);

fn dynamic_chunk_index(
    _parts: Parts,
    _req_body: Body,
    param: Value,
    _info: &ApiMethod,
    rpcenv: Box<dyn RpcEnvironment>,
) -> Result<ApiFuture, Error> {

    let env: &BackupEnvironment = rpcenv.as_ref();

    let archive_name = tools::required_string_param(&param, "archive-name")?.to_owned();

    if !archive_name.ends_with(".didx") {
        bail!("wrong archive extension: '{}'", archive_name);
    }

    let empty_response = {
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::empty())?
    };

    let last_backup = match &env.last_backup {
        Some(info) => info,
        None => return Ok(Box::new(future::ok(empty_response))),
    };

    let mut path = last_backup.backup_dir.relative_path();
    path.push(&archive_name);

    let index = match env.datastore.open_dynamic_reader(path) {
        Ok(index) => index,
        Err(_) => {
            env.log(format!("there is no last backup for archive '{}'", archive_name));
            return Ok(Box::new(future::ok(empty_response)));
        }
    };

    env.log(format!("download last backup index for archive '{}'", archive_name));

    let count = index.index_count();
    for pos in 0..count {
        let (start, end, digest) = index.chunk_info(pos)?;
        let size = (end - start) as u32;
        env.register_chunk(digest, size)?;
    }

    let reader = DigestListEncoder::new(Box::new(index));

    let stream = WrappedReaderStream::new(reader);

    // fixme: set size, content type?
    let response = http::Response::builder()
        .status(200)
        .body(Body::wrap_stream(stream))?;

    Ok(Box::new(future::ok(response)))
}

#[sortable]
pub const API_METHOD_FIXED_CHUNK_INDEX: ApiMethod = ApiMethod::new(
    &ApiHandler::Async(&fixed_chunk_index),
    &ObjectSchema::new(
        r###"
Download the fixed chunk index from the previous backup.
Simply returns an empty list if this is the first backup.
"### ,
        &sorted!([
            ("archive-name", false, &crate::api2::types::BACKUP_ARCHIVE_NAME_SCHEMA)
        ]),
    )
);

fn fixed_chunk_index(
    _parts: Parts,
    _req_body: Body,
    param: Value,
    _info: &ApiMethod,
    rpcenv: Box<dyn RpcEnvironment>,
) -> Result<ApiFuture, Error> {

    let env: &BackupEnvironment = rpcenv.as_ref();

    let archive_name = tools::required_string_param(&param, "archive-name")?.to_owned();

    if !archive_name.ends_with(".fidx") {
        bail!("wrong archive extension: '{}'", archive_name);
    }

    let empty_response = {
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::empty())?
    };

    let last_backup = match &env.last_backup {
        Some(info) => info,
        None => return Ok(Box::new(future::ok(empty_response))),
    };

    let mut path = last_backup.backup_dir.relative_path();
    path.push(&archive_name);

    let index = match env.datastore.open_fixed_reader(path) {
        Ok(index) => index,
        Err(_) => {
            env.log(format!("there is no last backup for archive '{}'", archive_name));
            return Ok(Box::new(future::ok(empty_response)));
        }
    };

    env.log(format!("download last backup index for archive '{}'", archive_name));

    let count = index.index_count();
    let image_size = index.index_bytes();
    for pos in 0..count {
        let digest = index.index_digest(pos).unwrap();
        // Note: last chunk can be smaller
        let start = (pos*index.chunk_size) as u64;
        let mut end = start + index.chunk_size as u64;
        if end > image_size { end = image_size; }
        let size = (end - start) as u32;
        env.register_chunk(*digest, size)?;
    }

    let reader = DigestListEncoder::new(Box::new(index));

    let stream = WrappedReaderStream::new(reader);

    // fixme: set size, content type?
    let response = http::Response::builder()
        .status(200)
        .body(Body::wrap_stream(stream))?;

    Ok(Box::new(future::ok(response)))
}
