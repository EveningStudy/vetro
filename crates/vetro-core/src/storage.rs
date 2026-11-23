use std::{collections::HashMap, sync::Mutex};

use vetro_proto::ThreadSummary;

pub trait ThreadStore: Send + Sync {
    fn get(&self, id: &str) -> Option<ThreadSummary>;
    fn all(&self) -> Vec<ThreadSummary>;
    fn upsert_attached(&self, thread: ThreadSummary);
    fn replace_history(&self, agent_id: &str, threads: Vec<ThreadSummary>) -> HistoryChanges;
}

#[derive(Default)]
pub struct HistoryChanges {
    pub upserted: Vec<ThreadSummary>,
    pub removed: Vec<String>,
}

#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, Entry>>);

#[derive(Default)]
struct Entry {
    history: Option<ThreadSummary>,
    attached: Option<ThreadSummary>,
}

impl Entry {
    fn summary(&self) -> Option<&ThreadSummary> {
        self.attached.as_ref().or(self.history.as_ref())
    }
}

impl ThreadStore for MemoryStore {
    fn get(&self, id: &str) -> Option<ThreadSummary> {
        self.0.lock().unwrap().get(id)?.summary().cloned()
    }

    fn all(&self) -> Vec<ThreadSummary> {
        self.0
            .lock()
            .unwrap()
            .values()
            .filter_map(|entry| entry.summary().cloned())
            .collect()
    }

    fn upsert_attached(&self, thread: ThreadSummary) {
        let id = thread.id.clone();
        self.0.lock().unwrap().entry(id).or_default().attached = Some(thread);
    }

    fn replace_history(&self, agent_id: &str, threads: Vec<ThreadSummary>) -> HistoryChanges {
        let mut index = self.0.lock().unwrap();
        let mut changes = HistoryChanges::default();
        let mut incoming: HashMap<_, _> = threads
            .into_iter()
            .map(|thread| (thread.id.clone(), thread))
            .collect();
        index.retain(|id, entry| {
            if entry
                .summary()
                .is_some_and(|thread| thread.agent_id == agent_id)
            {
                let before = entry.summary().cloned();
                entry.history = incoming.remove(id);
                if let Some(summary) = entry.summary() {
                    if Some(summary) != before.as_ref() {
                        changes.upserted.push(summary.clone());
                    }
                } else {
                    changes.removed.push(id.clone());
                    return false;
                }
            }
            true
        });
        for (id, thread) in incoming {
            changes.upserted.push(thread.clone());
            index.insert(
                id,
                Entry {
                    history: Some(thread),
                    attached: None,
                },
            );
        }
        changes
    }
}
