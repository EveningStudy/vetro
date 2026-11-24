use std::{
    collections::BTreeMap,
    fmt::Write,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde_json::{json, Value};
use vetro_agent_api::{AgentError, Result, SessionRecord};
use vetro_proto::{
    rpc::ErrorCode, ConfigCategory, ConfigChoice, ConfigOption, ContentBlock, SlashCommand,
    ThreadSource, ToolCall, ToolContent, ToolKind, ToolLocation, ToolStatus,
};

pub fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

pub fn array(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

pub fn timestamp(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let instant = humantime::parse_rfc3339_weak(value.as_str()?).ok()?;
        i64::try_from(instant.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
    })
}

pub fn record(agent_id: &str, value: &Value, resumable: bool) -> Option<SessionRecord> {
    let native_id = string(value, "sessionId")?;
    let record = SessionRecord {
        title: string(value, "title").unwrap_or_else(|| native_id.clone()),
        native_id,
        preview: string(value, "preview"),
        cwd: value["cwd"]
            .as_str()
            .and_then(|cwd| native_cwd(cwd, cfg!(windows))),
        created_at: timestamp(&value["createdAt"]),
        updated_at: timestamp(&value["updatedAt"]).unwrap_or_default(),
        source: ThreadSource::Unknown,
        origin: None,
        model: string(value, "model"),
        resumable,
        parent_native_id: string(value, "parentSessionId"),
        internal: false,
    };
    Some(record.classify(agent_id))
}

fn native_cwd(cwd: &str, windows: bool) -> Option<PathBuf> {
    let (authority, path) = if let Some(uri) = cwd.strip_prefix("file://") {
        uri.split_once('/')
            .map(|(host, _)| (host, &uri[host.len()..]))?
    } else if let Some(path) = cwd.strip_prefix("file:") {
        ("", path)
    } else if cwd.starts_with('/') {
        ("", cwd)
    } else {
        let path = PathBuf::from(cwd);
        return path.is_absolute().then_some(path);
    };
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    if decoded.contains('\0') {
        return None;
    }
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return (windows && !authority.contains(['\\', ':', '@']))
            .then(|| PathBuf::from(format!(r"\\{authority}{}", decoded.replace('/', "\\"))));
    }
    let path = decoded.as_ref();
    if windows {
        let path = path
            .strip_prefix('/')
            .filter(|drive| {
                let bytes = drive.as_bytes();
                bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && bytes[2] == b'/'
            })
            .unwrap_or(path);
        let path = PathBuf::from(path.replace('/', "\\"));
        path.is_absolute().then_some(path)
    } else {
        let path = PathBuf::from(path);
        path.is_absolute().then_some(path)
    }
}

#[cfg(test)]
mod cwd_tests {
    use super::*;

    #[test]
    fn decodes_absolute_paths_and_ignores_missing_directories() {
        let (uri, expected) = if cfg!(windows) {
            (
                "file://localhost/d%3A/Dev/My%20Project",
                PathBuf::from(r"d:\Dev\My Project"),
            )
        } else {
            (
                "file://localhost/tmp/My%20Project",
                PathBuf::from("/tmp/My Project"),
            )
        };
        assert_eq!(native_cwd(uri, cfg!(windows)), Some(expected.clone()));
        let encoded = uri.strip_prefix("file://localhost").unwrap();
        assert_eq!(native_cwd(encoded, cfg!(windows)), Some(expected));
        assert_eq!(native_cwd("", cfg!(windows)), None);
        assert_eq!(native_cwd("relative", cfg!(windows)), None);
        assert!(
            record("devin", &serde_json::json!({"sessionId": "no-cwd"}), true)
                .unwrap()
                .cwd
                .is_none()
        );
    }
}

fn bad_content(message: &str) -> AgentError {
    AgentError::new(ErrorCode::BadRequest, message)
}

pub fn prompt(content: &[ContentBlock], cwd: &Path, image: bool) -> Result<Vec<Value>> {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => Ok(json!({ "type": "text", "text": text })),
            ContentBlock::Image {
                mime_type,
                data,
                uri,
            } => {
                if !image {
                    return Err(bad_content("ACP agent does not accept images"));
                }
                let data = data
                    .as_deref()
                    .or_else(|| {
                        uri.as_deref()?
                            .strip_prefix("data:")?
                            .split_once(";base64,")
                            .filter(|(mime, _)| *mime == mime_type)
                            .map(|(_, data)| data)
                    })
                    .ok_or_else(|| bad_content("ACP images require a base64 payload"))?;
                let mut block = json!({ "type": "image", "mimeType": mime_type, "data": data });
                if let Some(uri) = uri {
                    block["uri"] = json!(uri);
                }
                Ok(block)
            }
            ContentBlock::File { path, name } => {
                let file = Path::new(path);
                let file = if file.is_absolute() {
                    file.to_path_buf()
                } else {
                    cwd.join(file)
                };
                let name = name
                    .as_deref()
                    .or_else(|| file.file_name().and_then(|name| name.to_str()))
                    .unwrap_or(path);
                Ok(json!({ "type": "resource_link", "uri": file_uri(&file), "name": name }))
            }
        })
        .collect()
}

fn file_uri(file: &Path) -> String {
    let path = file.to_string_lossy();
    #[cfg(windows)]
    let path = {
        let path = path.replace('\\', "/");
        if let Some(unc) = path.strip_prefix("//?/UNC/") {
            format!("//{unc}")
        } else {
            path.strip_prefix("//?/").unwrap_or(&path).to_owned()
        }
    };
    let mut uri = String::from("file://");
    #[cfg(windows)]
    let path = if let Some(unc) = path.strip_prefix("//") {
        unc
    } else {
        uri.push('/');
        &path
    };
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            write!(uri, "%{byte:02X}").unwrap();
        }
    }
    uri
}

pub fn content(value: &Value) -> Option<ContentBlock> {
    match value["type"].as_str()? {
        "text" => Some(ContentBlock::Text {
            text: string(value, "text")?,
        }),
        "image" => Some(ContentBlock::Image {
            mime_type: string(value, "mimeType")?,
            data: string(value, "data"),
            uri: string(value, "uri"),
        }),
        "resource_link" | "resource" => {
            let resource = value.get("resource").unwrap_or(value);
            Some(ContentBlock::File {
                path: file_path(&string(resource, "uri")?),
                name: string(value, "name"),
            })
        }
        _ => None,
    }
}

fn file_path(uri: &str) -> String {
    let Some(path) = uri.strip_prefix("file://") else {
        return uri.to_owned();
    };
    let mut decoded = Vec::new();
    let mut offset = 0;
    let bytes = path.as_bytes();
    while offset < bytes.len() {
        if bytes[offset] == b'%' && offset + 2 < bytes.len() {
            let hex = |byte| char::from(byte).to_digit(16);
            if let (Some(high), Some(low)) = (hex(bytes[offset + 1]), hex(bytes[offset + 2])) {
                decoded.push((high * 16 + low) as u8);
                offset += 3;
                continue;
            }
        }
        decoded.push(bytes[offset]);
        offset += 1;
    }
    let path = String::from_utf8_lossy(&decoded);
    if !path.starts_with('/') {
        return format!("//{path}");
    }
    #[cfg(windows)]
    if path.as_bytes().get(2) == Some(&b':') {
        return path[1..].to_owned();
    }
    path.into_owned()
}

#[derive(Default)]
pub struct Config {
    modern: Vec<ConfigOption>,
    legacy: BTreeMap<String, ConfigOption>,
    boolean: Vec<String>,
}

impl Config {
    pub fn update(&mut self, value: &Value) -> bool {
        let mut changed = false;
        if let Some(options) = value["configOptions"].as_array() {
            self.modern = options.iter().filter_map(config_option).collect();
            self.boolean = options
                .iter()
                .filter(|option| option["currentValue"].is_boolean())
                .filter_map(|option| string(option, "id"))
                .collect();
            changed = true;
        }
        for (id, category, state, current_key, choices_key, value_key) in [
            (
                "mode",
                ConfigCategory::Mode,
                "modes",
                "currentModeId",
                "availableModes",
                "id",
            ),
            (
                "model",
                ConfigCategory::Model,
                "models",
                "currentModelId",
                "availableModels",
                "modelId",
            ),
        ] {
            if let Some(current_value) = string(&value[state], current_key) {
                self.legacy.insert(
                    id.to_owned(),
                    ConfigOption {
                        id: id.to_owned(),
                        name: if id == "mode" { "Mode" } else { "Model" }.into(),
                        description: None,
                        category,
                        current_value,
                        choices: array(&value[state][choices_key])
                            .filter_map(|choice| config_choice(choice, value_key, None))
                            .collect(),
                    },
                );
                changed = true;
            }
        }
        changed
    }

    pub fn options(&self) -> Vec<ConfigOption> {
        self.modern
            .iter()
            .chain(self.legacy.values().filter(|legacy| {
                !self
                    .modern
                    .iter()
                    .any(|option| option.category == legacy.category)
            }))
            .cloned()
            .collect()
    }

    pub fn request(&self, id: &str, value: &str) -> Result<(&'static str, Value)> {
        if self.modern.iter().any(|option| option.id == id) {
            let value = if self.boolean.iter().any(|option| option == id) {
                json!(value
                    .parse::<bool>()
                    .map_err(|_| bad_content("Invalid ACP boolean value"))?)
            } else {
                json!(value)
            };
            return Ok((
                "session/set_config_option",
                json!({ "configId": id, "value": value }),
            ));
        }
        match self.legacy.get(id).map(|option| option.category) {
            Some(ConfigCategory::Mode) => Ok(("session/set_mode", json!({ "modeId": value }))),
            Some(ConfigCategory::Model) => Ok(("session/set_model", json!({ "modelId": value }))),
            _ => Err(bad_content("Unknown ACP session option")),
        }
    }

    pub fn set_current(&mut self, id: &str, value: &str) -> bool {
        let option = self
            .modern
            .iter_mut()
            .find(|option| option.id == id)
            .or_else(|| self.legacy.get_mut(id));
        if let Some(option) = option {
            option.current_value = value.to_owned();
            true
        } else {
            false
        }
    }

    pub fn set_mode(&mut self, value: &str) -> bool {
        let id = self
            .modern
            .iter()
            .find(|option| option.category == ConfigCategory::Mode)
            .map_or_else(|| "mode".into(), |option| option.id.clone());
        self.set_current(&id, value)
    }
}

fn config_choice(value: &Value, key: &str, group: Option<&str>) -> Option<ConfigChoice> {
    let id = string(value, key).or_else(|| string(value, "id"))?;
    Some(ConfigChoice {
        name: string(value, "name").unwrap_or_else(|| id.clone()),
        value: id,
        description: string(value, "description"),
        group: group.map(str::to_owned),
    })
}

fn config_option(value: &Value) -> Option<ConfigOption> {
    let id = string(value, "id")?;
    let category = match value["category"].as_str().unwrap_or(&id) {
        "mode" => ConfigCategory::Mode,
        "model" => ConfigCategory::Model,
        "thought_level" => ConfigCategory::ThoughtLevel,
        _ => ConfigCategory::Other,
    };
    let current = &value["currentValue"];
    let (current_value, choices) = if let Some(current) = current.as_bool() {
        let choices = [false, true]
            .into_iter()
            .map(|choice| ConfigChoice {
                value: choice.to_string(),
                name: choice.to_string(),
                description: None,
                group: None,
            })
            .collect();
        (current.to_string(), choices)
    } else {
        let choices = array(&value["options"])
            .flat_map(|choice| {
                if let Some(options) = choice["options"].as_array() {
                    options
                        .iter()
                        .filter_map(|option| {
                            config_choice(option, "value", choice["name"].as_str())
                        })
                        .collect()
                } else {
                    config_choice(choice, "value", None)
                        .into_iter()
                        .collect::<Vec<_>>()
                }
            })
            .collect();
        (current.as_str()?.to_owned(), choices)
    };
    Some(ConfigOption {
        name: string(value, "name").unwrap_or_else(|| id.clone()),
        description: string(value, "description"),
        id,
        category,
        current_value,
        choices,
    })
}

pub fn command(value: &Value) -> Option<SlashCommand> {
    Some(SlashCommand {
        name: string(value, "name")?,
        description: string(value, "description"),
        input_hint: string(&value["input"], "hint"),
    })
}

pub fn patch_tool(tool: &mut ToolCall, value: &Value) {
    if let Some(kind) = value["kind"].as_str() {
        tool.kind = match kind {
            "read" => ToolKind::Read,
            "edit" => ToolKind::Edit,
            "delete" => ToolKind::Delete,
            "move" => ToolKind::Move,
            "search" => ToolKind::Search,
            "execute" => ToolKind::Execute,
            "think" => ToolKind::Think,
            "fetch" => ToolKind::Fetch,
            "switch_mode" | "other" => ToolKind::Other,
            "agent" => ToolKind::Agent,
            "mcp" => ToolKind::Mcp,
            _ => ToolKind::Other,
        };
    }
    if let Some(title) = string(value, "title") {
        tool.title = title;
    }
    if let Some(name) = string(value, "name") {
        tool.name = Some(name);
    }
    if let Some(status) = value["status"].as_str() {
        tool.status = match status {
            "in_progress" => ToolStatus::InProgress,
            "completed" => ToolStatus::Completed,
            "failed" | "cancelled" => ToolStatus::Failed,
            "pending" => ToolStatus::Pending,
            _ => tool.status,
        };
    }
    if let Some(input) = value.get("rawInput").filter(|value| !value.is_null()) {
        tool.input = Some(input.clone());
    }
    if let Some(output) = value.get("rawOutput").filter(|value| !value.is_null()) {
        tool.output = Some(output.clone());
    }
    if let Some(content) = value["content"].as_array() {
        tool.content = content.iter().filter_map(tool_content).collect();
    }
    if let Some(locations) = value["locations"].as_array() {
        tool.locations = locations
            .iter()
            .filter_map(|location| {
                Some(ToolLocation {
                    path: string(location, "path")?,
                    line: location["line"]
                        .as_u64()
                        .and_then(|line| u32::try_from(line).ok()),
                })
            })
            .collect();
    }
}

fn tool_content(value: &Value) -> Option<ToolContent> {
    match value["type"].as_str()? {
        "diff" => Some(ToolContent::Diff {
            path: string(value, "path")?,
            old_text: string(value, "oldText"),
            new_text: string(value, "newText"),
            unified: None,
        }),
        "content" => match content(&value["content"])? {
            ContentBlock::Text { text } => Some(ToolContent::Text { text }),
            ContentBlock::Image {
                mime_type,
                data: Some(data),
                ..
            } => Some(ToolContent::Image { mime_type, data }),
            _ => None,
        },
        _ => None,
    }
}
