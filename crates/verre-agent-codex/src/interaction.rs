use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use verre_agent_api::{jsonrpc::Id, ItemDraft, Result};
use verre_proto::{
    InteractionRequest, ItemKind, PermissionOption, PermissionOptionKind, Question, QuestionChoice,
    RequestBody, RequestResolution,
};

use crate::{
    bad_request,
    mapping::{array, string},
};

#[derive(Clone)]
pub(crate) struct Pending {
    pub id: Id,
    pub item: ItemDraft,
    response: Response,
    pub responding: bool,
}

#[derive(Clone)]
enum Response {
    Select {
        choices: BTreeMap<String, Value>,
        cancel: Value,
    },
    Questions(Vec<Question>),
    Form(Value),
}

pub(crate) fn key(id: &Id) -> String {
    match id {
        Id::Number(number) => format!("n:{number}"),
        Id::String(value) => format!("s:{value}"),
        Id::Null => "null".into(),
    }
}

impl Pending {
    pub(crate) fn new(id: Id, method: &str, params: &Value) -> Option<Self> {
        let mut detail = string(params, "reason");
        let (title, body, response) = match method {
            "item/commandExecution/requestApproval" => {
                if let Some(command) = params["command"].as_str() {
                    let command = crate::mapping::command_text(command);
                    detail = Some(match detail {
                        Some(reason) => format!("{reason}\n\n{command}"),
                        None => command,
                    });
                }
                let (body, response) = approvals(
                    params["availableDecisions"]
                        .as_array()
                        .cloned()
                        .unwrap_or_else(|| {
                            vec![json!("accept"), json!("decline"), json!("cancel")]
                        }),
                );
                ("Approve command execution", body, response)
            }
            "item/fileChange/requestApproval" => {
                let mut decisions = vec![json!("accept"), json!("decline"), json!("cancel")];
                if params["grantRoot"].is_string() {
                    decisions.insert(1, json!("acceptForSession"));
                }
                let (body, response) = approvals(decisions);
                ("Approve file changes", body, response)
            }
            "item/permissions/requestApproval" => {
                let permissions = params["permissions"].as_object()?;
                let permissions = Value::Object(
                    permissions
                        .iter()
                        .filter(|(id, value)| {
                            matches!(id.as_str(), "fileSystem" | "network") && !value.is_null()
                        })
                        .map(|(id, value)| (id.clone(), value.clone()))
                        .collect(),
                );
                detail = Some(match detail {
                    Some(reason) => format!("{reason}\n\n{permissions}"),
                    None => permissions.to_string(),
                });
                let deny = json!({"permissions": {}, "scope": "turn"});
                let (body, response) = selections(
                    [
                        (
                            "turn",
                            "Allow for this turn",
                            PermissionOptionKind::AllowOnce,
                            json!({"permissions": permissions, "scope": "turn"}),
                        ),
                        (
                            "session",
                            "Allow for this session",
                            PermissionOptionKind::AllowAlways,
                            json!({"permissions": permissions, "scope": "session"}),
                        ),
                        (
                            "decline",
                            "Decline",
                            PermissionOptionKind::RejectOnce,
                            deny.clone(),
                        ),
                    ],
                    deny,
                );
                ("Approve additional permissions", body, response)
            }
            "item/tool/requestUserInput" => {
                let questions: Vec<_> = array(&params["questions"])
                    .iter()
                    .map(|raw| {
                        if raw["isSecret"] == true {
                            return None;
                        }
                        Some(Question {
                            id: string(raw, "id")?,
                            header: string(raw, "header"),
                            text: string(raw, "question")?,
                            choices: array(&raw["options"])
                                .iter()
                                .filter_map(|option| {
                                    Some(QuestionChoice {
                                        label: string(option, "label")?,
                                        description: string(option, "description"),
                                    })
                                })
                                .collect(),
                            multi_select: false,
                            allow_free_text: raw["isOther"] == true
                                || array(&raw["options"]).is_empty(),
                        })
                    })
                    .collect::<Option<_>>()?;
                (
                    "Codex has a question",
                    RequestBody::Questions {
                        questions: questions.clone(),
                    },
                    Response::Questions(questions),
                )
            }
            "mcpServer/elicitation/request" => {
                detail = string(params, "message");
                let (body, response) = match params["mode"].as_str()? {
                    "url" => {
                        let url = params["url"].as_str()?;
                        detail = Some(format!(
                            "{}\n\n{url}",
                            detail.as_deref().unwrap_or_default()
                        ));
                        selections(
                            [
                                (
                                    "accept",
                                    "Accept",
                                    PermissionOptionKind::AllowOnce,
                                    elicitation("accept", Value::Null),
                                ),
                                (
                                    "decline",
                                    "Decline",
                                    PermissionOptionKind::RejectOnce,
                                    elicitation("decline", Value::Null),
                                ),
                                (
                                    "cancel",
                                    "Cancel",
                                    PermissionOptionKind::RejectOnce,
                                    elicitation("cancel", Value::Null),
                                ),
                            ],
                            elicitation("cancel", Value::Null),
                        )
                    }
                    "form" | "openai/form" | "openaiForm" => {
                        let schema = &params["requestedSchema"];
                        let questions = form_questions(schema)?;
                        (
                            RequestBody::Questions { questions },
                            Response::Form(schema.clone()),
                        )
                    }
                    _ => return None,
                };
                ("MCP server request", body, response)
            }
            _ => return None,
        };
        let request_id = key(&id);
        let turn_id = string(params, "turnId");
        Some(Self {
            id,
            item: ItemDraft {
                id: match &turn_id {
                    Some(turn_id) => format!("{turn_id}:request:{request_id}"),
                    None => format!("request:{request_id}"),
                },
                ts: params["startedAtMs"].as_i64(),
                turn_id,
                kind: ItemKind::Request(InteractionRequest {
                    request_id,
                    call_id: string(params, "itemId"),
                    title: title.into(),
                    detail,
                    body,
                    resolution: None,
                }),
            },
            response,
            responding: false,
        })
    }

    pub(crate) fn payload(&self, resolution: &RequestResolution) -> Result<Value> {
        match (&self.response, resolution) {
            (Response::Select { choices, .. }, RequestResolution::Selected { option_id }) => {
                choices
                    .get(option_id)
                    .cloned()
                    .ok_or_else(|| bad_request("Unknown Codex request option"))
            }
            (Response::Select { cancel, .. }, RequestResolution::Cancelled) => Ok(cancel.clone()),
            (Response::Questions(questions), RequestResolution::Answered { answers }) => {
                let mut result = Map::new();
                for (id, answers) in answers {
                    let question = questions
                        .iter()
                        .find(|question| question.id == *id)
                        .ok_or_else(|| bad_request("Unknown Codex question id"))?;
                    if answers.len() > 1
                        || (!question.allow_free_text
                            && answers.iter().any(|answer| {
                                !question
                                    .choices
                                    .iter()
                                    .any(|choice| &choice.label == answer)
                            }))
                    {
                        return Err(bad_request(
                            "Answer does not match the Codex question choices",
                        ));
                    }
                    result.insert(id.clone(), json!({"answers": answers}));
                }
                Ok(json!({"answers": result}))
            }
            (Response::Questions(_), RequestResolution::Cancelled) => Ok(json!({"answers": {}})),
            (Response::Form(schema), RequestResolution::Answered { answers }) => {
                Ok(elicitation("accept", form_answers(schema, answers)?))
            }
            (Response::Form(_), RequestResolution::Cancelled) => {
                Ok(elicitation("cancel", Value::Null))
            }
            (Response::Form(_), RequestResolution::Selected { option_id })
                if option_id == "decline" =>
            {
                Ok(elicitation("decline", Value::Null))
            }
            _ => Err(bad_request("Resolution does not match the Codex request")),
        }
    }

    pub(crate) fn resolved(mut self, resolution: RequestResolution) -> ItemDraft {
        if let ItemKind::Request(request) = &mut self.item.kind {
            request.resolution = Some(resolution);
        }
        self.item
    }
}

fn approvals(decisions: Vec<Value>) -> (RequestBody, Response) {
    let options = decisions.into_iter().filter_map(|decision| {
        let (label, kind) = match decision.as_str() {
            Some("accept") => ("Allow once", PermissionOptionKind::AllowOnce),
            Some("acceptForSession") => {
                ("Allow for this session", PermissionOptionKind::AllowAlways)
            }
            Some("decline") => ("Decline", PermissionOptionKind::RejectOnce),
            Some("cancel") => ("Cancel", PermissionOptionKind::RejectOnce),
            _ if decision["acceptWithExecpolicyAmendment"].is_object() => {
                ("Allow similar commands", PermissionOptionKind::AllowAlways)
            }
            _ if decision["applyNetworkPolicyAmendment"].is_object() => {
                let amendment =
                    &decision["applyNetworkPolicyAmendment"]["network_policy_amendment"];
                if amendment["action"] == "deny" {
                    ("Always deny this host", PermissionOptionKind::RejectAlways)
                } else {
                    ("Always allow this host", PermissionOptionKind::AllowAlways)
                }
            }
            _ => return None,
        };
        let id = decision
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| decision.to_string());
        Some((id, label, kind, json!({"decision": decision})))
    });
    selections(options, json!({"decision": "cancel"}))
}

fn selections(
    choices: impl IntoIterator<Item = (impl Into<String>, &'static str, PermissionOptionKind, Value)>,
    cancel: Value,
) -> (RequestBody, Response) {
    let mut options = Vec::new();
    let mut values = BTreeMap::new();
    for (id, label, kind, value) in choices {
        let id = id.into();
        options.push(PermissionOption {
            id: id.clone(),
            label: label.into(),
            kind,
        });
        values.insert(id, value);
    }
    (
        RequestBody::Permission { options },
        Response::Select {
            choices: values,
            cancel,
        },
    )
}

fn elicitation(action: &str, content: Value) -> Value {
    json!({"action": action, "content": content, "_meta": null})
}

fn form_questions(schema: &Value) -> Option<Vec<Question>> {
    schema["properties"]
        .as_object()?
        .iter()
        .map(|(id, field)| {
            if field["format"] == "password" || field["writeOnly"] == true {
                return None;
            }
            let choices = form_choices(field);
            Some(Question {
                id: id.clone(),
                header: string(field, "title"),
                text: string(field, "description")
                    .or_else(|| string(field, "title"))
                    .unwrap_or_else(|| id.clone()),
                multi_select: field["type"] == "array",
                allow_free_text: choices.is_empty(),
                choices: choices.into_iter().map(|(choice, _)| choice).collect(),
            })
        })
        .collect()
}

fn form_choices(field: &Value) -> Vec<(QuestionChoice, Value)> {
    if field["type"] == "boolean" {
        return [true, false]
            .into_iter()
            .map(|value| {
                (
                    QuestionChoice {
                        label: value.to_string(),
                        description: None,
                    },
                    json!(value),
                )
            })
            .collect();
    }
    let field = if field["type"] == "array" {
        &field["items"]
    } else {
        field
    };
    if let Some(options) = field["oneOf"]
        .as_array()
        .or_else(|| field["anyOf"].as_array())
    {
        return options
            .iter()
            .filter_map(|entry| {
                let value = entry["const"].as_str()?;
                let label = entry["title"].as_str().unwrap_or(value).into();
                Some((
                    QuestionChoice {
                        label,
                        description: string(entry, "description"),
                    },
                    json!(value),
                ))
            })
            .collect();
    }
    array(&field["enum"])
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let value = entry.as_str()?;
            let label = array(&field["enumNames"])
                .get(index)
                .and_then(Value::as_str)
                .unwrap_or(value);
            Some((
                QuestionChoice {
                    label: label.into(),
                    description: None,
                },
                entry.clone(),
            ))
        })
        .collect()
}

fn form_answers(schema: &Value, answers: &BTreeMap<String, Vec<String>>) -> Result<Value> {
    let mut result = Map::new();
    for (id, answers) in answers {
        let field = schema["properties"]
            .get(id)
            .ok_or_else(|| bad_request("Unknown MCP form field"))?;
        let choices = form_choices(field);
        let parse = |answer: &String| -> Result<Value> {
            if !choices.is_empty() {
                return choices
                    .iter()
                    .find(|(choice, _)| choice.label == *answer)
                    .map(|(_, value)| value.clone())
                    .ok_or_else(|| bad_request("Unknown MCP form choice"));
            }
            let value = match field["type"].as_str() {
                Some("number") => answer
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number),
                Some("integer") => answer.parse::<i64>().ok().map(|value| json!(value)),
                _ => Some(json!(answer)),
            };
            value.ok_or_else(|| bad_request("Invalid number in MCP form"))
        };
        let value = if field["type"] == "array" {
            Value::Array(answers.iter().map(parse).collect::<Result<_>>()?)
        } else {
            match answers.as_slice() {
                [] => continue,
                [answer] => parse(answer)?,
                _ => return Err(bad_request("MCP form field requires a single answer")),
            }
        };
        result.insert(id.clone(), value);
    }
    if array(&schema["required"])
        .iter()
        .filter_map(Value::as_str)
        .any(|id| !result.contains_key(id))
    {
        return Err(bad_request("An MCP form field is required"));
    }
    Ok(Value::Object(result))
}
