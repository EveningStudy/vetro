use std::{collections::HashMap, fs::File, io::Read, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use vetro_agent_api::ItemDraft;
use vetro_proto::{
    ContentBlock, ItemKind, LinkConfidence, ThreadParent, ThreadSource, ThreadSummary, ToolContent,
    ToolKind, ToolStatus,
};

use crate::files;

#[derive(Default)]
pub(crate) struct Projection {
    pub messages: Vec<String>,
    pub prompt: Option<String>,
    pub calls: Vec<Call>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Call {
    pub id: String,
    pub targets: Vec<String>,
    pub ids: Vec<String>,
    command: String,
    cwd: Option<String>,
    start: Option<i64>,
    end: Option<i64>,
    #[serde(default)]
    activity: bool,
}

impl Projection {
    pub fn from_items<'a>(
        summary: &ThreadSummary,
        items: impl DoubleEndedIterator<Item = (&'a ItemKind, Option<i64>, Option<&'a str>)>,
        native_children: Option<&HashMap<String, Vec<String>>>,
    ) -> Self {
        let mut projection = Self::default();
        let mut next = None;
        let mut calls = Vec::new();
        for (kind, timestamp, turn) in items.rev() {
            match kind {
                ItemKind::UserMessage { content } => {
                    let text = content
                        .iter()
                        .filter_map(|part| match part {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.trim().is_empty() {
                        projection.prompt = Some(text.chars().take(16_384).collect());
                        projection.messages.push(text);
                    }
                }
                ItemKind::AgentMessage { text, .. } if !text.trim().is_empty() => {
                    projection.messages.push(text.clone());
                }
                ItemKind::ToolCall(tool)
                    if matches!(tool.kind, ToolKind::Execute | ToolKind::Agent) =>
                {
                    let terminal = tool.content.iter().find_map(|content| match content {
                        ToolContent::Terminal { command, cwd, .. } => {
                            Some((command.as_deref(), cwd.as_deref()))
                        }
                        _ => None,
                    });
                    let input = tool.input.as_ref().unwrap_or(&Value::Null);
                    let command = terminal
                        .and_then(|(command, _)| command)
                        .or_else(|| {
                            string(
                                input,
                                &["command", "cmd", "CommandLine", "commandLine", "prompt"],
                            )
                        })
                        .or_else(|| input.as_str())
                        .unwrap_or(&tool.title);
                    let mut targets = invocations(command);
                    let activity = tool.kind == ToolKind::Agent
                        && summary.agent_id == "codex"
                        && tool.title == "started"
                        && tool
                            .output
                            .as_ref()
                            .is_some_and(|output| output["agentThreadId"].is_string());
                    if tool.kind == ToolKind::Agent
                        && (summary.agent_id != "codex"
                            || tool.name.as_deref() == Some("spawnAgent")
                            || activity)
                    {
                        targets.push(summary.agent_id.clone());
                    }
                    targets.sort_unstable();
                    targets.dedup();
                    if !targets.is_empty() {
                        let cwd = terminal
                            .and_then(|(_, cwd)| cwd)
                            .or_else(|| {
                                string(input, &["cwd", "workdir", "workingDirectory", "Cwd"])
                            })
                            .or(summary.cwd.as_deref())
                            .map(|path| {
                                files::normalize(Path::new(path))
                                    .to_string_lossy()
                                    .into_owned()
                            });
                        let mut ids = Vec::new();
                        for child in native_children
                            .and_then(|children| children.get(&tool.call_id))
                            .unwrap_or(&tool.child_thread_ids)
                        {
                            let native = child.split_once(':').map_or(child.as_str(), |(_, id)| id);
                            push_id(native, &mut ids);
                        }
                        if let Some(output) = &tool.output {
                            collect_ids(output, false, &mut ids);
                        }
                        for content in &tool.content {
                            match content {
                                ToolContent::Text { text }
                                | ToolContent::Terminal { output: text, .. } => {
                                    text_ids(text, &mut ids);
                                }
                                _ => {}
                            }
                        }
                        ids.sort_unstable();
                        ids.dedup();
                        let end = input["completedAtMs"].as_i64().or_else(|| {
                            matches!(tool.status, ToolStatus::Completed | ToolStatus::Failed).then(
                                || {
                                    next.filter(|(next_turn, _)| *next_turn == turn)
                                        .map_or(summary.updated_at, |(_, ts)| ts)
                                },
                            )
                        });
                        calls.push(Call {
                            id: tool.call_id.clone(),
                            targets,
                            ids,
                            command: command.chars().take(32_768).collect(),
                            cwd,
                            start: timestamp.or(input["startedAtMs"].as_i64()),
                            end,
                            activity,
                        });
                    }
                }
                _ => {}
            }
            if let Some(timestamp) = timestamp {
                next = Some((turn, timestamp));
            }
        }
        calls.reverse();
        for call in calls {
            if let Some(existing) = projection
                .calls
                .iter_mut()
                .find(|existing| existing.id == call.id)
            {
                *existing = call;
            } else {
                projection.calls.push(call);
            }
        }
        projection
    }

    pub fn from_history(summary: &ThreadSummary, items: &[ItemDraft]) -> Self {
        Self::from_items(
            summary,
            items
                .iter()
                .map(|item| (&item.kind, item.ts, item.turn_id.as_deref())),
            None,
        )
    }
}

fn string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| value[*key].as_str())
}

fn collect_ids(value: &Value, keyed: bool, ids: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if keyed {
                push_id(text, ids);
            } else {
                text_ids(text, ids);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_ids(value, keyed, ids);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let key = key.replace('_', "").to_ascii_lowercase();
                collect_ids(
                    value,
                    matches!(
                        key.as_str(),
                        "sessionid"
                            | "conversationid"
                            | "threadid"
                            | "agentid"
                            | "receiverthreadids"
                            | "childthreadid"
                            | "agentthreadid"
                            | "spawnedagent"
                    ),
                    ids,
                );
            }
        }
        _ => {}
    }
}

fn text_ids(text: &str, ids: &mut Vec<String>) {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.len() <= 256 * 1024 {
            if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                collect_ids(&value, false, ids);
                continue;
            }
        }
        let lower = line.to_ascii_lowercase();
        for label in [
            "session id:",
            "session_id",
            "conversation_id",
            "thread_id",
            "agentid:",
            "agent id:",
        ] {
            for (offset, _) in lower.match_indices(label) {
                let candidate =
                    line[offset + label.len()..].trim_start_matches(|character: char| {
                        character.is_whitespace() || matches!(character, '"' | '\'' | ':' | '=')
                    });
                let end = candidate
                    .find(|character: char| !id_character(character))
                    .unwrap_or(candidate.len());
                push_id(&candidate[..end], ids);
            }
        }
        for word in line.split(|character| !id_character(character)) {
            if word.len() == 36
                && word.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
            {
                push_id(word, ids);
            }
        }
    }
}

fn id_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

fn push_id(candidate: &str, ids: &mut Vec<String>) {
    if !candidate.is_empty() && candidate.len() <= 128 && candidate.chars().all(id_character) {
        ids.push(candidate.to_ascii_lowercase());
    }
}

fn shell_words(command: &str) -> Vec<(String, bool)> {
    let mut quote = None;
    let mut literal = false;
    let mut word = String::new();
    let mut words = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(character) = chars.next() {
        if matches!(character, '\\' | '`')
            && chars
                .peek()
                .is_some_and(|next| next.is_whitespace() || matches!(next, '\'' | '"'))
            && quote != Some('\'')
        {
            word.push(chars.next().unwrap());
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '"' | '\'') {
            quote = Some(character);
            literal = true;
        } else if quote.is_none() && (character.is_whitespace() || "|;&<>()".contains(character)) {
            if !word.is_empty() {
                words.push((std::mem::take(&mut word), literal));
            }
            literal = false;
            if !character.is_whitespace() {
                words.push((character.to_string(), false));
            }
        } else {
            word.push(character);
        }
    }
    if !word.is_empty() {
        words.push((word, literal));
    }
    words
}

fn invocations(command: &str) -> Vec<String> {
    let words = shell_words(command);
    let mut agents = Vec::new();
    let mut executable = true;
    for (index, (word, quoted)) in words.iter().enumerate() {
        if !quoted && matches!(word.as_str(), "|" | ";" | "&" | "(") {
            executable = true;
            continue;
        }
        if !executable {
            if *quoted
                && index > 0
                && matches!(
                    words[index - 1].0.as_str(),
                    "-c" | "-lc" | "-Command" | "/c"
                )
            {
                agents.extend(invocations(word));
            }
            continue;
        }
        let name = word
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(word)
            .to_ascii_lowercase();
        let name = name
            .trim_end_matches(".exe")
            .trim_end_matches(".cmd")
            .trim_end_matches(".ps1");
        if name.contains('=')
            || matches!(
                name,
                "env" | "command" | "time" | "timeout" | "nohup" | "uvx" | "npx"
            )
            || word.starts_with('-')
            || word.chars().all(|character| character.is_ascii_digit())
        {
            continue;
        }
        executable = false;
        let args = words[index + 1..].iter().take_while(|(arg, quoted)| {
            *quoted || !matches!(arg.as_str(), "|" | ";" | "&" | "<" | ">" | ")")
        });
        let (agent, modes): (&str, &[&str]) = match name {
            "codex" => ("codex", &["exec", "e"]),
            "claude" => ("claude", &["-p", "--print"]),
            "agy" => ("antigravity", &["-p", "--prompt"]),
            "devin" => ("devin", &["-p", "--print", "--prompt"]),
            _ => continue,
        };
        if args.into_iter().any(|(arg, quoted)| {
            !quoted
                && modes.iter().any(|mode| {
                    arg == mode
                        || arg
                            .strip_prefix(mode)
                            .is_some_and(|rest| rest.starts_with('='))
                })
        }) {
            agents.push(agent.into());
        }
    }
    agents
}

pub(crate) fn eligible(summary: &ThreadSummary) -> bool {
    summary.source == ThreadSource::Exec || summary.source == ThreadSource::Unknown
}

pub(crate) struct Matcher<'a> {
    child: &'a ThreadSummary,
    hint: Option<&'a str>,
    prompt: Option<String>,
    best: Option<(u8, ThreadParent)>,
    ambiguous: bool,
}

impl<'a> Matcher<'a> {
    pub fn new(child: &'a ThreadSummary, hint: Option<&'a str>, prompt: Option<&str>) -> Self {
        Self {
            child,
            hint,
            prompt: prompt.map(normalized).filter(|prompt| !prompt.is_empty()),
            best: None,
            ambiguous: false,
        }
    }

    pub fn consider(&mut self, parent: String, call: &Call) {
        let prompt = self.prompt.as_deref();
        let direct = score(self.child, self.hint, prompt, None, &parent, call);
        let matched = if direct.is_none_or(|(rank, _)| rank < 2)
            && prompt.is_some()
            && eligible(self.child)
        {
            stdin(&call.command, call.cwd.as_deref())
                .and_then(|input| score(self.child, self.hint, prompt, Some(&input), &parent, call))
                .or(direct)
        } else {
            direct
        };
        let Some((rank, confidence)) = matched else {
            return;
        };
        let candidate = ThreadParent {
            thread_id: parent,
            call_id: Some(call.id.clone()),
            confidence,
        };
        match &self.best {
            Some((best, _)) if *best > rank => {}
            Some((best, current)) if *best == rank => self.ambiguous |= *current != candidate,
            _ => {
                self.best = Some((rank, candidate));
                self.ambiguous = false;
            }
        }
    }

    pub fn finish(self) -> Option<ThreadParent> {
        if let Some((_, candidate)) = self.best.filter(|_| !self.ambiguous) {
            return Some(candidate);
        }
        self.hint.map(|hint| ThreadParent {
            thread_id: hint.into(),
            call_id: None,
            confidence: LinkConfidence::Exact,
        })
    }
}

fn score(
    child: &ThreadSummary,
    hint: Option<&str>,
    prompt: Option<&str>,
    stdin: Option<&str>,
    parent: &str,
    call: &Call,
) -> Option<(u8, LinkConfidence)> {
    if parent == child.id
        || hint.is_some_and(|hint| hint != parent)
        || !call.targets.contains(&child.agent_id)
        || child
            .created_at
            .zip(call.start)
            .is_some_and(|(child, call)| child < call)
    {
        return None;
    }
    let explicit = hint == Some(parent);
    if call
        .ids
        .iter()
        .any(|id| id.eq_ignore_ascii_case(&child.native_id))
    {
        return Some((if call.activity { 4 } else { 5 }, LinkConfidence::Exact));
    }
    if !explicit && !eligible(child) {
        return None;
    }
    if prompt.is_some_and(|prompt| {
        normalized(&call.command).contains(prompt)
            || stdin.is_some_and(|input| normalized(input).contains(prompt))
    }) {
        return Some(if explicit {
            (3, LinkConfidence::Exact)
        } else {
            (2, LinkConfidence::Prompt)
        });
    }
    let cwd_matches = child
        .cwd
        .as_ref()
        .zip(call.cwd.as_ref())
        .is_some_and(|(left, right)| {
            if cfg!(windows) {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        });
    if (explicit || eligible(child))
        && cwd_matches
        && child
            .created_at
            .zip(call.start)
            .is_some_and(|(started, call_start)| {
                started
                    <= call
                        .end
                        .unwrap_or(call_start.saturating_add(15 * 60 * 1000))
            })
    {
        return Some(if explicit {
            (3, LinkConfidence::Exact)
        } else {
            (1, LinkConfidence::Heuristic)
        });
    }
    None
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn stdin(command: &str, cwd: Option<&str>) -> Option<String> {
    let words = shell_words(command);
    let file = words
        .windows(2)
        .find_map(|pair| (pair[0].0 == "<" && !pair[0].1 && pair[1].0 != "<").then_some(&pair[1].0))
        .or_else(|| {
            words.windows(3).find_map(|parts| {
                (matches!(parts[0].0.as_str(), "cat" | "Get-Content" | "type") && parts[2].0 == "|")
                    .then_some(&parts[1].0)
            })
        })?;
    let cwd = Path::new(cwd?).canonicalize().ok()?;
    let path = cwd.join(file).canonicalize().ok()?;
    if !path.starts_with(&cwd) || !path.is_file() {
        return None;
    }
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(128 * 1024)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(agent: &str) -> ThreadSummary {
        ThreadSummary {
            id: format!("{agent}:child-id"),
            agent_id: agent.into(),
            native_id: "child-id".into(),
            title: "Child".into(),
            preview: None,
            cwd: Some("/project".into()),
            created_at: Some(25),
            updated_at: 30,
            source: ThreadSource::Exec,
            origin: None,
            status: vetro_proto::ThreadStatus::Idle,
            live: false,
            resumable: true,
            parent: None,
            child_count: 0,
            model: None,
            archived: false,
            internal: true,
        }
    }

    #[test]
    fn matches_tiers_and_rejects_wrong_evidence() {
        use LinkConfidence::*;
        let child = child("codex");
        for (label, output, command, input, cwd, start, expected) in [
            (
                "exact",
                "session id: child-id",
                "codex exec -",
                None,
                "/other",
                24,
                Some(Exact),
            ),
            (
                "exact boundary",
                "session id: child-id-other",
                "codex exec -",
                None,
                "/other",
                35,
                None,
            ),
            (
                "command prompt",
                "",
                "codex exec 'a specific prompt'",
                None,
                "/other",
                24,
                Some(Prompt),
            ),
            (
                "reused prompt after child started",
                "",
                "codex exec 'a specific prompt'",
                None,
                "/project",
                26,
                None,
            ),
            (
                "stdin prompt",
                "",
                "codex exec - < prompt.txt",
                Some("a specific\nprompt"),
                "/other",
                24,
                Some(Prompt),
            ),
            (
                "wrong prompt",
                "",
                "codex exec 'a different prompt'",
                None,
                "/other",
                35,
                None,
            ),
            (
                "heuristic",
                "",
                "codex exec -",
                None,
                "/project",
                20,
                Some(Heuristic),
            ),
            ("wrong cwd", "", "codex exec -", None, "/other", 20, None),
            (
                "wrong window",
                "",
                "codex exec -",
                None,
                "/project",
                26,
                None,
            ),
            (
                "not a command",
                "",
                "echo codex exec 'a specific prompt'",
                None,
                "/project",
                20,
                None,
            ),
        ] {
            let mut ids = Vec::new();
            text_ids(output, &mut ids);
            let call = Call {
                id: "call".into(),
                targets: invocations(command),
                ids,
                command: command.into(),
                cwd: Some(cwd.into()),
                start: Some(start),
                end: Some(30),
                activity: false,
            };
            let result = score(
                &child,
                None,
                Some("a specific prompt"),
                input,
                "claude:parent",
                &call,
            );
            assert_eq!(
                result.map(|(_, confidence)| confidence),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn matches_all_agent_directions_and_native_task() {
        let commands = [
            ("codex", "codex exec 'a specific prompt'"),
            ("claude", r#""C:\Tools\claude.exe" -p 'a specific prompt'"#),
            ("antigravity", "cat prompt.txt | agy -p 'a specific prompt'"),
            ("devin", "devin -p 'a specific prompt'"),
        ];
        for (parent, _) in commands {
            for (agent, command) in commands {
                let child = child(agent);
                let call = Call {
                    id: "call".into(),
                    targets: invocations(command),
                    ids: Vec::new(),
                    command: command.into(),
                    cwd: None,
                    start: None,
                    end: None,
                    activity: false,
                };
                assert_eq!(
                    score(
                        &child,
                        None,
                        Some("a specific prompt"),
                        None,
                        &format!("{parent}:parent"),
                        &call
                    ),
                    Some((2, LinkConfidence::Prompt)),
                );
            }
        }
        let mut child = child("claude");
        child.source = ThreadSource::Cli;
        let mut ids = Vec::new();
        text_ids("agentId: child-id", &mut ids);
        let call = Call {
            id: "Task".into(),
            targets: vec!["claude".into()],
            ids,
            command: String::new(),
            cwd: None,
            start: None,
            end: None,
            activity: false,
        };
        let mut matcher = Matcher::new(&child, Some("claude:parent"), None);
        matcher.consider("claude:wrong-parent".into(), &call);
        matcher.consider("claude:parent".into(), &call);
        assert_eq!(matcher.finish().unwrap().call_id.as_deref(), Some("Task"));
        assert_eq!(
            Matcher::new(&child, Some("claude:parent"), None)
                .finish()
                .unwrap()
                .confidence,
            LinkConfidence::Exact
        );
    }

    #[test]
    fn strongest_unique_match_wins() {
        let child = child("codex");
        let call = Call {
            id: "first".into(),
            targets: vec!["codex".into()],
            ids: Vec::new(),
            command: "codex exec -".into(),
            cwd: child.cwd.clone(),
            start: Some(20),
            end: Some(30),
            activity: false,
        };
        let exact = Call {
            id: "exact".into(),
            ids: vec![child.native_id.clone()],
            ..call.clone()
        };
        let mut matcher = Matcher::new(&child, None, None);
        matcher.consider("claude:first".into(), &call);
        matcher.consider("claude:second".into(), &call);
        assert!(matcher.finish().is_none());
        let mut matcher = Matcher::new(&child, None, None);
        matcher.consider("claude:first".into(), &call);
        matcher.consider("claude:second".into(), &call);
        matcher.consider("claude:exact".into(), &exact);
        assert_eq!(matcher.finish().unwrap().confidence, LinkConfidence::Exact);
    }
}
