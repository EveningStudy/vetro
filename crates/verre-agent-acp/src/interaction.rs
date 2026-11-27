use std::collections::BTreeMap;

use serde_json::{json, Map, Number, Value};
use verre_agent_api::{jsonrpc::RemoteError, AgentError, Result};
use verre_proto::{
    rpc::ErrorCode, InteractionRequest, PermissionOption, PermissionOptionKind, Question,
    QuestionChoice, RequestBody, RequestResolution,
};

use crate::mapping::{array, string};

const PERMISSION: &str = "session/request_permission";

pub fn cancelled(method: &str) -> Value {
    if method == PERMISSION {
        json!({ "outcome": { "outcome": "cancelled" } })
    } else {
        json!({ "action": "cancel" })
    }
}

pub enum Interaction {
    Permission(Option<Value>),
    Form(BTreeMap<String, Field>),
}

pub struct Field {
    choices: BTreeMap<String, String>,
    kind: &'static str,
    multiple: bool,
    required: bool,
}

impl Interaction {
    pub fn parse(
        method: &str,
        params: &Value,
        request_id: String,
        kiro: bool,
    ) -> std::result::Result<(Self, InteractionRequest), RemoteError> {
        let (kind, call_id, title, detail, body) = if method == PERMISSION {
            let tool = &params["toolCall"];
            let context = &params["_meta"]["kiro"]["consent"];
            let consent = if kiro && context.is_object() {
                workspace_consent(context)
            } else {
                None
            };
            let options = array(&params["options"])
                .filter_map(permission)
                .filter_map(|mut option| {
                    if kiro && context.is_object() && persistent(option.kind) {
                        consent.as_ref()?;
                        option.label = format!("{} in this workspace", option.label);
                    }
                    Some(option)
                })
                .collect();
            let detail = (kiro && context.is_object())
                .then(|| consent_resource(context))
                .flatten()
                .map(str::to_owned);
            (
                Self::Permission(consent),
                string(tool, "toolCallId"),
                string(tool, "title").unwrap_or_else(|| "Permission request".into()),
                detail,
                RequestBody::Permission { options },
            )
        } else {
            let schema = &params["requestedSchema"];
            if !matches!(params["mode"].as_str(), Some("form") | None)
                || !schema["properties"].is_object()
            {
                return Err(invalid("Only session-scoped form elicitation is supported"));
            }
            let mut fields = BTreeMap::new();
            let mut questions = Vec::new();
            for (id, property) in schema["properties"].as_object().into_iter().flatten() {
                let required = array(&schema["required"]).any(|field| field.as_str() == Some(id));
                let (field, question) = parse_field(id, property, required)?;
                fields.insert(id.clone(), field);
                questions.push(question);
            }
            (
                Self::Form(fields),
                string(params, "toolCallId"),
                string(params, "message").unwrap_or_else(|| "Agent question".into()),
                None,
                RequestBody::Questions { questions },
            )
        };
        Ok((
            kind,
            InteractionRequest {
                request_id,
                call_id,
                title,
                detail,
                body,
                resolution: None,
            },
        ))
    }

    pub fn cancel(&self) -> Value {
        cancelled(if matches!(self, Self::Permission(_)) {
            PERMISSION
        } else {
            "elicitation/create"
        })
    }

    pub fn reply(&self, body: &RequestBody, resolution: &RequestResolution) -> Result<Value> {
        match (self, body, resolution) {
            (_, _, RequestResolution::Cancelled) => Ok(self.cancel()),
            (
                Self::Permission(consent),
                RequestBody::Permission { options },
                RequestResolution::Selected { option_id },
            ) => {
                let option = options
                    .iter()
                    .find(|option| option.id == *option_id)
                    .ok_or_else(bad_answer)?;
                let mut result =
                    json!({ "outcome": { "outcome": "selected", "optionId": option_id } });
                if let Some(consent) = consent.as_ref().filter(|_| persistent(option.kind)) {
                    result["_meta"] = json!({ "kiro": { "consent": consent } });
                }
                Ok(result)
            }
            (Self::Form(fields), _, RequestResolution::Answered { answers }) => {
                let mut content = Map::new();
                for id in answers.keys() {
                    if !fields.contains_key(id) {
                        return Err(bad_answer());
                    }
                }
                for (id, field) in fields {
                    if let Some(values) = answers.get(id).filter(|values| !values.is_empty()) {
                        content.insert(id.clone(), field.answer(values)?);
                    } else if field.required {
                        return Err(bad_answer());
                    }
                }
                Ok(json!({ "action": "accept", "content": content }))
            }
            _ => Err(bad_answer()),
        }
    }
}

fn persistent(kind: PermissionOptionKind) -> bool {
    matches!(
        kind,
        PermissionOptionKind::AllowAlways | PermissionOptionKind::RejectAlways
    )
}

fn consent_resource(context: &Value) -> Option<&str> {
    context["triggeringResource"]
        .as_str()
        .or_else(|| context["resource"].as_str())
        .filter(|resource| !resource.is_empty())
}

fn workspace_consent(context: &Value) -> Option<Value> {
    let resource = consent_resource(context)?;
    let root = context["workspaceRoot"].as_str()?;
    if context["persistableConsent"].as_bool() == Some(false)
        || !std::path::Path::new(root).is_absolute()
        || resource.contains(['*', '?', '[', ']', '{', '}', '\0', '\r', '\n'])
    {
        return None;
    }
    Some(json!({ "scope": "workspace", "resource": resource, "workspaceRoot": root }))
}

fn invalid(message: &str) -> RemoteError {
    RemoteError {
        code: -32602,
        message: message.into(),
        data: None,
    }
}

fn bad_answer() -> AgentError {
    AgentError::new(ErrorCode::BadRequest, "Invalid ACP interaction response")
}

fn permission(value: &Value) -> Option<PermissionOption> {
    let id = string(value, "optionId")?;
    let kind = match value["kind"].as_str()? {
        "allow_once" => PermissionOptionKind::AllowOnce,
        "allow_always" => PermissionOptionKind::AllowAlways,
        "reject_once" => PermissionOptionKind::RejectOnce,
        "reject_always" => PermissionOptionKind::RejectAlways,
        _ => return None,
    };
    Some(PermissionOption {
        label: string(value, "name").unwrap_or_else(|| id.clone()),
        id,
        kind,
    })
}

fn parse_field(
    id: &str,
    property: &Value,
    required: bool,
) -> std::result::Result<(Field, Question), RemoteError> {
    let (kind, multiple) = match property["type"].as_str() {
        Some("string") => ("string", false),
        Some("number") => ("number", false),
        Some("integer") => ("integer", false),
        Some("boolean") => ("boolean", false),
        Some("array") => ("string", true),
        _ => return Err(invalid("Unsupported ACP form field")),
    };
    let variants = if multiple {
        &property["items"]
    } else {
        property
    };
    let mut choices = Vec::new();
    let mut values = BTreeMap::new();
    let mut add = |label: &str, value: &str| {
        let mut display = label.to_owned();
        if values.contains_key(&display) {
            display = format!("{label} ({value})");
        }
        values.insert(display.clone(), value.to_owned());
        choices.push(QuestionChoice {
            label: display,
            description: None,
        });
    };
    for choice in array(&variants["oneOf"]).chain(array(&variants["anyOf"])) {
        if let Some(value) = choice["const"].as_str() {
            add(choice["title"].as_str().unwrap_or(value), value);
        }
    }
    for (index, value) in array(&variants["enum"])
        .filter_map(Value::as_str)
        .enumerate()
    {
        add(
            variants["enumNames"][index].as_str().unwrap_or(value),
            value,
        );
    }
    if kind == "boolean" {
        add("true", "true");
        add("false", "false");
    }
    let header = string(property, "title");
    let text = string(property, "description")
        .or_else(|| header.clone())
        .unwrap_or_else(|| id.to_owned());
    Ok((
        Field {
            choices: values,
            kind,
            multiple,
            required,
        },
        Question {
            id: id.to_owned(),
            header,
            text,
            allow_free_text: choices.is_empty(),
            choices,
            multi_select: multiple,
        },
    ))
}

impl Field {
    fn answer(&self, values: &[String]) -> Result<Value> {
        let parse = |value: &str| {
            let value = if self.choices.is_empty() {
                value
            } else {
                self.choices
                    .get(value)
                    .map(String::as_str)
                    .ok_or_else(bad_answer)?
            };
            match self.kind {
                "boolean" => value
                    .parse::<bool>()
                    .map(Value::Bool)
                    .map_err(|_| bad_answer()),
                "integer" => value
                    .parse::<i64>()
                    .map(|number| json!(number))
                    .map_err(|_| bad_answer()),
                "number" => value
                    .parse::<f64>()
                    .ok()
                    .and_then(Number::from_f64)
                    .map(Value::Number)
                    .ok_or_else(bad_answer),
                _ => Ok(json!(value)),
            }
        };
        if self.multiple {
            values
                .iter()
                .map(|value| parse(value))
                .collect::<Result<Vec<_>>>()
                .map(Value::Array)
        } else if let [value] = values {
            parse(value)
        } else {
            Err(bad_answer())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kiro_persistent_consent_uses_only_the_offered_workspace_and_resource() {
        let cwd = std::env::current_dir().unwrap();
        let mut params = json!({
            "toolCall": {"toolCallId": "call", "title": "Review command"},
            "options": [
                {"optionId": "accept", "name": "Allow", "kind": "allow_once"},
                {"optionId": "always-accept", "name": "Always allow", "kind": "allow_always"}
            ],
            "_meta": {"kiro": {"consent": {
                "capability": "shell", "resource": "npm run *", "triggeringResource": "npm run test",
                "workspaceRoot": cwd, "persistableConsent": true
            }}}
        });
        let selection = |option_id: &str| RequestResolution::Selected {
            option_id: option_id.to_owned(),
        };
        let (protocol, request) =
            Interaction::parse(PERMISSION, &params, "p".into(), true).unwrap();
        assert_eq!(request.detail.as_deref(), Some("npm run test"));
        assert_eq!(
            protocol
                .reply(&request.body, &selection("always-accept"))
                .unwrap(),
            json!({
                "outcome": {"outcome": "selected", "optionId": "always-accept"},
                "_meta": {"kiro": {"consent": {
                    "scope": "workspace", "resource": "npm run test", "workspaceRoot": cwd
                }}}
            })
        );
        assert_eq!(
            protocol.reply(&request.body, &selection("accept")).unwrap(),
            json!({"outcome": {"outcome": "selected", "optionId": "accept"}})
        );
        params["_meta"]["kiro"]["consent"]["triggeringResource"] = Value::Null;
        let (protocol, request) =
            Interaction::parse(PERMISSION, &params, "p".into(), true).unwrap();
        assert!(protocol
            .reply(&request.body, &selection("always-accept"))
            .is_err());
    }
}
