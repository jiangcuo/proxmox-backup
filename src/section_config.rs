use failure::*;

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;

use serde_json::{json, Value};
use serde::de::DeserializeOwned;
use serde::ser::Serialize;

use proxmox::api::schema::*;
use proxmox::try_block;

pub struct SectionConfigPlugin {
    type_name: String,
    properties: &'static ObjectSchema,
}

impl SectionConfigPlugin {

    pub fn new(type_name: String, properties: &'static ObjectSchema) -> Self {
        Self { type_name, properties }
    }

}

pub struct SectionConfig {
    plugins: HashMap<String, SectionConfigPlugin>,

    id_schema: &'static Schema,
    parse_section_header: fn(&str) -> Option<(String, String)>,
    parse_section_content: fn(&str) -> Option<(String, String)>,
    format_section_header: fn(type_name: &str, section_id: &str, data: &Value) -> String,
}

enum ParseState<'a> {
    BeforeHeader,
    InsideSection(&'a SectionConfigPlugin, String, Value),
}

#[derive(Debug)]
pub struct SectionConfigData {
    pub sections: HashMap<String, (String, Value)>,
    order: VecDeque<String>,
}

impl SectionConfigData {

    pub fn new() -> Self {
        Self { sections: HashMap::new(), order: VecDeque::new() }
    }

    pub fn set_data<T: Serialize>(
        &mut self,
        section_id: &str,
        type_name: &str,
        config: T,
    ) -> Result<(), Error> {
        let json = serde_json::to_value(config)?;
        self.sections.insert(section_id.to_string(), (type_name.to_string(), json));
        Ok(())
    }

    pub fn lookup_json(&self, type_name: &str, id: &str) -> Result<Value, Error> {
        match self.sections.get(id) {
            Some((section_type_name, config)) => {
                if type_name != section_type_name {
                    bail!("got unexpected type '{}' for {} '{}'", section_type_name, type_name, id);
                }
                Ok(config.clone())
            }
            None => {
                bail!("no such {} '{}'", type_name, id);
            }
        }
    }

    pub fn lookup<T: DeserializeOwned>(&self, type_name: &str, id: &str) -> Result<T, Error> {
        let config = self.lookup_json(type_name, id)?;
        let data = T::deserialize(config)?;
        Ok(data)
    }

    fn record_order(&mut self, section_id: &str) {
        self.order.push_back(section_id.to_string());
    }

    pub fn convert_to_array(&self, id_prop: &str, digest: Option<&[u8;32]>) -> Value {
        let mut list: Vec<Value> = vec![];

        let digest: Value = match digest {
            Some(v) => proxmox::tools::digest_to_hex(v).into(),
            None => Value::Null,
        };

        for (section_id, (_, data)) in &self.sections {
            let mut item = data.clone();
            item.as_object_mut().unwrap().insert(id_prop.into(), section_id.clone().into());
            if !digest.is_null() {
                item.as_object_mut().unwrap().insert("digest".into(), digest.clone());
            }
            list.push(item);
        }

        list.into()
    }
}

impl SectionConfig {

    pub fn new(id_schema: &'static Schema) -> Self {
        Self {
            plugins: HashMap::new(),
            id_schema,
            parse_section_header: SectionConfig::default_parse_section_header,
            parse_section_content: SectionConfig::default_parse_section_content,
            format_section_header: SectionConfig::default_format_section_header,
        }
    }

    pub fn register_plugin(&mut self, plugin: SectionConfigPlugin) {
        self.plugins.insert(plugin.type_name.clone(), plugin);
    }

    pub fn write(&self, filename: &str, config: &SectionConfigData) -> Result<String, Error> {

        try_block!({
            let mut list = VecDeque::new();

            let mut done = HashSet::new();

            for section_id in &config.order {
                if config.sections.get(section_id) == None { continue };
                list.push_back(section_id);
                done.insert(section_id);
            }

            for (section_id, _) in &config.sections {
                if done.contains(section_id) { continue };
                list.push_back(section_id);
            }

            let mut raw = String::new();

            for section_id in list {
                let (type_name, section_config) = config.sections.get(section_id).unwrap();
                let plugin = self.plugins.get(type_name).unwrap();

                if let Err(err) = parse_simple_value(&section_id, &self.id_schema) {
                    bail!("syntax error in section identifier: {}", err.to_string());
                }
                if section_id.chars().any(|c| c.is_control()) {
                    bail!("detected unexpected control character in section ID.");
                }

                verify_json_object(section_config, &plugin.properties)?;

                let head = (self.format_section_header)(type_name, section_id, section_config);

                if !raw.is_empty() { raw += "\n" }

                raw += &head;

                for (key, value) in section_config.as_object().unwrap() {
                    let text = match value {
                        Value::Null => { continue; }, // do nothing (delete)
                        Value::Bool(v) => v.to_string(),
                        Value::String(v) => v.to_string(),
                        Value::Number(v) => v.to_string(),
                        _ => {
                            bail!("got unsupported type in section '{}' key '{}'", section_id, key);
                        },
                    };
                    if text.chars().any(|c| c.is_control()) {
                        bail!("detected unexpected control character in section '{}' key '{}'", section_id, key);
                    }
                    raw += "\t";
                    raw += &key;
                    raw += " ";
                    raw += &text;
                    raw += "\n";
                }
            }

            Ok(raw)
        }).map_err(|e: Error| format_err!("writing '{}' failed: {}", filename, e))
    }

    pub fn parse(&self, filename: &str, raw: &str) -> Result<SectionConfigData, Error> {

        let mut state = ParseState::BeforeHeader;

        let test_required_properties = |value: &Value, schema: &ObjectSchema| -> Result<(), Error> {
            for (name, optional, _prop_schema) in schema.properties {
                if *optional == false && value[name] == Value::Null {
                    return Err(format_err!("property '{}' is missing and it is not optional.", name));
                }
            }
            Ok(())
        };

        let mut line_no = 0;

        try_block!({

            let mut result = SectionConfigData::new();

            try_block!({
                for line in raw.lines() {
                    line_no += 1;

                    match state {

                        ParseState::BeforeHeader => {

                            if line.trim().is_empty() { continue; }

                            if let Some((section_type, section_id)) = (self.parse_section_header)(line) {
                                //println!("OKLINE: type: {} ID: {}", section_type, section_id);
                                if let Some(ref plugin) = self.plugins.get(&section_type) {
                                    if let Err(err) = parse_simple_value(&section_id, &self.id_schema) {
                                        bail!("syntax error in section identifier: {}", err.to_string());
                                    }
                                    state = ParseState::InsideSection(plugin, section_id, json!({}));
                                } else {
                                    bail!("unknown section type '{}'", section_type);
                                }
                            } else {
                                bail!("syntax error (expected header)");
                            }
                        }
                        ParseState::InsideSection(plugin, ref mut section_id, ref mut config) => {

                            if line.trim().is_empty() {
                                // finish section
                                test_required_properties(config, &plugin.properties)?;
                                result.set_data(section_id, &plugin.type_name, config.take())?;
                                result.record_order(section_id);

                                state = ParseState::BeforeHeader;
                                continue;
                            }
                            if let Some((key, value)) = (self.parse_section_content)(line) {
                                //println!("CONTENT: key: {} value: {}", key, value);

                                if let Some((_optional, prop_schema)) = plugin.properties.lookup(&key) {
                                    match parse_simple_value(&value, prop_schema) {
                                        Ok(value) => {
                                            if config[&key] == Value::Null {
                                                config[key] = value;
                                            } else {
                                                bail!("duplicate property '{}'", key);
                                            }
                                        }
                                        Err(err) => {
                                            bail!("property '{}': {}", key, err.to_string());
                                        }
                                    }
                                } else {
                                    bail!("unknown property '{}'", key)
                                }
                            } else {
                                bail!("syntax error (expected section properties)");
                            }
                        }
                    }
                }

                if let ParseState::InsideSection(plugin, section_id, config) = state {
                    // finish section
                    test_required_properties(&config, &plugin.properties)?;
                    result.set_data(&section_id, &plugin.type_name, config)?;
                    result.record_order(&section_id);
                }

                Ok(())

            }).map_err(|e| format_err!("line {} - {}", line_no, e))?;

            Ok(result)

        }).map_err(|e: Error| format_err!("parsing '{}' failed: {}", filename, e))
    }

    pub fn default_format_section_header(type_name: &str, section_id: &str, _data: &Value) -> String {
        return format!("{}: {}\n", type_name, section_id);
    }

    pub fn default_parse_section_content(line: &str) -> Option<(String, String)> {

        if line.is_empty() { return None; }
        let first_char = line.chars().next().unwrap();

        if !first_char.is_whitespace() { return None }

        let mut kv_iter = line.trim_start().splitn(2, |c: char| c.is_whitespace());

        let key = match kv_iter.next() {
            Some(v) => v.trim(),
            None => return None,
        };

        if key.len() == 0 { return None; }

        let value = match kv_iter.next() {
            Some(v) => v.trim(),
            None => return None,
        };

        Some((key.into(), value.into()))
   }

    pub fn default_parse_section_header(line: &str) -> Option<(String, String)> {

        if line.is_empty() { return None; };

        let first_char = line.chars().next().unwrap();

        if !first_char.is_alphabetic() { return None }

        let mut head_iter = line.splitn(2, ':');

        let section_type = match head_iter.next() {
            Some(v) => v.trim(),
            None => return None,
        };

        if section_type.len() == 0 { return None; }

        let section_id = match head_iter.next() {
            Some(v) => v.trim(),
            None => return None,
        };

        Some((section_type.into(), section_id.into()))
    }
}

// cargo test test_section_config1 -- --nocapture
#[test]
fn test_section_config1() {

    let filename = "storage.cfg";

    //let mut file = File::open(filename).expect("file not found");
    //let mut contents = String::new();
    //file.read_to_string(&mut contents).unwrap();

    const PROPERTIES: ObjectSchema = ObjectSchema::new(
        "lvmthin properties",
        &[
            ("content", true, &StringSchema::new("Storage content types.").schema()),
            ("thinpool", false, &StringSchema::new("LVM thin pool name.").schema()),
            ("vgname", false, &StringSchema::new("LVM volume group name.").schema()),
        ],
    );

    let plugin = SectionConfigPlugin::new("lvmthin".to_string(), &PROPERTIES);

    const ID_SCHEMA: Schema = StringSchema::new("Storage ID schema.")
        .min_length(3)
        .schema();

    let mut config = SectionConfig::new(&ID_SCHEMA);
    config.register_plugin(plugin);

    let raw = r"

lvmthin: local-lvm
        thinpool data
        vgname pve5
        content rootdir,images

lvmthin: local-lvm2
        thinpool data
        vgname pve5
        content rootdir,images
";

    let res = config.parse(filename, &raw);
    println!("RES: {:?}", res);
    let raw = config.write(filename, &res.unwrap());
    println!("CONFIG:\n{}", raw.unwrap());


}
