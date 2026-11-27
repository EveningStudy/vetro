CREATE TABLE threads (
    id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, source TEXT NOT NULL,
    originator TEXT, cwd TEXT NOT NULL, name TEXT, title TEXT NOT NULL,
    preview TEXT, first_user_message TEXT, created_at INTEGER NOT NULL,
    created_at_ms INTEGER, updated_at INTEGER NOT NULL, updated_at_ms INTEGER,
    archived INTEGER NOT NULL, model TEXT, attached_thread_parent_id TEXT
);
INSERT INTO threads VALUES
    ('20000000-0000-4000-8000-000000000002', '', 'exec', 'codex_exec',
     '/fixture', NULL, 'Fruit fixture', 'Name a fruit.', 'Name a fruit.',
     1800000000, 1800000000123, 1800000003, 1800000003456, 0, 'fixture-model', NULL),
    ('fixture-cloud', '', 'chatgpt', NULL, '', NULL, 'Cloud stub', NULL, '',
     1800000000, NULL, 1800000003, NULL, 0, NULL, NULL);
