use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use futures::{Stream, StreamExt};
use mikrotik_api::Response;
use serde::Serialize;
use tokio::task::JoinHandle;

use super::Row;

/// Maximum number of recent events retained in memory per streaming task.
pub const MAX_BUFFERED_EVENTS_PER_TASK: usize = 500;

/// Lifecycle state of an asynchronous MCP Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is actively consuming events from the router.
    Working,
    /// Task completed normally (e.g. router finished or stream closed).
    Completed,
    /// Task encountered an unrecoverable router or network error.
    Failed,
    /// Task was cancelled by client request.
    Cancelled,
}

/// Point-in-time snapshot of a task's buffered events and execution state.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    /// Unique MCP Task identifier.
    pub task_id: String,
    /// Command path that initiated the stream.
    pub command: String,
    /// RouterOS API tag associated with the stream.
    pub tag: u16,
    /// Current task lifecycle status.
    pub status: TaskStatus,
    /// Accumulated events since last poll (or total retained events).
    pub events: Vec<Row>,
    /// Number of dropped events due to ring-buffer overflow.
    pub dropped_events: usize,
    /// Error message if status is Failed.
    pub error: Option<String>,
}

struct TaskEntry {
    command: String,
    tag: u16,
    status: TaskStatus,
    buffer: VecDeque<Row>,
    dropped_events: usize,
    error: Option<String>,
    handle: Option<JoinHandle<()>>,
}

/// Thread-safe registry managing streaming MCP Tasks and their buffered outputs.
#[derive(Clone, Default)]
pub struct TaskRegistry {
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    next_id: Arc<AtomicU64>,
}

impl TaskRegistry {
    /// Creates a new, empty `TaskRegistry`.
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Spawns a background worker consuming a `StreamingCall` and returns the allocated `task_id`.
    pub fn spawn_stream_task<S>(&self, command: String, tag: u16, mut stream: S) -> String
    where
        S: Stream<Item = Response<Row>> + Send + Unpin + 'static,
    {
        let id_num = self.next_id.fetch_add(1, Ordering::Relaxed);
        let task_id = format!("task-{id_num}");

        let entry = TaskEntry {
            command: command.clone(),
            tag,
            status: TaskStatus::Working,
            buffer: VecDeque::with_capacity(64),
            dropped_events: 0,
            error: None,
            handle: None,
        };

        {
            let mut guard = self.tasks.lock().unwrap();
            guard.insert(task_id.clone(), entry);
        }

        let tasks_clone = self.tasks.clone();
        let task_id_clone = task_id.clone();

        let handle = tokio::spawn(async move {
            while let Some(item) = stream.next().await {
                let mut guard = tasks_clone.lock().unwrap();
                let Some(entry) = guard.get_mut(&task_id_clone) else {
                    break;
                };

                // Stop processing if cancelled
                if entry.status == TaskStatus::Cancelled {
                    break;
                }

                match item {
                    Response::Reply(row) => {
                        if entry.buffer.len() >= MAX_BUFFERED_EVENTS_PER_TASK {
                            entry.buffer.pop_front();
                            entry.dropped_events += 1;
                        }
                        entry.buffer.push_back(row);
                    }
                    Response::Trap { message, .. } => {
                        entry.status = TaskStatus::Failed;
                        entry.error = Some(message);
                        break;
                    }
                    Response::Fatal => {
                        entry.status = TaskStatus::Failed;
                        entry.error = Some("Connection closed by router (Fatal)".into());
                        break;
                    }
                    Response::Done => {
                        entry.status = TaskStatus::Completed;
                        break;
                    }
                }
            }

            // If the stream ended without error or explicit cancel, mark completed
            let mut guard = tasks_clone.lock().unwrap();
            if let Some(entry) = guard.get_mut(&task_id_clone) {
                if entry.status == TaskStatus::Working {
                    entry.status = TaskStatus::Completed;
                }
            }
        });

        {
            let mut guard = self.tasks.lock().unwrap();
            if let Some(entry) = guard.get_mut(&task_id) {
                entry.handle = Some(handle);
            }
        }

        task_id
    }

    /// Reads buffered events from a task. If `drain` is true, removes read events from the buffer.
    pub fn poll_task(&self, task_id: &str, drain: bool) -> Option<TaskSnapshot> {
        let mut guard = self.tasks.lock().unwrap();
        let entry = guard.get_mut(task_id)?;

        let events: Vec<Row> = if drain {
            entry.buffer.drain(..).collect()
        } else {
            entry.buffer.iter().cloned().collect()
        };

        Some(TaskSnapshot {
            task_id: task_id.to_string(),
            command: entry.command.clone(),
            tag: entry.tag,
            status: entry.status,
            events,
            dropped_events: entry.dropped_events,
            error: entry.error.clone(),
        })
    }

    /// Marks a task as `Cancelled` and returns its router `.tag` so `/cancel` can be sent.
    pub fn mark_cancelling(&self, task_id: &str) -> Option<u16> {
        let mut guard = self.tasks.lock().unwrap();
        let entry = guard.get_mut(task_id)?;
        if entry.status == TaskStatus::Working {
            entry.status = TaskStatus::Cancelled;
            if let Some(handle) = entry.handle.take() {
                handle.abort();
            }
            Some(entry.tag)
        } else {
            None
        }
    }

    /// Finalizes cancellation state.
    pub fn finish_cancel(&self, task_id: &str) {
        let mut guard = self.tasks.lock().unwrap();
        if let Some(entry) = guard.get_mut(task_id) {
            entry.status = TaskStatus::Cancelled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_task_registry_lifecycle_and_ring_buffer() {
        let registry = TaskRegistry::new();

        // Create a simulated stream of 10 items
        let items: Vec<Response<Row>> = (0..10)
            .map(|i| {
                let mut map = Row::new();
                map.insert("index".to_string(), i.to_string());
                Response::Reply(map)
            })
            .collect();

        let stream = futures::stream::iter(items);
        let task_id = registry.spawn_stream_task("/interface/listen".into(), 42, stream);

        // Allow background task to process stream
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let snapshot = registry.poll_task(&task_id, true).expect("task exists");
        assert_eq!(snapshot.task_id, task_id);
        assert_eq!(snapshot.tag, 42);
        assert_eq!(snapshot.status, TaskStatus::Completed);
        assert_eq!(snapshot.events.len(), 10);
        assert_eq!(snapshot.events[0].get("index").unwrap(), "0");
        assert_eq!(snapshot.events[9].get("index").unwrap(), "9");

        // Polling again with drain should yield 0 events
        let snapshot2 = registry.poll_task(&task_id, false).expect("task exists");
        assert_eq!(snapshot2.events.len(), 0);
    }
}
