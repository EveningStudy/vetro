use std::{collections::BTreeMap, path::Path};

use serde_json::{json, Value};
use vetro_agent_api::Result;
use vetro_proto::{ConfigCategory, ConfigChoice, ConfigOption};

use crate::{
    bad_request,
    mapping::{array, string},
    unavailable, App,
};

#[derive(Default)]
pub(crate) struct Catalog {
    models: Vec<Value>,
    modes: Vec<Value>,
    permissions: Vec<Value>,
    approvals: Vec<Value>,
    sandboxes: Vec<Value>,
    default_permissions: Option<String>,
}

impl Catalog {
    pub(crate) async fn load(app: &App, cwd: Option<&Path>) -> Result<Self> {
        let mut catalog = Self {
            models: pages(app, "model/list", json!({})).await?,
            ..Self::default()
        };
        if let Ok(response) = app.call("collaborationMode/list", json!({})).await {
            catalog.modes = array(&response["data"]).to_vec();
        }
        let params = cwd.map_or_else(|| json!({}), |cwd| json!({"cwd": cwd}));
        catalog.permissions = pages(app, "permissionProfile/list", params)
            .await
            .unwrap_or_default();
        let params = cwd.map_or_else(|| json!({}), |cwd| json!({"cwd": cwd}));
        if let Ok(response) = app.call("config/read", params).await {
            let config = &response["config"];
            catalog.default_permissions = string(config, "default_permissions").or_else(|| {
                match config["sandbox_mode"].as_str()? {
                    "read-only" => Some(":read-only".into()),
                    "workspace-write" => Some(":workspace".into()),
                    "danger-full-access" => Some(":danger-full-access".into()),
                    _ => None,
                }
            });
        }
        let requirements = app.requirements().await;
        catalog.approvals = array(&requirements["allowedApprovalPolicies"]).to_vec();
        catalog.sandboxes = array(&requirements["allowedSandboxModes"]).to_vec();
        Ok(catalog)
    }

    fn model(&self, selected: &BTreeMap<String, String>) -> Option<&Value> {
        match selected.get("model") {
            Some(model) => self
                .models
                .iter()
                .find(|entry| model_id(entry) == Some(model)),
            None => self.models.iter().find(|entry| entry["isDefault"] == true),
        }
    }

    pub(crate) fn options(&self, selected: &BTreeMap<String, String>) -> Vec<ConfigOption> {
        let model = self.model(selected);
        let models = self.models.iter().filter_map(|model| {
            let id = model_id(model)?;
            Some(choice(
                id,
                model["displayName"].as_str().unwrap_or(id),
                string(model, "description"),
            ))
        });
        let efforts = model
            .into_iter()
            .flat_map(|entry| array(&entry["supportedReasoningEfforts"]))
            .filter_map(|entry| {
                let id = entry["reasoningEffort"].as_str()?;
                Some(choice(id, id, string(entry, "description")))
            });
        let modes = self.modes.iter().filter_map(|entry| {
            let id = entry["mode"].as_str()?;
            Some(choice(id, entry["name"].as_str().unwrap_or(id), None))
        });
        let permissions = self
            .permissions
            .iter()
            .filter(|profile| profile["allowed"] != false)
            .filter_map(|profile| {
                let id = profile["id"].as_str()?;
                let name = match id {
                    ":read-only" => "Read only",
                    ":workspace" => "Workspace",
                    ":danger-full-access" => "Full access",
                    other => profile["name"].as_str().unwrap_or(other),
                };
                Some(choice(id, name, string(profile, "description")))
            });
        let approvals = self.approvals.iter().filter_map(|entry| {
            let id = choice_id(entry)?;
            Some(choice(&id, entry.as_str().unwrap_or("Granular"), None))
        });
        let sandboxes = self
            .sandboxes
            .iter()
            .filter_map(Value::as_str)
            .map(|id| choice(id, id, None));
        [
            option(
                "model",
                "Model",
                ConfigCategory::Model,
                selected,
                model.and_then(model_id),
                models,
            ),
            option(
                "effort",
                "Reasoning effort",
                ConfigCategory::ThoughtLevel,
                selected,
                model.and_then(|model| model["defaultReasoningEffort"].as_str()),
                efforts,
            ),
            option(
                "mode",
                "Collaboration mode",
                ConfigCategory::Mode,
                selected,
                self.modes
                    .iter()
                    .find(|mode| mode["mode"] == "default")
                    .and_then(|mode| mode["mode"].as_str()),
                modes,
            ),
            option(
                "permissions",
                "Permission profile",
                ConfigCategory::Mode,
                selected,
                self.default_permissions.as_deref(),
                permissions,
            ),
            option(
                "approvalPolicy",
                "Approval policy",
                ConfigCategory::Mode,
                selected,
                None,
                approvals,
            ),
            option(
                "sandbox",
                "Sandbox",
                ConfigCategory::Mode,
                selected,
                None,
                sandboxes,
            ),
        ]
        .into_iter()
        .filter(|entry| !entry.choices.is_empty())
        .collect()
    }

    pub(crate) fn validate(
        &self,
        id: &str,
        value: &str,
        selected: &BTreeMap<String, String>,
    ) -> Result<()> {
        let advertised = self
            .options(selected)
            .iter()
            .find(|option| option.id == id)
            .map(|option| option.choices.iter().any(|choice| choice.value == value));
        let permitted = match advertised {
            Some(allowed) => allowed,
            None => match id {
                "approvalPolicy" => {
                    self.approvals.is_empty()
                        && matches!(value, "untrusted" | "on-request" | "never")
                }
                "sandbox" => {
                    self.sandboxes.is_empty()
                        && matches!(
                            value,
                            "read-only" | "workspace-write" | "danger-full-access"
                        )
                }
                _ => false,
            },
        };
        if permitted {
            Ok(())
        } else {
            Err(bad_request(format!(
                "Unsupported Codex option {id}={value}"
            )))
        }
    }

    pub(crate) fn start_params(
        &self,
        cwd: &Path,
        selected: &BTreeMap<String, String>,
    ) -> Result<(Value, Value)> {
        if selected.contains_key("permissions") && selected.contains_key("sandbox") {
            return Err(bad_request(
                "Codex permissions and sandbox cannot be selected together",
            ));
        }
        let mut thread = json!({ "cwd": cwd });
        let mut turn = json!({});
        for (id, value) in selected {
            self.validate(id, value, selected)?;
            match id.as_str() {
                "mode" => {}
                "effort" => turn["effort"] = json!(value),
                "approvalPolicy" => thread[id] = approval_value(value),
                _ => thread[id] = json!(value),
            }
        }
        Ok((thread, turn))
    }

    pub(crate) fn collaboration(
        &self,
        mode: &str,
        selected: &BTreeMap<String, String>,
    ) -> Result<Value> {
        self.validate("mode", mode, selected)?;
        let model = selected
            .get("model")
            .map(String::as_str)
            .or_else(|| self.model(selected).and_then(model_id))
            .ok_or_else(|| bad_request("Codex collaboration mode requires a model"))?;
        let mut settings = json!({"model": model});
        if let Some(effort) = selected.get("effort") {
            settings["reasoning_effort"] = json!(effort);
        }
        Ok(json!({"mode": mode, "settings": settings}))
    }
}

fn model_id(model: &Value) -> Option<&str> {
    model["model"].as_str().or_else(|| model["id"].as_str())
}

fn choice(value: &str, name: &str, description: Option<String>) -> ConfigChoice {
    ConfigChoice {
        value: value.to_owned(),
        name: name.to_owned(),
        description,
        group: None,
    }
}

fn option(
    id: &str,
    name: &str,
    category: ConfigCategory,
    selected: &BTreeMap<String, String>,
    default: Option<&str>,
    choices: impl Iterator<Item = ConfigChoice>,
) -> ConfigOption {
    ConfigOption {
        id: id.into(),
        name: name.into(),
        description: None,
        category,
        current_value: selected
            .get(id)
            .cloned()
            .or_else(|| default.map(str::to_owned))
            .unwrap_or_default(),
        choices: choices.collect(),
    }
}

fn choice_id(value: &Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        Some(value.to_owned())
    } else if value.is_object() {
        Some(value.to_string())
    } else {
        None
    }
}

pub(crate) fn approval_value(value: &str) -> Value {
    if value.starts_with('{') {
        serde_json::from_str(value).unwrap_or_else(|_| json!(value))
    } else {
        json!(value)
    }
}

pub(crate) async fn pages(app: &App, method: &str, mut params: Value) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    loop {
        let mut page = app.call(method, params.clone()).await?;
        if let Some(items) = page["data"].as_array_mut() {
            entries.append(items);
        }
        let Some(cursor) = string(&page, "nextCursor") else {
            return Ok(entries);
        };
        if params["cursor"] == cursor {
            return Err(unavailable(format!("Codex {method} repeated its cursor")));
        }
        params["cursor"] = json!(cursor);
    }
}

pub(crate) fn open_browser(url: &str) -> Result<()> {
    if !url.starts_with("https://")
        && !url.starts_with("http://localhost:")
        && !url.starts_with("http://127.0.0.1:")
    {
        return Err(unavailable("Codex returned an unsupported sign-in URL"));
    }
    if url.contains(['\0', '\r', '\n']) {
        return Err(unavailable("Codex returned an invalid sign-in URL"));
    }
    launch_browser(url)
}

#[cfg(windows)]
fn launch_browser(url: &str) -> Result<()> {
    use std::ptr::null;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let url: Vec<u16> = url.encode_utf16().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            url.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err(unavailable("Could not open the Codex sign-in URL"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_browser(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut opener = std::process::Command::new(opener);
    let status = opener
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !status.is_ok_and(|status| status.success()) {
        return Err(unavailable("Could not open the Codex sign-in URL"));
    }
    Ok(())
}
