use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};

const MAX_SHAPES: usize = 1024;

#[derive(Default)]
struct Diagnostics {
    seen: HashSet<(String, String, String)>,
    pending: Vec<String>,
}

static DIAGNOSTICS: LazyLock<Mutex<Diagnostics>> = LazyLock::new(Mutex::default);

pub fn report(agent: &str, record: &'static str, discriminator: Option<&str>) {
    DIAGNOSTICS
        .lock()
        .unwrap()
        .report(agent, record, discriminator);
}

pub fn drain() -> Vec<String> {
    std::mem::take(&mut DIAGNOSTICS.lock().unwrap().pending)
}

impl Diagnostics {
    fn report(&mut self, agent: &str, record: &str, discriminator: Option<&str>) {
        let agent = identifier(Some(agent));
        let kind = identifier(discriminator);
        if self.seen.len() == MAX_SHAPES
            || !self
                .seen
                .insert((agent.to_owned(), record.to_owned(), kind.to_owned()))
        {
            return;
        }
        self.pending
            .push(format!("Native format drift: {agent} {record} ({kind})"));
    }
}

fn identifier(value: Option<&str>) -> &str {
    match value {
        None => "missing",
        Some(value)
            if !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')) =>
        {
            value
        }
        Some(_) => "invalid",
    }
}

#[test]
fn deduplicates_across_drains_and_keeps_agent_and_record_distinct() {
    let mut diagnostics = Diagnostics::default();
    diagnostics.report("devin", "ACP sessionUpdate", Some("sample"));
    assert_eq!(std::mem::take(&mut diagnostics.pending).len(), 1);
    diagnostics.report("devin", "ACP sessionUpdate", Some("sample"));
    diagnostics.report("kiro", "ACP sessionUpdate", Some("sample"));
    diagnostics.report("kiro", "ACP content", Some("sample"));
    diagnostics.report("kiro", "ACP sessionUpdate", Some("path/to/private text\n"));
    assert_eq!(
        diagnostics.pending,
        [
            "Native format drift: kiro ACP sessionUpdate (sample)",
            "Native format drift: kiro ACP content (sample)",
            "Native format drift: kiro ACP sessionUpdate (invalid)",
        ]
    );
}
