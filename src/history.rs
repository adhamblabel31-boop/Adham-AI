use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: i64 = 7;
static DATABASE_OPEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Conversation {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) created_at: u64,
    pub(super) updated_at: u64,
    pub(super) revision: u64,
    pub(super) messages: Vec<StoredMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredMessage {
    pub(super) id: u64,
    pub(super) role: Role,
    pub(super) content: String,
    pub(super) model_context: bool,
    pub(super) turn_id: Option<u64>,
    pub(super) request_id: Option<u64>,
    pub(super) model: Option<String>,
    pub(super) status: Option<ResponseStatus>,
    pub(super) status_detail: Option<String>,
    pub(super) created_at: u64,
    pub(super) document_context: bool,
    pub(super) source_status: Option<String>,
    pub(super) sources: Vec<MessageSource>,
    pub(super) document_context_mode: String,
    pub(super) selected_document_ids: Vec<String>,
    pub(super) deep_think: bool,
    pub(super) thinking_received: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MessageSource {
    pub(super) document_id: String,
    pub(super) filename: String,
    pub(super) excerpt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResponseStatus {
    Generating,
    Completed,
    Cancelled,
    Failed,
    Replaced,
}

pub(super) struct HistoryStore {
    pub(super) connection: Connection,
}

impl Conversation {
    pub(super) fn new(now: u64) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Self {
            id: format!("{unique_time}-{sequence}"),
            title: "New Chat".into(),
            created_at: now,
            updated_at: now,
            revision: 0,
            messages: Vec::new(),
        }
    }
}

impl HistoryStore {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let _migration_guard = DATABASE_OPEN_LOCK
            .lock()
            .map_err(|error| format!("Could not lock local database initialization: {error}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create chat history directory: {error}"))?;
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("Could not open chat history database: {error}"))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("Could not configure chat history database: {error}"))?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(|error| format!("Could not enable chat history foreign keys: {error}"))?;

        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), String> {
        let version: i64 = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| format!("Could not read chat history schema version: {error}"))?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "Chat history database version {version} is newer than this application supports."
            ));
        }

        if version < 1 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("Could not begin chat history migration: {error}"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE conversations (
                        id TEXT PRIMARY KEY NOT NULL,
                        title TEXT NOT NULL CHECK(length(title) > 0),
                        created_at INTEGER NOT NULL CHECK(created_at >= 0),
                        updated_at INTEGER NOT NULL CHECK(updated_at >= created_at),
                        revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0)
                    );
                    CREATE INDEX conversations_updated_at
                        ON conversations(updated_at DESC, created_at DESC);
                    CREATE TABLE messages (
                        id INTEGER PRIMARY KEY NOT NULL,
                        conversation_id TEXT NOT NULL
                            REFERENCES conversations(id) ON DELETE CASCADE,
                        position INTEGER NOT NULL CHECK(position >= 0),
                        role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
                        content TEXT NOT NULL,
                        model_context INTEGER NOT NULL CHECK(model_context IN (0, 1)),
                        turn_id INTEGER,
                        request_id INTEGER,
                        model TEXT,
                        status TEXT CHECK(status IS NULL OR status IN (
                            'generating', 'completed', 'cancelled', 'failed', 'replaced'
                        )),
                        status_detail TEXT,
                        created_at INTEGER NOT NULL CHECK(created_at >= 0),
                        UNIQUE(conversation_id, position)
                    );",
                )
                .map_err(|error| format!("Could not create chat history schema: {error}"))?;
            transaction
                .pragma_update(None, "user_version", 1)
                .map_err(|error| {
                    format!("Could not record chat history schema version: {error}")
                })?;
            transaction
                .commit()
                .map_err(|error| format!("Could not finish chat history migration: {error}"))?;
        }
        if version < 2 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("Could not begin chat history migration: {error}"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE conversation_tombstones (
                        id TEXT PRIMARY KEY NOT NULL,
                        deleted_at INTEGER NOT NULL CHECK(deleted_at >= 0)
                    );",
                )
                .map_err(|error| {
                    format!("Could not create chat history deletion table: {error}")
                })?;
            transaction
                .pragma_update(None, "user_version", 2)
                .map_err(|error| {
                    format!("Could not record chat history schema version: {error}")
                })?;
            transaction
                .commit()
                .map_err(|error| format!("Could not finish chat history migration: {error}"))?;
        }
        if version < 3 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("Could not begin document storage migration: {error}"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE documents (
                        id TEXT PRIMARY KEY NOT NULL,
                        original_filename TEXT NOT NULL CHECK(length(original_filename) > 0),
                        source_path TEXT NOT NULL,
                        file_type TEXT NOT NULL CHECK(file_type IN ('txt', 'md', 'pdf')),
                        file_size INTEGER NOT NULL CHECK(file_size >= 0),
                        imported_at INTEGER NOT NULL CHECK(imported_at >= 0),
                        fingerprint TEXT NOT NULL UNIQUE,
                        indexing_status TEXT NOT NULL CHECK(indexing_status IN ('indexed', 'error'))
                    );
                    CREATE INDEX documents_imported_at ON documents(imported_at DESC);
                    CREATE TABLE document_contents (
                        document_id TEXT PRIMARY KEY NOT NULL
                            REFERENCES documents(id) ON DELETE CASCADE,
                        extracted_text TEXT NOT NULL
                    );
                    CREATE VIRTUAL TABLE document_search USING fts5(
                        document_id UNINDEXED,
                        filename,
                        content,
                        tokenize = 'unicode61 remove_diacritics 2'
                    );",
                )
                .map_err(|error| format!("Could not create document storage schema: {error}"))?;
            transaction
                .pragma_update(None, "user_version", 3)
                .map_err(|error| format!("Could not record document schema version: {error}"))?;
            transaction
                .commit()
                .map_err(|error| format!("Could not finish document storage migration: {error}"))?;
        }
        if version < 4 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("Could not begin document citation migration: {error}"))?;
            transaction
                .execute_batch(
                    "ALTER TABLE messages ADD COLUMN document_context INTEGER NOT NULL DEFAULT 0
                        CHECK(document_context IN (0, 1));
                    ALTER TABLE messages ADD COLUMN source_status TEXT;
                    CREATE TABLE message_sources (
                        assistant_message_id INTEGER NOT NULL
                            REFERENCES messages(id) ON DELETE CASCADE,
                        position INTEGER NOT NULL CHECK(position >= 0),
                        document_id TEXT NOT NULL,
                        filename TEXT NOT NULL,
                        excerpt TEXT NOT NULL,
                        PRIMARY KEY(assistant_message_id, position)
                    );
                    CREATE INDEX message_sources_document
                        ON message_sources(document_id);",
                )
                .map_err(|error| format!("Could not create document citation storage: {error}"))?;
            transaction
                .pragma_update(None, "user_version", 4)
                .map_err(|error| format!("Could not record citation schema version: {error}"))?;
            transaction.commit().map_err(|error| {
                format!("Could not finish document citation migration: {error}")
            })?;
        }
        if version < 5 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("Could not begin chat context migration: {error}"))?;
            transaction
                .execute_batch(
                    "ALTER TABLE messages ADD COLUMN document_context_mode TEXT NOT NULL
                        DEFAULT 'no_documents'
                        CHECK(document_context_mode IN (
                            'no_documents', 'all_documents', 'selected_documents'
                        ));
                    CREATE TABLE message_context_documents (
                        message_id INTEGER NOT NULL
                            REFERENCES messages(id) ON DELETE CASCADE,
                        document_id TEXT NOT NULL,
                        position INTEGER NOT NULL CHECK(position >= 0),
                        PRIMARY KEY(message_id, document_id),
                        UNIQUE(message_id, position)
                    );
                    CREATE INDEX message_context_documents_document
                        ON message_context_documents(document_id);",
                )
                .map_err(|error| format!("Could not create chat context storage: {error}"))?;
            transaction
                .pragma_update(None, "user_version", 5)
                .map_err(|error| {
                    format!("Could not record chat context schema version: {error}")
                })?;
            transaction
                .commit()
                .map_err(|error| format!("Could not finish chat context migration: {error}"))?;
        }
        if version < 6 {
            let has_deep_think: bool = self
                .connection
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM pragma_table_info('messages') WHERE name = 'deep_think'
                    )",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| {
                    format!("Could not inspect Deep Think message storage: {error}")
                })?;
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| {
                    format!("Could not begin Deep Think storage migration: {error}")
                })?;
            if !has_deep_think {
                transaction
                    .execute_batch(
                        "ALTER TABLE messages ADD COLUMN deep_think INTEGER NOT NULL DEFAULT 0
                            CHECK(deep_think IN (0, 1));",
                    )
                    .map_err(|error| format!("Could not create Deep Think storage: {error}"))?;
            }
            transaction
                .pragma_update(None, "user_version", 6)
                .map_err(|error| format!("Could not record Deep Think schema version: {error}"))?;
            transaction.commit().map_err(|error| {
                format!("Could not finish Deep Think schema migration: {error}")
            })?;
        }
        if version < 7 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| {
                    format!("Could not begin Deep Think activity migration: {error}")
                })?;
            transaction
                .execute_batch(
                    "ALTER TABLE messages ADD COLUMN thinking_received INTEGER NOT NULL DEFAULT 0
                        CHECK(thinking_received IN (0, 1));",
                )
                .map_err(|error| {
                    format!("Could not create Deep Think activity storage: {error}")
                })?;
            transaction
                .pragma_update(None, "user_version", 7)
                .map_err(|error| {
                    format!("Could not record Deep Think activity schema version: {error}")
                })?;
            transaction.commit().map_err(|error| {
                format!("Could not finish Deep Think activity migration: {error}")
            })?;
        }
        Ok(())
    }

    pub(super) fn create_conversation(
        &mut self,
        conversation: &Conversation,
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("Could not begin conversation creation: {error}"))?;
        let deleted: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_tombstones WHERE id = ?1)",
                [&conversation.id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not check conversation tombstones: {error}"))?;
        if deleted {
            return Err("A deleted conversation ID cannot be reused.".into());
        }
        if insert_conversation(&transaction, conversation)? {
            insert_messages(&transaction, conversation)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not finish conversation creation: {error}"))
    }

    pub(super) fn save_conversation(&mut self, conversation: &Conversation) -> Result<(), String> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("Could not begin conversation save: {error}"))?;
        let deleted: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_tombstones WHERE id = ?1)",
                [&conversation.id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not check conversation tombstones: {error}"))?;
        if deleted {
            transaction
                .commit()
                .map_err(|error| format!("Could not finish stale conversation save: {error}"))?;
            return Ok(());
        }
        let stored_revision = transaction
            .query_row(
                "SELECT revision FROM conversations WHERE id = ?1",
                [&conversation.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("Could not check conversation revision: {error}"))?;
        if stored_revision.is_some_and(|revision| revision > as_i64(conversation.revision)) {
            transaction
                .commit()
                .map_err(|error| format!("Could not finish stale conversation save: {error}"))?;
            return Ok(());
        }

        transaction
            .execute(
                "INSERT INTO conversations (id, title, created_at, updated_at, revision)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at,
                    revision = excluded.revision",
                params![
                    conversation.id,
                    conversation.title,
                    as_i64(conversation.created_at),
                    as_i64(conversation.updated_at),
                    as_i64(conversation.revision),
                ],
            )
            .map_err(|error| format!("Could not save conversation: {error}"))?;
        transaction
            .execute(
                "DELETE FROM messages WHERE conversation_id = ?1",
                [&conversation.id],
            )
            .map_err(|error| format!("Could not replace conversation messages: {error}"))?;
        insert_messages(&transaction, conversation)?;
        transaction
            .commit()
            .map_err(|error| format!("Could not finish conversation save: {error}"))
    }

    pub(super) fn list_conversations(&self) -> Result<Vec<Conversation>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, title, created_at, updated_at, revision
                 FROM conversations
                 ORDER BY updated_at DESC, created_at DESC, id ASC",
            )
            .map_err(|error| format!("Could not prepare conversation list: {error}"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Could not list conversations: {error}"))?;

        let ids = rows
            .map(|row| row.map_err(|error| format!("Could not read conversation id: {error}")))
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| {
                self.load_conversation(id)?
                    .ok_or_else(|| "Conversation disappeared while listing chat history.".into())
            })
            .collect()
    }

    pub fn load_conversation(&self, id: &str) -> Result<Option<Conversation>, String> {
        let row = self
            .connection
            .query_row(
                "SELECT title, created_at, updated_at, revision
                 FROM conversations WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("Could not load conversation: {error}"))?;
        let Some((title, created_at, updated_at, revision)) = row else {
            return Ok(None);
        };

        Ok(Some(Conversation {
            id: id.into(),
            title,
            created_at: as_u64(created_at)?,
            updated_at: as_u64(updated_at)?,
            revision: as_u64(revision)?,
            messages: self.load_messages(id)?,
        }))
    }

    pub fn rename_conversation(
        &mut self,
        id: &str,
        title: &str,
        updated_at: u64,
        revision: u64,
    ) -> Result<Option<u64>, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("Conversation title cannot be empty.".into());
        }
        let changed = self
            .connection
            .execute(
                "UPDATE conversations
                 SET title = ?2, updated_at = MAX(updated_at, ?3),
                     revision = MAX(revision, ?4) + 1
                 WHERE id = ?1",
                params![id, title, as_i64(updated_at), as_i64(revision)],
            )
            .map_err(|error| format!("Could not rename conversation: {error}"))?;
        if changed == 0 {
            return Ok(None);
        }
        let revision = self
            .connection
            .query_row(
                "SELECT revision FROM conversations WHERE id = ?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("Could not verify renamed conversation: {error}"))?;
        Ok(Some(as_u64(revision)?))
    }

    pub(super) fn delete_conversation(&mut self, id: &str) -> Result<bool, String> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("Could not begin conversation deletion: {error}"))?;
        let deleted = transaction
            .execute("DELETE FROM conversations WHERE id = ?1", [id])
            .map_err(|error| format!("Could not delete conversation: {error}"))?;
        transaction
            .execute(
                "INSERT INTO conversation_tombstones (id, deleted_at)
                 VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING",
                params![id, as_i64(now_millis())],
            )
            .map_err(|error| format!("Could not record conversation deletion: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not finish conversation deletion: {error}"))?;
        Ok(deleted == 1)
    }

    fn load_messages(&self, conversation_id: &str) -> Result<Vec<StoredMessage>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, role, content, model_context, turn_id, request_id, model,
                        status, status_detail, created_at, document_context, source_status,
                        document_context_mode, deep_think, thinking_received
                 FROM messages
                 WHERE conversation_id = ?1
                 ORDER BY position ASC",
            )
            .map_err(|error| format!("Could not prepare conversation messages: {error}"))?;
        let rows = statement
            .query_map([conversation_id], |row| {
                let role: String = row.get(1)?;
                let status: Option<String> = row.get(7)?;
                Ok(StoredMessage {
                    id: as_u64(row.get::<_, i64>(0)?).map_err(to_sql_error)?,
                    role: Role::from_db(&role)?,
                    content: row.get(2)?,
                    model_context: row.get(3)?,
                    turn_id: row
                        .get::<_, Option<i64>>(4)?
                        .map(as_u64)
                        .transpose()
                        .map_err(to_sql_error)?,
                    request_id: row
                        .get::<_, Option<i64>>(5)?
                        .map(as_u64)
                        .transpose()
                        .map_err(to_sql_error)?,
                    model: row.get(6)?,
                    status: status.as_deref().map(ResponseStatus::from_db).transpose()?,
                    status_detail: row.get(8)?,
                    created_at: as_u64(row.get::<_, i64>(9)?).map_err(to_sql_error)?,
                    document_context: row.get(10)?,
                    source_status: row.get(11)?,
                    sources: Vec::new(),
                    document_context_mode: row.get(12)?,
                    selected_document_ids: Vec::new(),
                    deep_think: row.get(13)?,
                    thinking_received: row.get(14)?,
                })
            })
            .map_err(|error| format!("Could not load conversation messages: {error}"))?;

        let mut messages = rows
            .map(|row| row.map_err(|error| format!("Could not read conversation message: {error}")))
            .collect::<Result<Vec<_>, _>>()?;
        for message in &mut messages {
            message.sources = self.load_message_sources(message.id)?;
            message.selected_document_ids = self.load_message_context_documents(message.id)?;
        }
        Ok(messages)
    }

    fn load_message_context_documents(&self, message_id: u64) -> Result<Vec<String>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT document_id FROM message_context_documents
                 WHERE message_id = ?1 ORDER BY position",
            )
            .map_err(|error| format!("Could not prepare selected chat documents: {error}"))?;
        let rows = statement
            .query_map([as_i64(message_id)], |row| row.get(0))
            .map_err(|error| format!("Could not load selected chat documents: {error}"))?;
        rows.map(|row| {
            row.map_err(|error| format!("Could not read selected chat document: {error}"))
        })
        .collect()
    }

    fn load_message_sources(&self, message_id: u64) -> Result<Vec<MessageSource>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT document_id, filename, excerpt FROM message_sources
                 WHERE assistant_message_id = ?1 ORDER BY position",
            )
            .map_err(|error| format!("Could not prepare response sources: {error}"))?;
        let rows = statement
            .query_map([as_i64(message_id)], |row| {
                Ok(MessageSource {
                    document_id: row.get(0)?,
                    filename: row.get(1)?,
                    excerpt: row.get(2)?,
                })
            })
            .map_err(|error| format!("Could not load response sources: {error}"))?;
        rows.map(|row| row.map_err(|error| format!("Could not read response source: {error}")))
            .collect()
    }
}

pub(super) async fn load_all_async() -> Result<Vec<Conversation>, String> {
    tokio::task::spawn_blocking(|| {
        let store = HistoryStore::open(&storage_path()?)?;
        store.list_conversations()
    })
    .await
    .map_err(|error| format!("Chat history worker failed: {error}"))?
}

pub(super) async fn create_async(conversation: Conversation) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = storage_path()?;
        HistoryStore::open(&path)?.create_conversation(&conversation)
    })
    .await
    .map_err(|error| format!("Chat history worker failed: {error}"))?
}

pub(super) async fn save_async(conversation: Conversation) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = storage_path()?;
        HistoryStore::open(&path)?.save_conversation(&conversation)
    })
    .await
    .map_err(|error| format!("Chat history worker failed: {error}"))?
}

pub(super) async fn rename_async(
    id: String,
    title: String,
    updated_at: u64,
    revision: u64,
) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || {
        let path = storage_path()?;
        let mut store = HistoryStore::open(&path)?;
        store
            .rename_conversation(&id, &title, updated_at, revision)?
            .ok_or_else(|| "Conversation no longer exists.".into())
    })
    .await
    .map_err(|error| format!("Chat history worker failed: {error}"))?
}

pub(super) async fn delete_async(id: String, latest: Option<Conversation>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = storage_path()?;
        let mut store = HistoryStore::open(&path)?;
        if let Some(conversation) = latest {
            store.save_conversation(&conversation)?;
        }
        store.delete_conversation(&id)?;
        Ok(())
    })
    .await
    .map_err(|error| format!("Chat history worker failed: {error}"))?
}

pub(super) fn storage_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());

    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|home| home.join("Library").join("Application Support"));

    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local").join("share"))
        });

    base.map(|path| path.join("adham-ai").join("history.sqlite3"))
        .ok_or_else(|| "Could not determine the local application data directory.".into())
}

pub(super) fn sort_recent_first(conversations: &mut [Conversation]) {
    conversations.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
}

pub(super) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as u64
}

fn insert_conversation(
    transaction: &Transaction<'_>,
    conversation: &Conversation,
) -> Result<bool, String> {
    let inserted = transaction
        .execute(
            "INSERT INTO conversations (id, title, created_at, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO NOTHING",
            params![
                conversation.id,
                conversation.title,
                as_i64(conversation.created_at),
                as_i64(conversation.updated_at),
                as_i64(conversation.revision),
            ],
        )
        .map_err(|error| format!("Could not create conversation: {error}"))?;
    if inserted == 0 {
        return Ok(false);
    }
    Ok(true)
}

fn insert_messages(
    transaction: &Transaction<'_>,
    conversation: &Conversation,
) -> Result<(), String> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO messages (
                id, conversation_id, position, role, content, model_context,
                turn_id, request_id, model, status, status_detail, created_at,
                document_context, source_status, document_context_mode, deep_think,
                thinking_received
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        )
        .map_err(|error| format!("Could not prepare conversation messages: {error}"))?;
    for (position, message) in conversation.messages.iter().enumerate() {
        statement
            .execute(params![
                as_i64(message.id),
                conversation.id,
                position as i64,
                message.role.as_db(),
                message.content,
                message.model_context,
                message.turn_id.map(as_i64),
                message.request_id.map(as_i64),
                message.model,
                message.status.map(ResponseStatus::as_db),
                message.status_detail,
                as_i64(message.created_at),
                message.document_context,
                message.source_status,
                message.document_context_mode,
                message.deep_think,
                message.thinking_received,
            ])
            .map_err(|error| format!("Could not save conversation message: {error}"))?;
        for (source_position, source) in message.sources.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO message_sources (
                        assistant_message_id, position, document_id, filename, excerpt
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        as_i64(message.id),
                        source_position as i64,
                        source.document_id,
                        source.filename,
                        source.excerpt,
                    ],
                )
                .map_err(|error| format!("Could not save document source: {error}"))?;
        }
        for (context_position, document_id) in message.selected_document_ids.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO message_context_documents (message_id, document_id, position)
                     VALUES (?1, ?2, ?3)",
                    params![as_i64(message.id), document_id, context_position as i64],
                )
                .map_err(|error| format!("Could not save selected chat document: {error}"))?;
        }
    }
    Ok(())
}

impl Role {
    fn as_db(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    fn from_db(value: &str) -> Result<Self, rusqlite::Error> {
        match value {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            _ => Err(rusqlite::Error::InvalidColumnType(
                1,
                "role".into(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

impl ResponseStatus {
    fn as_db(self) -> &'static str {
        match self {
            Self::Generating => "generating",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Replaced => "replaced",
        }
    }

    fn from_db(value: &str) -> Result<Self, rusqlite::Error> {
        match value {
            "generating" => Ok(Self::Generating),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            "replaced" => Ok(Self::Replaced),
            _ => Err(rusqlite::Error::InvalidColumnType(
                7,
                "status".into(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

fn as_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn as_u64(value: i64) -> Result<u64, String> {
    u64::try_from(value).map_err(|error| format!("Invalid negative value in chat history: {error}"))
}

fn to_sql_error(error: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Integer,
        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Conversation, HistoryStore, MessageSource, ResponseStatus, Role, StoredMessage, now_millis,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDatabase(PathBuf);

    impl TestDatabase {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!("adham-ai-history-{unique}-{sequence}.sqlite3")))
        }

        fn open(&self) -> HistoryStore {
            HistoryStore::open(&self.0).unwrap()
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("sqlite3-wal"));
            let _ = std::fs::remove_file(self.0.with_extension("sqlite3-shm"));
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }

    fn conversation(id: &str, updated_at: u64) -> Conversation {
        let first_message_id = updated_at * 10;
        Conversation {
            id: id.into(),
            title: "A title".into(),
            created_at: 1,
            updated_at,
            revision: 1,
            messages: vec![
                StoredMessage {
                    id: first_message_id,
                    role: Role::User,
                    content: "## Mixed **Markdown** مرحبًا".into(),
                    model_context: true,
                    turn_id: Some(updated_at),
                    request_id: None,
                    model: Some("qwen3:4b".into()),
                    status: None,
                    status_detail: None,
                    created_at: 2,
                    document_context: false,
                    source_status: None,
                    sources: Vec::new(),
                    document_context_mode: "no_documents".into(),
                    selected_document_ids: Vec::new(),
                    deep_think: false,
                    thinking_received: false,
                },
                StoredMessage {
                    id: first_message_id + 1,
                    role: Role::Assistant,
                    content: "A response".into(),
                    model_context: true,
                    turn_id: Some(updated_at),
                    request_id: Some(updated_at),
                    model: Some("qwen3:4b".into()),
                    status: Some(ResponseStatus::Completed),
                    status_detail: None,
                    created_at: 3,
                    document_context: true,
                    source_status: None,
                    sources: vec![MessageSource {
                        document_id: "doc-1".into(),
                        filename: "guide.md".into(),
                        excerpt: "Relevant retrieved passage".into(),
                    }],
                    document_context_mode: "all_documents".into(),
                    selected_document_ids: vec!["doc-1".into()],
                    deep_think: false,
                    thinking_received: false,
                },
            ],
        }
    }

    #[test]
    fn schema_initialization_migrates_a_fresh_database_to_latest_version() {
        let database = TestDatabase::new();
        let store = database.open();
        let version: i64 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);

        let table_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('conversations', 'messages')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 2);
        let document_tables: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('documents', 'document_contents', 'document_search')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(document_tables, 3);
        let source_tables: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'message_sources'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_tables, 1);
        let context_tables: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'message_context_documents'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(context_tables, 1);
        assert!(store.list_conversations().unwrap().is_empty());
    }

    #[test]
    fn schema_migrates_version_five_without_deep_think_and_preserves_messages() {
        let database = TestDatabase::new();
        let expected = conversation("saved-before-deep-think", 12);
        {
            let mut store = database.open();
            store.create_conversation(&expected).unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN deep_think", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN thinking_received", [])
                .unwrap();
            store
                .connection
                .pragma_update(None, "user_version", 5)
                .unwrap();
            let deep_think_columns: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('messages')
                     WHERE name = 'deep_think'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(deep_think_columns, 0);
        }

        for _ in 0..2 {
            let store = database.open();
            let version: i64 = store
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 7);
            let deep_think_columns: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('messages')
                     WHERE name = 'deep_think'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(deep_think_columns, 1);
            let thinking_received_columns: i64 = store
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('messages')
                     WHERE name = 'thinking_received'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(thinking_received_columns, 1);
            assert_eq!(
                store.load_conversation("saved-before-deep-think").unwrap(),
                Some(expected.clone())
            );
            assert_eq!(store.list_conversations().unwrap().len(), 1);
        }
    }

    #[test]
    fn concurrent_connections_initialize_a_fresh_database_safely() {
        let database = TestDatabase::new();
        let path = database.0.clone();
        let first_path = path.clone();
        let first = std::thread::spawn(move || HistoryStore::open(&first_path).unwrap());
        let second = std::thread::spawn(move || HistoryStore::open(&path).unwrap());
        assert!(
            first
                .join()
                .unwrap()
                .list_conversations()
                .unwrap()
                .is_empty()
        );
        assert!(
            second
                .join()
                .unwrap()
                .list_conversations()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn schema_migrates_version_one_databases_to_latest_version() {
        let database = TestDatabase::new();
        {
            let store = database.open();
            store
                .connection
                .execute("DROP TABLE document_search", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE document_contents", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE documents", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE conversation_tombstones", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE message_sources", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE message_context_documents", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN deep_think", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN thinking_received", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN document_context_mode", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN source_status", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN document_context", [])
                .unwrap();
            store
                .connection
                .pragma_update(None, "user_version", 1)
                .unwrap();
        }

        let store = database.open();
        let version: i64 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        let tombstones_exist: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'conversation_tombstones'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tombstones_exist, 1);
    }

    #[test]
    fn schema_migrates_version_two_without_changing_existing_conversations() {
        let database = TestDatabase::new();
        let mut expected = conversation("saved-before-documents", 4);
        expected.messages[1].document_context = false;
        expected.messages[1].sources.clear();
        expected.messages[1].document_context_mode = "no_documents".into();
        expected.messages[1].selected_document_ids.clear();
        {
            let mut store = database.open();
            store.create_conversation(&expected).unwrap();
            store
                .connection
                .execute("DROP TABLE document_search", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE document_contents", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE documents", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE message_sources", [])
                .unwrap();
            store
                .connection
                .execute("DROP TABLE message_context_documents", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN deep_think", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN thinking_received", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN document_context_mode", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN source_status", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN document_context", [])
                .unwrap();
            store
                .connection
                .pragma_update(None, "user_version", 2)
                .unwrap();
        }

        let store = database.open();
        let version: i64 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        assert_eq!(
            store.load_conversation("saved-before-documents").unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn schema_migrates_version_four_and_preserves_existing_messages() {
        let database = TestDatabase::new();
        let mut expected = conversation("saved-before-context-snapshots", 8);
        expected.messages[1].document_context = false;
        expected.messages[1].sources.clear();
        expected.messages[1].document_context_mode = "no_documents".into();
        expected.messages[1].selected_document_ids.clear();
        {
            let mut store = database.open();
            store.create_conversation(&expected).unwrap();
            store
                .connection
                .execute("DROP TABLE message_context_documents", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN deep_think", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN thinking_received", [])
                .unwrap();
            store
                .connection
                .execute("ALTER TABLE messages DROP COLUMN document_context_mode", [])
                .unwrap();
            store
                .connection
                .pragma_update(None, "user_version", 4)
                .unwrap();
        }

        let store = database.open();
        let version: i64 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        assert_eq!(
            store
                .load_conversation("saved-before-context-snapshots")
                .unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn saving_and_loading_preserves_message_order_roles_and_markdown_source() {
        let database = TestDatabase::new();
        let mut store = database.open();
        let mut expected = conversation("one", 4);
        expected.messages[1].deep_think = true;
        expected.messages[1].thinking_received = true;
        store.create_conversation(&expected).unwrap();
        let loaded = store.load_conversation("one").unwrap().unwrap();

        assert_eq!(loaded, expected);
        assert_eq!(loaded.messages[0].role, Role::User);
        assert_eq!(loaded.messages[1].role, Role::Assistant);
        assert_eq!(loaded.messages[0].content, "## Mixed **Markdown** مرحبًا");
        assert!(loaded.messages[1].thinking_received);
        assert!(!loaded.messages[1].content.contains("private reasoning"));
    }

    #[test]
    fn cancelled_turn_context_and_partial_text_survive_storage_roundtrip() {
        let database = TestDatabase::new();
        let mut store = database.open();
        let mut cancelled = conversation("cancelled", 4);
        cancelled.messages[0].model_context = false;
        cancelled.messages[1].content = "partial response".into();
        cancelled.messages[1].model_context = false;
        cancelled.messages[1].status = Some(ResponseStatus::Cancelled);
        cancelled.messages[1].status_detail =
            Some("Generation stopped. Partial text was kept.".into());
        store.create_conversation(&cancelled).unwrap();

        let loaded = store.load_conversation("cancelled").unwrap().unwrap();
        assert_eq!(loaded.messages[0].content, cancelled.messages[0].content);
        assert!(!loaded.messages[0].model_context);
        assert_eq!(loaded.messages[1].content, "partial response");
        assert_eq!(loaded.messages[1].status, Some(ResponseStatus::Cancelled));
        assert!(!loaded.messages[1].model_context);
    }

    #[test]
    fn conversations_are_isolated_and_listed_by_recent_update() {
        let database = TestDatabase::new();
        let mut store = database.open();
        store
            .create_conversation(&conversation("older", 4))
            .unwrap();
        store
            .create_conversation(&conversation("newer", 8))
            .unwrap();

        let listed = store.list_conversations().unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older"]
        );
        assert_eq!(
            store.load_conversation("older").unwrap().unwrap().messages[0].content,
            "## Mixed **Markdown** مرحبًا"
        );
    }

    #[test]
    fn rename_updates_title_and_timestamp() {
        let database = TestDatabase::new();
        let mut store = database.open();
        store.create_conversation(&conversation("one", 4)).unwrap();

        let revision = store
            .rename_conversation("one", "Renamed", 9, 1)
            .unwrap()
            .unwrap();
        let loaded = store.load_conversation("one").unwrap().unwrap();
        assert_eq!(loaded.title, "Renamed");
        assert_eq!(loaded.updated_at, 9);
        assert_eq!(loaded.revision, revision);
        assert!(
            store
                .rename_conversation("missing", "Missing", 10, revision)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .rename_conversation("one", "  ", 10, revision)
                .is_err()
        );
    }

    #[test]
    fn partial_stream_and_final_response_survive_database_reopening() {
        let database = TestDatabase::new();
        let mut store = database.open();
        let mut streaming = conversation("stream", 4);
        streaming.messages[1].content = "Partial output".into();
        streaming.messages[1].model_context = false;
        streaming.messages[1].status = Some(ResponseStatus::Generating);
        store.create_conversation(&streaming).unwrap();

        let partial = database
            .open()
            .load_conversation("stream")
            .unwrap()
            .unwrap();
        assert_eq!(partial.messages[1].content, "Partial output");
        assert_eq!(partial.messages[1].status, Some(ResponseStatus::Generating));

        streaming.revision = partial.revision + 1;
        streaming.updated_at += 1;
        streaming.messages[1].content.push_str(" completed");
        streaming.messages[1].model_context = true;
        streaming.messages[1].status = Some(ResponseStatus::Completed);
        store.save_conversation(&streaming).unwrap();

        let reopened = database
            .open()
            .load_conversation("stream")
            .unwrap()
            .unwrap();
        assert_eq!(reopened.messages[1].content, "Partial output completed");
        assert_eq!(reopened.messages[1].status, Some(ResponseStatus::Completed));
    }

    #[test]
    fn stale_snapshot_created_before_rename_cannot_revert_the_title() {
        let database = TestDatabase::new();
        let mut store = database.open();
        let stale = conversation("one", 4);
        store.create_conversation(&stale).unwrap();
        let renamed_revision = store
            .rename_conversation("one", "Renamed", 9, stale.revision)
            .unwrap()
            .unwrap();

        store.save_conversation(&stale).unwrap();
        let loaded = store.load_conversation("one").unwrap().unwrap();
        assert_eq!(loaded.title, "Renamed");
        assert_eq!(loaded.revision, renamed_revision);
    }

    #[test]
    fn delete_removes_messages_through_foreign_key_cascade() {
        let database = TestDatabase::new();
        let mut store = database.open();
        store.create_conversation(&conversation("one", 4)).unwrap();

        assert!(store.delete_conversation("one").unwrap());
        assert!(store.load_conversation("one").unwrap().is_none());
        assert!(store.list_conversations().unwrap().is_empty());
        assert!(!store.delete_conversation("one").unwrap());
    }

    #[test]
    fn stale_save_cannot_resurrect_a_deleted_conversation() {
        let database = TestDatabase::new();
        let mut store = database.open();
        let pending_save = conversation("one", 4);
        store.create_conversation(&pending_save).unwrap();
        store.delete_conversation("one").unwrap();
        store.save_conversation(&pending_save).unwrap();

        assert!(store.load_conversation("one").unwrap().is_none());
        assert!(store.create_conversation(&pending_save).is_err());
    }

    #[test]
    fn each_connection_enables_foreign_key_enforcement() {
        let database = TestDatabase::new();
        let store = database.open();
        let second_connection = database.open();
        let enabled: i64 = store
            .connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(enabled, 1);
        let enabled_on_second: i64 = second_connection
            .connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(enabled_on_second, 1);
        let result = store.connection.execute(
            "INSERT INTO messages (
                id, conversation_id, position, role, content, model_context, created_at
             ) VALUES (1, 'missing', 0, 'user', 'orphan', 1, 0)",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn malformed_database_is_reported() {
        let database = TestDatabase::new();
        std::fs::write(&database.0, b"not a SQLite database").unwrap();
        assert!(HistoryStore::open(&database.0).is_err());
    }

    #[test]
    fn data_persists_after_reopening_the_database() {
        let database = TestDatabase::new();
        {
            let mut store = database.open();
            store.create_conversation(&conversation("one", 4)).unwrap();
        }
        let store = database.open();
        assert_eq!(
            store.load_conversation("one").unwrap().unwrap(),
            conversation("one", 4)
        );
    }

    #[test]
    fn older_async_snapshot_cannot_replace_newer_messages() {
        let database = TestDatabase::new();
        let mut store = database.open();
        let mut newer = conversation("one", 5);
        newer.revision = 2;
        newer.messages[0].content = "newer".into();
        store.create_conversation(&newer).unwrap();
        let mut older = conversation("one", 4);
        older.revision = 1;
        older.messages[0].content = "older".into();
        store.save_conversation(&older).unwrap();

        assert_eq!(
            store.load_conversation("one").unwrap().unwrap().messages[0].content,
            "newer"
        );
    }

    #[test]
    fn new_conversations_get_distinct_ids_and_timestamps() {
        let first = Conversation::new(now_millis());
        let second = Conversation::new(now_millis());
        assert_ne!(first.id, second.id);
        assert_eq!(first.title, "New Chat");
        assert_eq!(second.title, "New Chat");
    }

    #[test]
    fn renamed_title_survives_message_saves_and_database_reloads() {
        let database = TestDatabase::new();
        let mut store = database.open();
        let conversation = Conversation::new(4);
        let id = conversation.id.clone();
        store.create_conversation(&conversation).unwrap();
        let revision = store
            .rename_conversation(&id, "A custom title", 5, conversation.revision)
            .unwrap()
            .unwrap();

        let mut updated = store.load_conversation(&id).unwrap().unwrap();
        updated.messages.push(StoredMessage {
            id: 10,
            role: Role::User,
            content: "A very long first message that must never become the title".into(),
            model_context: true,
            turn_id: Some(1),
            request_id: None,
            model: Some("qwen3:4b".into()),
            status: None,
            status_detail: None,
            created_at: 6,
            document_context: false,
            source_status: None,
            sources: Vec::new(),
            document_context_mode: "no_documents".into(),
            selected_document_ids: Vec::new(),
            deep_think: false,
            thinking_received: false,
        });
        updated.revision = revision + 1;
        updated.updated_at = 6;
        store.save_conversation(&updated).unwrap();

        let reopened = database.open().load_conversation(&id).unwrap().unwrap();
        assert_eq!(reopened.title, "A custom title");
        assert_eq!(
            reopened.messages[0].content,
            "A very long first message that must never become the title"
        );
    }
}
