use std::process::{Command, Stdio};

use anyhow::{bail, Error};
use serde_json::{json, Value};

use proxmox::{sortable, identity, list_subdirs_api_method};
use proxmox::api::{api, Router, Permission};
use proxmox::api::router::SubdirMap;
use proxmox::api::schema::*;

use crate::api2::types::*;
use crate::config::acl::{PRIV_SYS_AUDIT, PRIV_SYS_MODIFY};

static SERVICE_NAME_LIST: [&str; 7] = [
    "proxmox-backup",
    "proxmox-backup-proxy",
    "sshd",
    "syslog",
    "cron",
    "postfix",
    "systemd-timesyncd",
];

fn real_service_name(service: &str) -> &str {

    // since postfix package 3.1.0-3.1 the postfix unit is only here
    // to manage subinstances, of which the default is called "-".
    // This is where we look for the daemon status

    if service == "postfix" {
        "postfix@-"
    } else {
        service
    }
}

fn get_full_service_state(service: &str) -> Result<Value, Error> {

    let real_service_name = real_service_name(service);

    let mut child = Command::new("systemctl")
        .args(&["show", real_service_name])
        .stdout(Stdio::piped())
        .spawn()?;

    use std::io::{BufRead,BufReader};

    let mut result = json!({});

    if let Some(ref mut stdout) = child.stdout {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    let mut iter = line.splitn(2, '=');
                    let key = iter.next();
                    let value = iter.next();
                    if let (Some(key), Some(value)) = (key, value) {
                        result[key] = Value::from(value);
                    }
                }
                Err(err) => {
                    log::error!("reading service config failed: {}", err);
                    let _ = child.kill();
                    break;
                }
            }
        }
    }

    let status = child.wait().unwrap();
    if !status.success() {
        bail!("systemctl show failed with {}", status);
    }

    Ok(result)
}

fn json_service_state(service: &str, status: Value) -> Value {

    if let Some(desc) = status["Description"].as_str() {
        let name = status["Name"].as_str().unwrap_or(service);
        let state = status["SubState"].as_str().unwrap_or("unknown");
        return json!({
            "service": service,
            "name": name,
            "desc": desc,
            "state": state,
        });
    }

    Value::Null
}

#[api(
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
        },
    },
    returns: {
        description: "Returns a list of systemd services.",
        type: Array,
        items: {
            description: "Service details.",
            properties: {
                service: {
                    schema: SERVICE_ID_SCHEMA,
                },
                name: {
                    type: String,
                    description: "systemd service name.",
                },
                desc: {
                    type: String,
                    description: "systemd service description.",
                },
                state: {
                    type: String,
                    description: "systemd service 'SubState'.",
                },
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "services"], PRIV_SYS_AUDIT, false),
    },
)]
/// Service list.
fn list_services(
    _param: Value,
) -> Result<Value, Error> {

    let mut list = vec![];

    for service in &SERVICE_NAME_LIST {
        match get_full_service_state(service) {
            Ok(status) => {
                let state = json_service_state(service, status);
                if state != Value::Null {
                    list.push(state);
                }
            }
            Err(err) => log::error!("{}", err),
        }
    }

    Ok(Value::from(list))
}

#[api(
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            service: {
                schema: SERVICE_ID_SCHEMA,
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "services", "{service}"], PRIV_SYS_AUDIT, false),
    },
)]
/// Read service properties.
fn get_service_state(
    service: String,
    _param: Value,
) -> Result<Value, Error> {

    let service = service.as_str();

    if !SERVICE_NAME_LIST.contains(&service) {
        bail!("unknown service name '{}'", service);
    }

    let status = get_full_service_state(&service)?;

    Ok(json_service_state(&service, status))
}

fn run_service_command(service: &str, cmd: &str) -> Result<Value, Error> {

    // fixme: run background worker (fork_worker) ???

    let cmd = match cmd {
        "start"|"stop"|"restart"=> cmd,
        "reload" => "try-reload-or-restart", // some services do not implement reload
        _ => bail!("unknown service command '{}'", cmd),
    };

    if service == "proxmox-backup" && cmd == "stop" {
        bail!("invalid service cmd '{} {}' cannot stop essential service!", service, cmd);
    }

    let real_service_name = real_service_name(service);

    let status = Command::new("systemctl")
        .args(&[cmd, real_service_name])
        .status()?;

    if !status.success() {
        bail!("systemctl {} failed with {}", cmd, status);
    }

    Ok(Value::Null)
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            service: {
                schema: SERVICE_ID_SCHEMA,
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "services", "{service}"], PRIV_SYS_MODIFY, false),
    },
)]
/// Start service.
fn start_service(
    service: String,
    _param: Value,
) -> Result<Value, Error> {

    log::info!("starting service {}", service);

    run_service_command(&service, "start")
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            service: {
                schema: SERVICE_ID_SCHEMA,
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "services", "{service}"], PRIV_SYS_MODIFY, false),
    },
)]
/// Stop service.
fn stop_service(
    service: String,
    _param: Value,
 ) -> Result<Value, Error> {

    log::info!("stopping service {}", service);

    run_service_command(&service, "stop")
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            service: {
                schema: SERVICE_ID_SCHEMA,
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "services", "{service}"], PRIV_SYS_MODIFY, false),
    },
)]
/// Retart service.
fn restart_service(
    service: String,
    _param: Value,
) -> Result<Value, Error> {

    log::info!("re-starting service {}", service);

    if &service == "proxmox-backup-proxy" {
        // special case, avoid aborting running tasks
        run_service_command(&service, "reload")
    } else {
        run_service_command(&service, "restart")
    }
}

#[api(
    protected: true,
    input: {
        properties: {
            node: {
                schema: NODE_SCHEMA,
            },
            service: {
                schema: SERVICE_ID_SCHEMA,
            },
        },
    },
    access: {
        permission: &Permission::Privilege(&["system", "services", "{service}"], PRIV_SYS_MODIFY, false),
    },
)]
/// Reload service.
fn reload_service(
    service: String,
    _param: Value,
) -> Result<Value, Error> {

    log::info!("reloading service {}", service);

    run_service_command(&service, "reload")
}


const SERVICE_ID_SCHEMA: Schema = StringSchema::new("Service ID.")
    .max_length(256)
    .schema();

#[sortable]
const SERVICE_SUBDIRS: SubdirMap = &sorted!([
    (
        "reload", &Router::new()
            .post(&API_METHOD_RELOAD_SERVICE)
    ),
    (
        "restart", &Router::new()
            .post(&API_METHOD_RESTART_SERVICE)
    ),
    (
        "start", &Router::new()
            .post(&API_METHOD_START_SERVICE)
    ),
    (
        "state", &Router::new()
            .get(&API_METHOD_GET_SERVICE_STATE)
    ),
    (
        "stop", &Router::new()
            .post(&API_METHOD_STOP_SERVICE)
    ),
]);

const SERVICE_ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(SERVICE_SUBDIRS))
    .subdirs(SERVICE_SUBDIRS);

pub const ROUTER: Router = Router::new()
    .get(&API_METHOD_LIST_SERVICES)
    .match_all("service", &SERVICE_ROUTER);
