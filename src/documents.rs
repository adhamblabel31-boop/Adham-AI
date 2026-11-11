use crate::history::{self, HistoryStore, MessageSource};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) const MAX_DOCUMENT_BYTES: u64 = 10 * 1024 * 1024;
const MAX_EXTRACTED_TEXT_BYTES: usize = 20 * 1024 * 1024;
const HIGHLIGHT_START: &str = "\u{e000}";
const HIGHLIGHT_END: &str = "\u{e001}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DocumentMetadata {
    pub(super) id: String,
    pub(super) original_filename: String,
    pub(super) source_path: String,
    pub(super) file_type: String,
    pub(super) file_size: u64,
    pub(super) imported_at: u64,
    pub(super) fingerprint: String,
    pub(super) indexing_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredDocument {
    pub(super) metadata: DocumentMetadata,
    pub(super) content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchResult {
    pub(super) document_id: String,
    pub(super) filename: String,
    pub(super) file_type: String,
    pub(super) excerpt: String,
    pub(super) matched_fragments: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ImportOutcome {
    pub(super) documents: Vec<DocumentMetadata>,
    pub(super) removed_ids: Vec<String>,
    pub(super) imported: usize,
    pub(super) duplicates: usize,
    pub(super) errors: Vec<String>,
}

#[derive(Debug)]
struct ExtractedDocument {
    filename: String,
    source_path: String,
    file_type: String,
    file_size: u64,
    fingerprint: String,
    content: String,
}

pub(super) async fn load_documents_async() -> Result<Vec<DocumentMetadata>, String> {
    tokio::task::spawn_blocking(|| {
        let store = HistoryStore::open(&history::storage_path()?)?;
        list_documents(&store)
    })
    .await
    .map_err(|error| format!("Document storage worker failed: {error}"))?
}

pub(super) async fn load_document_async(id: String) -> Result<Option<StoredDocument>, String> {
    tokio::task::spawn_blocking(move || {
        let store = HistoryStore::open(&history::storage_path()?)?;
        load_document(&store, &id)
    })
    .await
    .map_err(|error| format!("Document storage worker failed: {error}"))?
}

pub(super) async fn import_paths_async(paths: Vec<PathBuf>) -> Result<ImportOutcome, String> {
    tokio::task::spawn_blocking(move || {
        let mut store = HistoryStore::open(&history::storage_path()?)?;
        let mut outcome = ImportOutcome::default();
        for path in paths {
            match extract_document(&path) {
                Ok(document) => match save_document(&mut store, document) {
                    Ok((metadata, imported, removed_id)) => {
                        outcome.documents.push(metadata);
                        if let Some(id) = removed_id {
                            outcome.removed_ids.push(id);
                        }
                        if imported {
                            outcome.imported += 1;
                        } else {
                            outcome.duplicates += 1;
                        }
                    }
                    Err(error) => outcome.errors.push(format!(
                        "{}: {error}",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Document")
                    )),
                },
                Err(error) => outcome.errors.push(format!(
                    "{}: {error}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Document")
                )),
            }
        }
        Ok(outcome)
    })
    .await
    .map_err(|error| format!("Document import worker failed: {error}"))?
}

pub(super) async fn search_async(query: String) -> Result<Vec<SearchResult>, String> {
    tokio::task::spawn_blocking(move || {
        let store = HistoryStore::open(&history::storage_path()?)?;
        search_documents(&store, &query)
    })
    .await
    .map_err(|error| format!("Document search worker failed: {error}"))?
}

pub(super) async fn retrieve_async(
    query: String,
    document_ids: Option<Vec<String>>,
    limit: usize,
) -> Result<Vec<MessageSource>, String> {
    tokio::task::spawn_blocking(move || {
        let store = HistoryStore::open(&history::storage_path()?)?;
        if document_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(Vec::new());
        }
        ensure_documents_have_text(&store, document_ids.as_deref())?;
        let results =
            search_documents_filtered(&store, &query, document_ids.as_deref(), limit, true)?;
        Ok(results
            .into_iter()
            .map(|result| MessageSource {
                document_id: result.document_id,
                filename: result.filename,
                excerpt: result.excerpt,
            })
            .collect())
    })
    .await
    .map_err(|error| format!("Document retrieval worker failed: {error}"))?
}

pub(super) async fn delete_async(id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let mut store = HistoryStore::open(&history::storage_path()?)?;
        delete_document(&mut store, &id).map(|_| ())
    })
    .await
    .map_err(|error| format!("Document deletion worker failed: {error}"))?
}

fn extract_document(path: &Path) -> Result<ExtractedDocument, String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The filename is not valid Unicode.".to_owned())?
        .to_owned();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .ok_or_else(|| "Unsupported file type. Choose a .txt, .md, or .pdf file.".to_owned())?
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "txt" | "md" | "pdf") {
        return Err("Unsupported file type. Choose a .txt, .md, or .pdf file.".into());
    }
    let canonical_path = std::fs::canonicalize(path)
        .map_err(|error| format!("Could not access the selected file: {error}"))?;
    let file = std::fs::File::open(&canonical_path)
        .map_err(|error| format!("Could not open the selected file: {error}"))?;
    let file_size = file
        .metadata()
        .map_err(|error| format!("Could not read file information: {error}"))?
        .len();
    if file_size > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "File exceeds the {} MiB import limit.",
            MAX_DOCUMENT_BYTES / (1024 * 1024)
        ));
    }
    let mut bytes = Vec::with_capacity(file_size as usize);
    file.take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the selected file: {error}"))?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "File exceeds the {} MiB import limit.",
            MAX_DOCUMENT_BYTES / (1024 * 1024)
        ));
    }

    let content = match extension.as_str() {
        "txt" | "md" => std::str::from_utf8(&bytes)
            .map_err(|_| "Text file is not valid UTF-8.".to_owned())?
            .to_owned(),
        "pdf" => pdf_extract::extract_text_from_mem(&bytes).map_err(|error| {
            format!(
                "Could not extract selectable PDF text: {error}. OCR for scanned PDFs is not supported."
            )
        })?,
        _ => unreachable!("extensions are validated above"),
    };
    if content.len() > MAX_EXTRACTED_TEXT_BYTES {
        return Err("Extracted text exceeds the 20 MiB storage limit.".into());
    }
    if content.trim().is_empty() {
        return Err(if extension == "pdf" {
            "No selectable text was found. OCR for scanned PDFs is not supported in this phase."
                .into()
        } else {
            "The file contains no readable text.".into()
        });
    }
    let fingerprint = format!("{:x}", Sha256::digest(content.as_bytes()));
    Ok(ExtractedDocument {
        filename,
        source_path: canonical_path.to_string_lossy().into_owned(),
        file_type: extension,
        file_size: bytes.len() as u64,
        fingerprint,
        content,
    })
}

use std::io::Read;

fn save_document(
    store: &mut HistoryStore,
    document: ExtractedDocument,
) -> Result<(DocumentMetadata, bool, Option<String>), String> {
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("Could not begin document import: {error}"))?;

    let by_fingerprint = transaction
        .query_row(
            "SELECT id FROM documents WHERE fingerprint = ?1",
            [&document.fingerprint],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not check duplicate documents: {error}"))?;
    let by_path = transaction
        .query_row(
            "SELECT id FROM documents WHERE source_path = ?1",
            [&document.source_path],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Could not check previously imported files: {error}"))?;

    if let Some(id) = by_fingerprint
        && by_path.as_deref() != Some(id.as_str())
    {
        let metadata = query_metadata(&transaction, &id)?
            .ok_or_else(|| "Duplicate document disappeared during import.".to_owned())?;
        let mut removed_id = None;
        if let Some(previous_id) = by_path {
            transaction
                .execute(
                    "DELETE FROM document_search WHERE document_id = ?1",
                    [&previous_id],
                )
                .map_err(|error| format!("Could not update duplicate document index: {error}"))?;
            transaction
                .execute("DELETE FROM documents WHERE id = ?1", [&previous_id])
                .map_err(|error| format!("Could not replace changed document: {error}"))?;
            removed_id = Some(previous_id);
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not finish duplicate check: {error}"))?;
        return Ok((metadata, false, removed_id));
    }

    let imported_at = history::now_millis();
    let id = by_path.unwrap_or_else(new_document_id);
    let metadata = DocumentMetadata {
        id: id.clone(),
        original_filename: document.filename,
        source_path: document.source_path,
        file_type: document.file_type,
        file_size: document.file_size,
        imported_at,
        fingerprint: document.fingerprint,
        indexing_status: "indexed".into(),
    };
    transaction
        .execute(
            "INSERT INTO documents (
                id, original_filename, source_path, file_type, file_size,
                imported_at, fingerprint, indexing_status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'indexed')
             ON CONFLICT(id) DO UPDATE SET
                original_filename = excluded.original_filename,
                source_path = excluded.source_path,
                file_type = excluded.file_type,
                file_size = excluded.file_size,
                imported_at = excluded.imported_at,
                fingerprint = excluded.fingerprint,
                indexing_status = 'indexed'",
            params![
                metadata.id,
                metadata.original_filename,
                metadata.source_path,
                metadata.file_type,
                to_i64(metadata.file_size),
                to_i64(metadata.imported_at),
                metadata.fingerprint,
            ],
        )
        .map_err(|error| format!("Could not save document information: {error}"))?;
    transaction
        .execute(
            "INSERT INTO document_contents (document_id, extracted_text)
             VALUES (?1, ?2)
             ON CONFLICT(document_id) DO UPDATE SET extracted_text = excluded.extracted_text",
            params![id, document.content],
        )
        .map_err(|error| format!("Could not save extracted document text: {error}"))?;
    transaction
        .execute("DELETE FROM document_search WHERE document_id = ?1", [&id])
        .map_err(|error| format!("Could not update local document search: {error}"))?;
    transaction
        .execute(
            "INSERT INTO document_search (document_id, filename, content)
             VALUES (?1, ?2, ?3)",
            params![id, metadata.original_filename, document.content],
        )
        .map_err(|error| format!("Could not index local document text: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not finish document import: {error}"))?;
    Ok((metadata, true, None))
}

fn list_documents(store: &HistoryStore) -> Result<Vec<DocumentMetadata>, String> {
    let mut statement = store
        .connection
        .prepare(
            "SELECT id, original_filename, source_path, file_type, file_size,
                    imported_at, fingerprint, indexing_status
             FROM documents ORDER BY imported_at DESC, id ASC",
        )
        .map_err(|error| format!("Could not prepare document list: {error}"))?;
    let rows = statement
        .query_map([], read_metadata)
        .map_err(|error| format!("Could not load documents: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("Could not read document information: {error}")))
        .collect()
}

fn load_document(store: &HistoryStore, id: &str) -> Result<Option<StoredDocument>, String> {
    let metadata = store
        .connection
        .query_row(
            "SELECT id, original_filename, source_path, file_type, file_size,
                    imported_at, fingerprint, indexing_status
             FROM documents WHERE id = ?1",
            [id],
            read_metadata,
        )
        .optional()
        .map_err(|error| format!("Could not load document information: {error}"))?;
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    let content = store
        .connection
        .query_row(
            "SELECT extracted_text FROM document_contents WHERE document_id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not load stored document text: {error}"))?;
    Ok(Some(StoredDocument { metadata, content }))
}

fn search_documents(store: &HistoryStore, query: &str) -> Result<Vec<SearchResult>, String> {
    search_documents_filtered(store, query, None, 50, false)
}

fn search_documents_filtered(
    store: &HistoryStore,
    query: &str,
    document_ids: Option<&[String]>,
    limit: usize,
    content_only: bool,
) -> Result<Vec<SearchResult>, String> {
    let terms = query
        .split_whitespace()
        .take(12)
        .map(|term| term.chars().take(64).collect::<String>())
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(|term| {
            let prefix = format!("\"{}\"*", term.replace('"', "\"\""));
            if content_only {
                format!("content : {prefix}")
            } else {
                prefix
            }
        })
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    if document_ids.is_some_and(<[String]>::is_empty) {
        return Ok(Vec::new());
    }
    let match_query = terms.join(" AND ");
    let mut sql = String::from(
        "SELECT d.id, d.original_filename, d.file_type,
                snippet(document_search, 2, ?2, ?3, '…', 24)
         FROM document_search
         JOIN documents d ON d.id = document_search.document_id
         JOIN document_contents c ON c.document_id = d.id
         WHERE document_search MATCH ?1",
    );
    if let Some(ids) = document_ids {
        sql.push_str(" AND document_search.document_id IN (");
        sql.push_str(
            &(0..ids.len())
                .map(|index| format!("?{}", index + 4))
                .collect::<Vec<_>>()
                .join(", "),
        );
        sql.push(')');
    }
    let limit_parameter = 4 + document_ids.map_or(0, <[String]>::len);
    sql.push_str(&format!(
        " ORDER BY rank, d.original_filename COLLATE NOCASE, d.id LIMIT ?{limit_parameter}"
    ));
    let mut statement = store
        .connection
        .prepare(&sql)
        .map_err(|error| format!("Could not prepare local document search: {error}"))?;
    let mut parameters = vec![
        rusqlite::types::Value::Text(match_query),
        rusqlite::types::Value::Text(HIGHLIGHT_START.into()),
        rusqlite::types::Value::Text(HIGHLIGHT_END.into()),
    ];
    if let Some(ids) = document_ids {
        parameters.extend(ids.iter().cloned().map(rusqlite::types::Value::Text));
    }
    parameters.push(rusqlite::types::Value::Integer(
        limit.min(i64::MAX as usize) as i64,
    ));
    let rows = statement
        .query_map(rusqlite::params_from_iter(parameters), |row| {
            let snippet: String = row.get(3)?;
            let (excerpt, matched_fragments) = split_highlighted_excerpt(&snippet);
            Ok(SearchResult {
                document_id: row.get(0)?,
                filename: row.get(1)?,
                file_type: row.get(2)?,
                excerpt,
                matched_fragments,
            })
        })
        .map_err(|error| format!("Could not search local documents: {error}"))?;
    rows.map(|row| row.map_err(|error| format!("Could not read search result: {error}")))
        .collect()
}

fn ensure_documents_have_text(
    store: &HistoryStore,
    document_ids: Option<&[String]>,
) -> Result<(), String> {
    let mut sql = String::from(
        "SELECT d.original_filename
         FROM documents d
         LEFT JOIN document_contents c ON c.document_id = d.id
         WHERE (c.document_id IS NULL OR length(trim(c.extracted_text)) = 0)",
    );
    if let Some(ids) = document_ids {
        sql.push_str(" AND d.id IN (");
        sql.push_str(
            &(0..ids.len())
                .map(|index| format!("?{}", index + 1))
                .collect::<Vec<_>>()
                .join(", "),
        );
        sql.push(')');
    }
    sql.push_str(" LIMIT 1");
    let parameters = document_ids
        .unwrap_or_default()
        .iter()
        .cloned()
        .map(rusqlite::types::Value::Text)
        .collect::<Vec<_>>();
    let unavailable = store
        .connection
        .query_row(&sql, rusqlite::params_from_iter(parameters), |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|error| format!("Could not verify stored document text: {error}"))?;
    if let Some(filename) = unavailable {
        return Err(format!(
            "Stored text for “{filename}” is unavailable or empty. Reimport the document before using it as chat context."
        ));
    }
    Ok(())
}

fn split_highlighted_excerpt(snippet: &str) -> (String, Vec<String>) {
    let mut excerpt = String::with_capacity(snippet.len());
    let mut matched_fragments = Vec::new();
    let mut remaining = snippet;

    loop {
        let Some(start) = remaining.find(HIGHLIGHT_START) else {
            excerpt.push_str(remaining);
            break;
        };
        excerpt.push_str(&remaining[..start]);
        remaining = &remaining[start + HIGHLIGHT_START.len()..];

        let Some(end) = remaining.find(HIGHLIGHT_END) else {
            excerpt.push_str(HIGHLIGHT_START);
            excerpt.push_str(remaining);
            break;
        };
        let fragment = &remaining[..end];
        excerpt.push_str(fragment);
        if !fragment.is_empty() && !matched_fragments.iter().any(|item| item == fragment) {
            matched_fragments.push(fragment.to_owned());
        }
        remaining = &remaining[end + HIGHLIGHT_END.len()..];
    }

    (excerpt, matched_fragments)
}

fn delete_document(store: &mut HistoryStore, id: &str) -> Result<bool, String> {
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("Could not begin document removal: {error}"))?;
    transaction
        .execute("DELETE FROM document_search WHERE document_id = ?1", [id])
        .map_err(|error| format!("Could not remove document search entry: {error}"))?;
    let deleted = transaction
        .execute("DELETE FROM documents WHERE id = ?1", [id])
        .map_err(|error| format!("Could not remove document: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not finish document removal: {error}"))?;
    Ok(deleted == 1)
}

fn query_metadata(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Option<DocumentMetadata>, String> {
    connection
        .query_row(
            "SELECT id, original_filename, source_path, file_type, file_size,
                    imported_at, fingerprint, indexing_status
             FROM documents WHERE id = ?1",
            [id],
            read_metadata,
        )
        .optional()
        .map_err(|error| format!("Could not load duplicate document details: {error}"))
}

fn read_metadata(row: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentMetadata> {
    Ok(DocumentMetadata {
        id: row.get(0)?,
        original_filename: row.get(1)?,
        source_path: row.get(2)?,
        file_type: row.get(3)?,
        file_size: to_u64(row.get(4)?)?,
        imported_at: to_u64(row.get(5)?)?,
        fingerprint: row.get(6)?,
        indexing_status: row.get(7)?,
    })
}

fn to_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn new_document_id() -> String {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{time}-{sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempStore {
        root: PathBuf,
        database: PathBuf,
    }

    impl TempStore {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("adham-ai-documents-{}-{id}", history::now_millis()));
            std::fs::create_dir_all(&root).unwrap();
            Self {
                database: root.join("test.sqlite3"),
                root,
            }
        }

        fn open(&self) -> HistoryStore {
            HistoryStore::open(&self.database).unwrap()
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn text_extraction_preserves_unicode_and_rejects_invalid_files() {
        let temp = TempStore::new();
        let txt = temp.root.join("arabic.txt");
        std::fs::write(&txt, "Hello مرحبًا\n").unwrap();
        let extracted = extract_document(&txt).unwrap();
        assert_eq!(extracted.content, "Hello مرحبًا\n");
        assert_eq!(extracted.file_type, "txt");

        let invalid = temp.root.join("bad.bin");
        std::fs::write(&invalid, "not supported").unwrap();
        assert!(
            extract_document(&invalid)
                .unwrap_err()
                .contains("Unsupported")
        );

        let empty_pdf = temp.root.join("scan.pdf");
        std::fs::write(&empty_pdf, b"%PDF-1.4\n%%EOF").unwrap();
        assert!(extract_document(&empty_pdf).unwrap_err().contains("OCR"));

        let empty_text = temp.root.join("empty.md");
        std::fs::write(&empty_text, " \n ").unwrap();
        assert!(
            extract_document(&empty_text)
                .unwrap_err()
                .contains("no readable text")
        );
    }

    #[test]
    fn import_deduplicates_by_extracted_text_and_updates_changed_source_files() {
        let temp = TempStore::new();
        let first = temp.root.join("first.md");
        let copy = temp.root.join("copy.txt");
        std::fs::write(&first, "# Shared\nHello world").unwrap();
        std::fs::write(&copy, "# Shared\nHello world").unwrap();

        let mut store = temp.open();
        let (created, inserted, _) =
            save_document(&mut store, extract_document(&first).unwrap()).unwrap();
        assert!(inserted);
        let (duplicate, inserted, removed_id) =
            save_document(&mut store, extract_document(&copy).unwrap()).unwrap();
        assert!(!inserted);
        assert!(removed_id.is_none());
        assert_eq!(duplicate.id, created.id);
        assert_eq!(list_documents(&store).unwrap().len(), 1);

        std::fs::write(&first, "# Updated\nمرحبا").unwrap();
        let (updated, inserted, _) =
            save_document(&mut store, extract_document(&first).unwrap()).unwrap();
        assert!(inserted);
        assert_eq!(updated.id, created.id);
        assert_eq!(
            load_document(&store, &created.id).unwrap().unwrap().content,
            "# Updated\nمرحبا"
        );
        assert!(search_documents(&store, "Shared").unwrap().is_empty());
        assert_eq!(search_documents(&store, "مرحبا").unwrap().len(), 1);
    }

    #[test]
    fn local_fts_search_returns_excerpt_and_handles_empty_or_operator_queries() {
        let temp = TempStore::new();
        let source = temp.root.join("notes.md");
        std::fs::write(&source, "The local search finds مرحبًا in this note.").unwrap();
        let mut store = temp.open();
        let (metadata, _, _) =
            save_document(&mut store, extract_document(&source).unwrap()).unwrap();

        let results = search_documents(&store, "مرحبًا").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].document_id, metadata.id);
        assert!(results[0].excerpt.contains("مرحبًا"));
        assert!(search_documents(&store, "  ").unwrap().is_empty());
        assert!(search_documents(&store, "\" OR )").unwrap().is_empty());
        assert!(search_documents(&store, "… ! ?").unwrap().is_empty());
    }

    #[test]
    fn prefix_search_finds_english_word_and_returns_its_matching_fragment() {
        let temp = TempStore::new();
        let source = temp.root.join("programming.md");
        std::fs::write(&source, "Notes about programming languages.").unwrap();
        let mut store = temp.open();
        save_document(&mut store, extract_document(&source).unwrap()).unwrap();

        let results = search_documents(&store, "prog").unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].excerpt.contains("programming"));
        assert!(results[0].matched_fragments.contains(&"programming".into()));
    }

    #[test]
    fn arabic_prefix_search_preserves_matching_unicode_text() {
        let temp = TempStore::new();
        let source = temp.root.join("arabic.txt");
        std::fs::write(&source, "أتعلم البرمجة باللغة العربية.").unwrap();
        let mut store = temp.open();
        save_document(&mut store, extract_document(&source).unwrap()).unwrap();

        let results = search_documents(&store, "البرم").unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].excerpt.contains("البرمجة"));
        assert!(results[0].matched_fragments.contains(&"البرمجة".into()));
    }

    #[test]
    fn multiple_search_terms_require_each_term_and_return_matching_excerpts() {
        let temp = TempStore::new();
        let matching = temp.root.join("matching.txt");
        let partial = temp.root.join("partial.txt");
        std::fs::write(&matching, "Programming with Rust is enjoyable.").unwrap();
        std::fs::write(&partial, "Programming without the requested language.").unwrap();
        let mut store = temp.open();
        save_document(&mut store, extract_document(&matching).unwrap()).unwrap();
        save_document(&mut store, extract_document(&partial).unwrap()).unwrap();

        let results = search_documents(&store, "prog Rust").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].filename, "matching.txt");
        assert!(results[0].excerpt.contains("Programming"));
        assert!(results[0].excerpt.contains("Rust"));
        assert!(results[0].matched_fragments.len() >= 2);
    }

    #[test]
    fn retrieval_limits_results_to_selected_documents_and_handles_no_matches() {
        let temp = TempStore::new();
        let first = temp.root.join("first.txt");
        let second = temp.root.join("second.txt");
        std::fs::write(&first, "Rust programming has ownership rules.").unwrap();
        std::fs::write(&second, "Rust programming supports safe systems code.").unwrap();
        let mut store = temp.open();
        let (first_metadata, _, _) =
            save_document(&mut store, extract_document(&first).unwrap()).unwrap();
        let (second_metadata, _, _) =
            save_document(&mut store, extract_document(&second).unwrap()).unwrap();

        let selected = search_documents_filtered(
            &store,
            "prog Rust",
            Some(std::slice::from_ref(&second_metadata.id)),
            4,
            true,
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].document_id, second_metadata.id);
        assert!(selected[0].excerpt.contains("programming"));
        assert!(
            search_documents_filtered(
                &store,
                "no-such-passage",
                Some(std::slice::from_ref(&first_metadata.id)),
                4,
                true,
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            search_documents_filtered(&store, "programming", Some(&[]), 4, true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn retrieval_reports_documents_with_missing_stored_text() {
        let temp = TempStore::new();
        let source = temp.root.join("missing.txt");
        std::fs::write(&source, "local retrieval text").unwrap();
        let mut store = temp.open();
        let (metadata, _, _) =
            save_document(&mut store, extract_document(&source).unwrap()).unwrap();
        store
            .connection
            .execute(
                "DELETE FROM document_contents WHERE document_id = ?1",
                [&metadata.id],
            )
            .unwrap();

        let error = ensure_documents_have_text(&store, Some(std::slice::from_ref(&metadata.id)))
            .unwrap_err();
        assert!(error.contains("missing.txt"));
        assert!(error.contains("Reimport"));
    }

    #[test]
    fn unicode_excerpt_and_highlight_fragments_remain_valid_and_clean() {
        let snippet = format!("… مرحبًا {}بالبرمجة{} هنا", HIGHLIGHT_START, HIGHLIGHT_END);
        let (excerpt, matches) = split_highlighted_excerpt(&snippet);

        assert_eq!(excerpt, "… مرحبًا بالبرمجة هنا");
        assert_eq!(matches, ["بالبرمجة"]);
        assert!(excerpt.is_char_boundary(excerpt.len()));
    }

    #[test]
    fn deleting_document_removes_metadata_text_and_search_index() {
        let temp = TempStore::new();
        let source = temp.root.join("remove.txt");
        std::fs::write(&source, "unique searchable phrase").unwrap();
        let mut store = temp.open();
        let (metadata, _, _) =
            save_document(&mut store, extract_document(&source).unwrap()).unwrap();

        assert!(delete_document(&mut store, &metadata.id).unwrap());
        assert!(load_document(&store, &metadata.id).unwrap().is_none());
        assert!(search_documents(&store, "unique").unwrap().is_empty());
        assert!(!delete_document(&mut store, &metadata.id).unwrap());
    }

    #[test]
    fn document_and_extracted_text_persist_after_database_reopen() {
        let temp = TempStore::new();
        let source = temp.root.join("persistent.txt");
        std::fs::write(&source, "persisted text").unwrap();
        let metadata = {
            let mut store = temp.open();
            save_document(&mut store, extract_document(&source).unwrap())
                .unwrap()
                .0
        };
        std::fs::remove_file(source).unwrap();
        let reopened = temp.open();
        let loaded = load_document(&reopened, &metadata.id).unwrap().unwrap();
        assert_eq!(loaded.metadata, metadata);
        assert_eq!(loaded.content, "persisted text");
        assert_eq!(search_documents(&reopened, "persisted").unwrap().len(), 1);
    }

    #[test]
    fn import_rejects_files_over_the_size_limit() {
        let temp = TempStore::new();
        let source = temp.root.join("large.txt");
        let file = std::fs::File::create(&source).unwrap();
        file.set_len(MAX_DOCUMENT_BYTES + 1).unwrap();
        assert!(extract_document(&source).unwrap_err().contains("10 MiB"));
    }

    #[test]
    fn changed_source_that_matches_another_document_removes_its_old_index_entry() {
        let temp = TempStore::new();
        let original = temp.root.join("original.txt");
        let existing = temp.root.join("existing.txt");
        std::fs::write(&original, "old text").unwrap();
        std::fs::write(&existing, "new shared text").unwrap();

        let mut store = temp.open();
        let (old, _, _) = save_document(&mut store, extract_document(&original).unwrap()).unwrap();
        let (canonical, _, _) =
            save_document(&mut store, extract_document(&existing).unwrap()).unwrap();
        std::fs::write(&original, "new shared text").unwrap();

        let (duplicate, imported, removed_id) =
            save_document(&mut store, extract_document(&original).unwrap()).unwrap();
        assert!(!imported);
        assert_eq!(duplicate.id, canonical.id);
        assert_eq!(removed_id.as_deref(), Some(old.id.as_str()));
        assert!(load_document(&store, &old.id).unwrap().is_none());
        assert_eq!(list_documents(&store).unwrap().len(), 1);
        assert_eq!(search_documents(&store, "old").unwrap().len(), 0);
        assert_eq!(search_documents(&store, "shared").unwrap().len(), 1);
    }

    #[test]
    fn selectable_pdf_text_is_extracted_for_local_storage() {
        let temp = TempStore::new();
        let source = temp.root.join("selectable.pdf");
        std::fs::write(&source, minimal_pdf("Hello PDF text")).unwrap();

        let extracted = extract_document(&source).unwrap();
        assert_eq!(extracted.file_type, "pdf");
        assert!(extracted.content.contains("Hello PDF text"));
    }

    fn minimal_pdf(text: &str) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        let stream = format!("BT /F1 12 Tf 20 100 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                stream.len(),
                stream
            ),
        ];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
        }

        let xref_offset = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                xref_offset
            )
            .as_bytes(),
        );
        pdf
    }
}
