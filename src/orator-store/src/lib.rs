#![forbid(unsafe_code)]

//! orator-store: Durable records on fluent-db: the append-only event log, transcripts, sessions, notes, checkpoints, and audit rows.

pub mod events;
pub mod schema;
pub mod store;

pub use events::{EventLog, StoredEvent, REPLAY_PAGE};
pub use schema::{migrations, CURRENT_VERSION};
pub use store::{
    backup_db, open_store, spawn_writer, writer_feed_config, InstanceLock, OpenError, WriteReq,
};
