CREATE TABLE IF NOT EXISTS completed_workflows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    machine_id TEXT NOT NULL,
    workflow_info TEXT NOT NULL,
    completed_at DATETIME NOT NULL,
    FOREIGN KEY (machine_id) REFERENCES machines(id) ON DELETE CASCADE
);

CREATE INDEX idx_completed_workflows_machine_id ON completed_workflows(machine_id);
CREATE INDEX idx_completed_workflows_completed_at ON completed_workflows(completed_at);

-- For SQLite, we'll handle cleanup in the application code instead of using triggers 