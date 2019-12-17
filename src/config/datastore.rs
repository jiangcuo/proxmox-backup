use std::collections::HashMap;
use std::io::Read;

use failure::*;
use lazy_static::lazy_static;

use proxmox::tools::{fs::file_set_contents_full, try_block};
use proxmox::api::schema::{Schema, ObjectSchema, StringSchema};

use crate::section_config::{SectionConfig, SectionConfigData, SectionConfigPlugin};

lazy_static! {
    static ref CONFIG: SectionConfig = init();
}

const DIR_NAME_SCHEMA: Schema = StringSchema::new("Directory name").schema();
const DATASTORE_ID_SCHEMA: Schema = StringSchema::new("DataStore ID schema.")
    .min_length(3)
    .schema();
const DATASTORE_PROPERTIES: ObjectSchema = ObjectSchema::new(
    "DataStore properties",
    &[
        ("path", false, &DIR_NAME_SCHEMA)
    ]
);

fn init() -> SectionConfig {
    let plugin = SectionConfigPlugin::new("datastore".to_string(), &DATASTORE_PROPERTIES);
    let mut config = SectionConfig::new(&DATASTORE_ID_SCHEMA);
    config.register_plugin(plugin);

    config
}

const DATASTORE_CFG_FILENAME: &str = "/etc/proxmox-backup/datastore.cfg";

pub fn config() -> Result<SectionConfigData, Error> {
    let mut contents = String::new();

    try_block!({
        match std::fs::File::open(DATASTORE_CFG_FILENAME) {
            Ok(mut file) => file.read_to_string(&mut contents),
            Err(err) => {
                if err.kind() == std::io::ErrorKind::NotFound {
                    contents = String::from("");
                    Ok(0)
                } else {
                    Err(err)
                }
            }
        }
    })
    .map_err(|e| format_err!("unable to read '{}' - {}", DATASTORE_CFG_FILENAME, e))?;

    CONFIG.parse(DATASTORE_CFG_FILENAME, &contents)
}

pub fn save_config(config: &SectionConfigData) -> Result<(), Error> {
    let raw = CONFIG.write(DATASTORE_CFG_FILENAME, &config)?;

    let (backup_uid, _) = crate::tools::getpwnam_ugid("backup")?;
    let uid = nix::unistd::Uid::from_raw(backup_uid);

    // manager runs as root, so we need to set the correct owner while saving file (backup:root)
    file_set_contents_full(DATASTORE_CFG_FILENAME, raw.as_bytes(), None, Some(uid), None)?;

    Ok(())
}

// shell completion helper
pub fn complete_datastore_name(_arg: &str, _param: &HashMap<String, String>) -> Vec<String> {
    match config() {
        Ok(data) => data.sections.iter().map(|(id, _)| id.to_string()).collect(),
        Err(_) => return vec![],
    }
}
