use bytes::Bytes;
use iced::widget::text_editor;
use iced::{Element, Subscription, Task, Theme, task};
use std::collections::HashMap;
use std::time::Duration;

use crate::{
    components,
    documents::{self, DocumentMetadata, ImportOutcome, SearchResult},
    history::{self, Conversation, MessageSource},
    media::{BackendStatus, MediaImage},
    media_process::MediaProcessManager,
    ollama::{ChatEvent, ChatMessage as OllamaMessage, OllamaClient, OllamaError},
    pages,
    theme::ThemeMode,
    update,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Page {
    Chat,
    Documents,
    MediaLab,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DocumentContextMode {
    #[default]
    NoDocuments,
    AllDocuments,
    SelectedDocuments,
}

impl DocumentContextMode {
    pub(super) const ALL: [Self; 3] = [
        Self::NoDocuments,
        Self::AllDocuments,
        Self::SelectedDocuments,
    ];

    pub(super) fn as_db(self) -> &'static str {
        match self {
            Self::NoDocuments => "no_documents",
            Self::AllDocuments => "all_documents",
            Self::SelectedDocuments => "selected_documents",
        }
    }

    pub(super) fn from_db(value: &str) -> Self {
        match value {
            "all_documents" => Self::AllDocuments,
            "selected_documents" => Self::SelectedDocuments,
            _ => Self::NoDocuments,
        }
    }
}

impl std::fmt::Display for DocumentContextMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoDocuments => "No Documents",
            Self::AllDocuments => "All Documents",
            Self::SelectedDocuments => "Selected Documents",
        })
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ChatDocumentContext {
    pub(super) mode: DocumentContextMode,
    pub(super) selected_document_ids: Vec<String>,
}

impl Page {
    pub(super) const ALL: [(Self, &'static str); 4] = [
        (Self::Chat, "Chat"),
        (Self::Documents, "Documents"),
        (Self::MediaLab, "Media Lab"),
        (Self::Settings, "Settings"),
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Documents => "Documents",
            Self::MediaLab => "Media Lab",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum Message {
    Navigate(Page),
    ToggleSidebar,
    ToggleComposerTools,
    ComposerEdited(text_editor::Action),
    Send,
    StartNewConversation,
    OpenConversation(String),
    RequestDeleteConversation(String),
    ConfirmDeleteConversation,
    CancelDeleteConversation,
    BeginRenameConversation(String),
    RenameTitleEdited(String),
    CommitRenameConversation,
    CancelRenameConversation,
    ConversationRenamed {
        id: String,
        title: String,
        result: Result<u64, String>,
    },
    ConversationDeleted {
        id: String,
        result: Result<(), String>,
    },
    CancelChat,
    RetryPrompt {
        turn_id: u64,
    },
    ChatScrolled {
        at_bottom: bool,
    },
    ThinkingTick,
    JumpToLatest,
    SelectModel(String),
    SetDeepThink(bool),
    DeepThinkCapabilityLoaded {
        request_id: u64,
        model: String,
        result: Result<bool, OllamaError>,
    },
    RefreshModels,
    SelectTheme(ThemeMode),
    CheckForUpdates,
    UpdatesChecked {
        request_id: u64,
        result: Result<crate::software_update::Release, String>,
    },
    DownloadUpdate,
    CancelUpdateDownload,
    UpdateDownloadProgress {
        request_id: u64,
        progress: crate::software_update::DownloadProgress,
    },
    UpdateDownloadFinished {
        request_id: u64,
        result: Result<crate::software_update::VerifiedPackage, String>,
    },
    RequestUpdateInstallation,
    ConfirmUpdateInstallation,
    CancelUpdateInstallation,
    UpdateInstallationFinished(Result<(), String>),
    RestartUpdatedApplication,
    OpenReleasesPage,
    ModelsLoaded {
        request_id: u64,
        result: Result<Vec<String>, OllamaError>,
    },
    ChatProgress {
        request_id: u64,
        assistant_message_id: u64,
        conversation_id: String,
        event: ChatEvent,
    },
    HistoryLoaded(Result<Vec<Conversation>, String>),
    ImportDocuments,
    DocumentFilesChosen {
        request_id: u64,
        paths: Result<Option<Vec<std::path::PathBuf>>, String>,
    },
    DocumentsImported {
        request_id: u64,
        result: Result<ImportOutcome, String>,
    },
    DocumentsLoaded {
        request_id: u64,
        result: Result<Vec<DocumentMetadata>, String>,
    },
    SelectDocument(String),
    DocumentContentLoaded {
        request_id: u64,
        id: String,
        result: Result<Option<documents::StoredDocument>, String>,
    },
    DocumentSearchEdited(String),
    DocumentSearchLoaded {
        request_id: u64,
        query: String,
        result: Result<Vec<SearchResult>, String>,
    },
    SelectDocumentContextMode(DocumentContextMode),
    ToggleDocumentChatContext(String),
    OpenSourceDocument(String),
    DocumentContextLoaded {
        request_id: u64,
        assistant_message_id: u64,
        conversation_id: String,
        result: Result<Vec<MessageSource>, String>,
    },
    RequestRemoveDocument(String),
    CancelRemoveDocument,
    ConfirmRemoveDocument,
    DocumentRemoved {
        id: String,
        result: Result<(), String>,
    },
    HistorySaved {
        revision: u64,
        result: Result<(), String>,
    },
    MarkdownLinkClicked(String),
    MediaPromptChanged(String),
    MediaStepsChanged(String),
    RefreshMediaBackend,
    MediaBackendLoaded {
        request_id: u64,
        status: BackendStatus,
    },
    MediaLifecycleTick,
    MediaHealthTick,
    MediaHealthChecked(BackendStatus),
    GenerateMedia,
    CancelMediaGeneration,
    MediaCancelCompleted {
        request_id: u64,
        result: Result<bool, String>,
    },
    MediaGenerated {
        request_id: u64,
        result: Result<Vec<u8>, String>,
    },
    MediaAutoSaveCompleted {
        generation_request_id: u64,
        save_request_id: u64,
        result: Result<std::path::PathBuf, String>,
    },
    RetryMediaAutoSave,
    OpenMediaOutputFolder,
    MediaOutputFolderOpened(Result<(), String>),
    ExportMediaImage,
    MediaExportPathChosen {
        request_id: u64,
        path: Option<std::path::PathBuf>,
    },
    MediaExportWriteCompleted {
        request_id: u64,
        path: std::path::PathBuf,
        bytes: Bytes,
        result: Result<(), String>,
    },
    ConfirmMediaOverwrite,
    CancelMediaOverwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatResponseStatus {
    Generating,
    Completed,
    Cancelled,
    Failed,
    Replaced,
}

#[derive(Debug)]
pub(super) struct ChatMessage {
    pub(super) id: u64,
    pub(super) conversation_id: Option<String>,
    pub(super) created_at: u64,
    pub(super) from_user: bool,
    pub(super) content: String,
    pub(super) model_context: bool,
    pub(super) turn_id: Option<u64>,
    pub(super) request_id: Option<u64>,
    pub(super) response_status: Option<ChatResponseStatus>,
    pub(super) status_detail: Option<String>,
    pub(super) model: Option<String>,
    pub(super) markdown: Option<iced::widget::markdown::Content>,
    pub(super) document_context: bool,
    pub(super) source_status: Option<String>,
    pub(super) sources: Vec<MessageSource>,
    pub(super) document_context_mode: DocumentContextMode,
    pub(super) selected_document_ids: Vec<String>,
    pub(super) deep_think: bool,
    pub(super) thinking_received: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModelStatus {
    Loading,
    Ready,
    Unavailable,
}

pub(super) struct ActiveChatRequest {
    pub(super) id: u64,
    pub(super) assistant_message_id: u64,
    pub(super) conversation_id: String,
    pub(super) turn_id: u64,
    pub(super) model: String,
    pub(super) deep_think: bool,
    pub(super) abort: task::Handle,
    pub(super) document_context_mode: DocumentContextMode,
    pub(super) selected_document_ids: Vec<String>,
    pub(super) history: Vec<OllamaMessage>,
}

pub(super) struct App {
    pub(super) page: Page,
    pub(super) sidebar_collapsed: bool,
    pub(super) composer_tools_open: bool,
    pub(super) theme_mode: ThemeMode,
    pub(super) input: text_editor::Content,
    pub(super) messages: Vec<ChatMessage>,
    pub(super) ollama: OllamaClient,
    pub(super) models: Vec<String>,
    pub(super) selected_model: Option<String>,
    pub(super) model_status: ModelStatus,
    pub(super) model_error: Option<OllamaError>,
    pub(super) model_request_id: u64,
    pub(super) deep_think_enabled: bool,
    pub(super) deep_think_supported: Option<bool>,
    pub(super) deep_think_capability_request_id: u64,
    pub(super) next_chat_request_id: u64,
    pub(super) next_message_id: u64,
    pub(super) next_turn_id: u64,
    pub(super) active_chat: Option<ActiveChatRequest>,
    pub(super) follow_chat_bottom: bool,
    pub(super) conversations: Vec<Conversation>,
    pub(super) selected_conversation_id: Option<String>,
    pub(super) pending_delete_conversation: Option<String>,
    pub(super) deleting_conversation: Option<String>,
    pub(super) history_revision: u64,
    pub(super) history_error: Option<String>,
    pub(super) history_loaded: bool,
    pub(super) last_stream_persisted_at: Option<std::time::Instant>,
    pub(super) last_stream_persisted_bytes: usize,
    pub(super) editing_conversation_id: Option<String>,
    pub(super) conversation_title_draft: String,
    pub(super) saving_conversation_id: Option<String>,
    pub(super) thinking_frame: u8,
    pub(super) documents: Vec<DocumentMetadata>,
    pub(super) selected_document_id: Option<String>,
    pub(super) selected_document_text: Option<String>,
    pub(super) document_search_query: String,
    pub(super) document_search_results: Vec<SearchResult>,
    pub(super) document_load_request_id: u64,
    pub(super) document_detail_request_id: u64,
    pub(super) document_search_request_id: u64,
    pub(super) document_import_request_id: u64,
    pub(super) documents_loading: bool,
    pub(super) document_detail_loading: bool,
    pub(super) document_search_loading: bool,
    pub(super) document_importing: bool,
    pub(super) pending_document_removal: Option<String>,
    pub(super) removing_document_id: Option<String>,
    pub(super) document_error: Option<String>,
    pub(super) document_notice: Option<String>,
    pub(super) document_context_mode: DocumentContextMode,
    pub(super) selected_document_context_ids: Vec<String>,
    pub(super) conversation_document_contexts: HashMap<String, ChatDocumentContext>,
    pub(super) media_backend_status: BackendStatus,
    pub(super) media_process_manager: std::sync::Arc<MediaProcessManager>,
    pub(super) media_backend_request_id: u64,
    pub(super) media_generation_request_id: u64,
    pub(super) media_auto_save_request_id: u64,
    pub(super) media_active_prompt_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    pub(super) media_export_request_id: u64,
    pub(super) media_prompt: String,
    pub(super) media_steps: String,
    pub(super) media_result: Option<MediaImage>,
    pub(super) media_saved_path: Option<std::path::PathBuf>,
    pub(super) media_auto_saving: bool,
    pub(super) media_auto_save_error: Option<String>,
    pub(super) media_generating: bool,
    pub(super) media_exporting: bool,
    pub(super) media_abort: Option<task::Handle>,
    pub(super) media_error: Option<String>,
    pub(super) media_notice: Option<String>,
    pub(super) pending_media_overwrite: Option<(std::path::PathBuf, Bytes)>,
    pub(super) update_check_request_id: u64,
    pub(super) update_checking: bool,
    pub(super) latest_release: Option<crate::software_update::Release>,
    pub(super) update_last_successful_check: Option<std::time::SystemTime>,
    pub(super) update_error: Option<String>,
    pub(super) update_download_request_id: u64,
    pub(super) update_downloading: bool,
    pub(super) update_download_progress: Option<crate::software_update::DownloadProgress>,
    pub(super) verified_update: Option<crate::software_update::VerifiedPackage>,
    pub(super) update_download_abort: Option<task::Handle>,
    pub(super) pending_update_installation_confirmation: bool,
    pub(super) update_installing: bool,
    pub(super) update_installation_succeeded: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            page: Page::Chat,
            sidebar_collapsed: false,
            composer_tools_open: false,
            theme_mode: ThemeMode::Dark,
            input: text_editor::Content::new(),
            messages: vec![ChatMessage {
                id: 0,
                conversation_id: None,
                created_at: 0,
                from_user: false,
                content: "Welcome to Adham AI. Choose an available local model to begin.".into(),
                model_context: false,
                turn_id: None,
                request_id: None,
                response_status: None,
                status_detail: None,
                model: None,
                markdown: Some(crate::markdown::parse(
                    "Welcome to Adham AI. Choose an available local model to begin.",
                )),
                document_context: false,
                source_status: None,
                sources: Vec::new(),
                document_context_mode: DocumentContextMode::NoDocuments,
                selected_document_ids: Vec::new(),
                deep_think: false,
                thinking_received: false,
            }],
            ollama: OllamaClient::new(),
            models: Vec::new(),
            selected_model: None,
            model_status: ModelStatus::Loading,
            model_error: None,
            model_request_id: 1,
            deep_think_enabled: false,
            deep_think_supported: None,
            deep_think_capability_request_id: 0,
            next_chat_request_id: 0,
            next_message_id: 0,
            next_turn_id: 0,
            active_chat: None,
            follow_chat_bottom: true,
            conversations: Vec::new(),
            selected_conversation_id: None,
            pending_delete_conversation: None,
            deleting_conversation: None,
            history_revision: 0,
            history_error: None,
            history_loaded: true,
            last_stream_persisted_at: None,
            last_stream_persisted_bytes: 0,
            editing_conversation_id: None,
            conversation_title_draft: String::new(),
            saving_conversation_id: None,
            thinking_frame: 0,
            documents: Vec::new(),
            selected_document_id: None,
            selected_document_text: None,
            document_search_query: String::new(),
            document_search_results: Vec::new(),
            document_load_request_id: 1,
            document_detail_request_id: 0,
            document_search_request_id: 0,
            document_import_request_id: 0,
            documents_loading: true,
            document_detail_loading: false,
            document_search_loading: false,
            document_importing: false,
            pending_document_removal: None,
            removing_document_id: None,
            document_error: None,
            document_notice: None,
            document_context_mode: DocumentContextMode::NoDocuments,
            selected_document_context_ids: Vec::new(),
            conversation_document_contexts: HashMap::new(),
            media_backend_status: BackendStatus::Checking,
            media_process_manager: std::sync::Arc::new(MediaProcessManager::default()),
            media_backend_request_id: 1,
            media_generation_request_id: 0,
            media_auto_save_request_id: 0,
            media_active_prompt_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
            media_export_request_id: 0,
            media_prompt: String::new(),
            media_steps: "20".into(),
            media_result: None,
            media_saved_path: None,
            media_auto_saving: false,
            media_auto_save_error: None,
            media_generating: false,
            media_exporting: false,
            media_abort: None,
            media_error: None,
            media_notice: None,
            pending_media_overwrite: None,
            update_check_request_id: 0,
            update_checking: false,
            latest_release: None,
            update_last_successful_check: None,
            update_error: None,
            update_download_request_id: 0,
            update_downloading: false,
            update_download_progress: None,
            verified_update: None,
            update_download_abort: None,
            pending_update_installation_confirmation: false,
            update_installing: false,
            update_installation_succeeded: false,
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(abort) = self.update_download_abort.take() {
            abort.abort();
        }
        self.media_process_manager.shutdown_owned();
    }
}

impl App {
    pub(super) fn new() -> (Self, Task<Message>) {
        let mut app = Self::default();
        app.history_loaded = false;
        let model_task = update::load_models_task(&app.ollama, app.model_request_id);
        let history_task = Task::perform(history::load_all_async(), |result| {
            Message::HistoryLoaded(result.map_err(|error| error.to_string()))
        });
        let documents_task = Task::perform(documents::load_documents_async(), |result| {
            Message::DocumentsLoaded {
                request_id: 1,
                result,
            }
        });
        let media_task = update::media_probe_task(
            app.media_backend_request_id,
            app.media_process_manager.clone(),
        );
        (
            app,
            Task::batch([model_task, history_task, documents_task, media_task]),
        )
    }

    pub(super) fn update(&mut self, message: Message) -> Task<Message> {
        update::update(self, message)
    }

    pub(super) fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = Vec::new();
        if self.active_chat.is_some() {
            subscriptions
                .push(iced::time::every(Duration::from_millis(120)).map(|_| Message::ThinkingTick));
        }
        if self.media_process_manager.state().is_busy() {
            subscriptions.push(
                iced::time::every(Duration::from_millis(250)).map(|_| Message::MediaLifecycleTick),
            );
        }
        if self.page == Page::MediaLab && self.media_backend_status.is_available() {
            subscriptions
                .push(iced::time::every(Duration::from_secs(15)).map(|_| Message::MediaHealthTick));
        }
        Subscription::batch(subscriptions)
    }

    pub(super) fn thinking_assistant_id(&self) -> Option<u64> {
        self.active_chat
            .as_ref()
            .map(|request| request.assistant_message_id)
    }

    pub(super) fn theme(&self) -> Theme {
        self.theme_mode.iced_theme()
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let colors = self.theme_mode.colors();
        let visible_model = self
            .active_chat
            .as_ref()
            .map(|request| &request.model)
            .or(self.selected_model.as_ref());
        let sidebar = components::sidebar(
            self.sidebar_collapsed,
            self.page,
            &self.conversations,
            self.selected_conversation_id.as_deref(),
            self.pending_delete_conversation.as_deref(),
            self.deleting_conversation.as_deref(),
            self.editing_conversation_id.as_deref(),
            &self.conversation_title_draft,
            self.saving_conversation_id.as_deref(),
            self.active_chat.is_none(),
            self.history_error.as_deref(),
            colors,
        );
        let header = components::page_header(self.page.title(), colors);
        let content = pages::view(
            self.page,
            self.theme_mode,
            &self.input,
            &self.messages,
            &self.models,
            visible_model,
            self.model_status,
            self.model_error.as_ref(),
            self.active_chat.is_some(),
            self.deep_think_supported == Some(true),
            self.deep_think_enabled,
            self.composer_tools_open,
            self.thinking_assistant_id(),
            self.thinking_frame,
            self.retryable_assistant_id(),
            self.follow_chat_bottom,
            self.history_loaded,
            self.document_context_mode,
            &self.selected_document_context_ids,
            &self.documents,
            self.selected_document_id.as_deref(),
            self.selected_document_text.as_deref(),
            &self.document_search_query,
            &self.document_search_results,
            self.documents_loading,
            self.document_detail_loading,
            self.document_search_loading,
            self.document_importing,
            self.pending_document_removal.as_deref(),
            self.removing_document_id.as_deref(),
            self.document_error.as_deref(),
            self.document_notice.as_deref(),
            &self.media_backend_status,
            &self.media_prompt,
            &self.media_steps,
            self.media_result.as_ref(),
            self.media_saved_path.as_deref(),
            self.media_auto_saving,
            self.media_auto_save_error.as_deref(),
            self.media_generating,
            self.media_exporting,
            self.media_error.as_deref(),
            self.media_notice.as_deref(),
            self.pending_media_overwrite
                .as_ref()
                .map(|(path, _)| path.as_path()),
            self.latest_release.as_ref(),
            self.update_checking,
            self.update_last_successful_check,
            self.update_error.as_deref(),
            self.update_downloading,
            self.update_download_progress,
            self.verified_update.as_ref(),
            self.pending_update_installation_confirmation,
            self.update_installing,
            self.update_installation_succeeded,
            colors,
        );

        components::application_shell(sidebar, header, content, colors)
    }

    fn retryable_assistant_id(&self) -> Option<u64> {
        if self.active_chat.is_some() || self.selected_model.is_none() {
            return None;
        }

        let turn_id = self
            .messages
            .iter()
            .rev()
            .find(|message| message.from_user)?
            .turn_id?;
        let response = self.messages.iter().rev().find(|message| {
            !message.from_user
                && message.turn_id == Some(turn_id)
                && message.response_status != Some(ChatResponseStatus::Replaced)
        })?;

        matches!(
            response.response_status,
            Some(
                ChatResponseStatus::Completed
                    | ChatResponseStatus::Cancelled
                    | ChatResponseStatus::Failed
            )
        )
        .then_some(response.request_id?)
    }
}
