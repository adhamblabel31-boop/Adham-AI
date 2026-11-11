use iced::keyboard::{Key, key::Named};
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_editor, text_input,
    tooltip,
};
use iced::{Alignment, Background, Element, Length};

use crate::app::{
    ChatMessage, ChatResponseStatus, DocumentContextMode, Message, ModelStatus, Page,
};
use crate::components;
use crate::documents::{DocumentMetadata, SearchResult};
use crate::icons::Icon;
use crate::markdown;
use crate::media::{BackendStatus, MediaImage};
use crate::ollama::OllamaError;
use crate::theme::{Colors, ThemeMode};

const CHAT_SCROLL_ID: &str = "chat-messages";

fn documents_page<'a>(
    documents: &'a [DocumentMetadata],
    selected_document_id: Option<&'a str>,
    selected_document_text: Option<&'a str>,
    selected_document_context_ids: &'a [String],
    search_query: &'a str,
    search_results: &'a [SearchResult],
    documents_loading: bool,
    detail_loading: bool,
    search_loading: bool,
    importing: bool,
    pending_removal: Option<&'a str>,
    removing_id: Option<&'a str>,
    error: Option<&'a str>,
    notice: Option<&'a str>,
    colors: Colors,
) -> Element<'a, Message> {
    let search_content: Element<'a, Message> = if !search_query.trim().is_empty() {
        if search_loading {
            text("Searching local documents…")
                .size(12)
                .color(colors.muted)
                .into()
        } else if search_results.is_empty() {
            text("No matching documents.")
                .size(12)
                .color(colors.muted)
                .into()
        } else {
            search_results
                .iter()
                .fold(column![].spacing(6), |items, result| {
                    let title = truncate_document_name(&result.filename, 30);
                    items.push(
                        button(
                            column![
                                text(title).size(12).color(colors.foreground),
                                text(format!("MATCH · {}", result.matched_fragments.join(" · ")))
                                    .size(10)
                                    .color(colors.accent),
                                text(result.excerpt.as_str()).size(11).color(colors.muted)
                            ]
                            .spacing(4),
                        )
                        .width(Length::Fill)
                        .padding(8)
                        .on_press(Message::SelectDocument(result.document_id.clone()))
                        .style(move |_, status| button::Style {
                            background: Some(Background::Color(
                                if matches!(status, button::Status::Hovered) {
                                    colors.hover_surface
                                } else {
                                    colors.surface
                                },
                            )),
                            border: iced::Border {
                                color: colors.border,
                                width: 1.0,
                                radius: 7.0.into(),
                            },
                            ..button::Style::default()
                        }),
                    )
                })
                .into()
        }
    } else if documents_loading {
        text("Loading documents…")
            .size(12)
            .color(colors.muted)
            .into()
    } else if documents.is_empty() {
        text("No documents yet. Import a TXT, Markdown, or searchable PDF file.")
            .size(12)
            .color(colors.muted)
            .into()
    } else {
        documents
            .iter()
            .fold(column![].spacing(5), |items, document| {
                let selected = selected_document_id == Some(document.id.as_str());
                let title = truncate_document_name(&document.original_filename, 24);
                let select = button(
                    column![
                        text(title).size(12).color(colors.foreground),
                        text(format!(
                            "{} · {} · Indexed",
                            document.file_type.to_ascii_uppercase(),
                            format_size(document.file_size)
                        ))
                        .size(10)
                        .color(colors.muted)
                    ]
                    .spacing(3),
                )
                .width(Length::Fill)
                .padding(8)
                .on_press(Message::SelectDocument(document.id.clone()))
                .style(move |_, status| button::Style {
                    background: Some(Background::Color(if selected {
                        colors.selected_surface
                    } else if matches!(status, button::Status::Hovered) {
                        colors.hover_surface
                    } else {
                        colors.surface
                    })),
                    border: iced::Border {
                        color: if selected {
                            colors.accent
                        } else {
                            colors.border
                        },
                        width: 1.0,
                        radius: 7.0.into(),
                    },
                    ..button::Style::default()
                });
                items.push(
                    row![
                        select,
                        components::icon_button(
                            Icon::Trash,
                            "Remove document",
                            Some(Message::RequestRemoveDocument(document.id.clone())),
                            false,
                            colors,
                        )
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
            })
            .into()
    };

    let selected_document =
        selected_document_id.and_then(|id| documents.iter().find(|document| document.id == id));
    let document_viewer: Element<'a, Message> = if detail_loading {
        text("Loading extracted text…")
            .size(12)
            .color(colors.muted)
            .into()
    } else if let (Some(document), Some(content)) = (selected_document, selected_document_text) {
        column![
            row![
                text(document.original_filename.as_str())
                    .size(16)
                    .color(colors.foreground),
                iced::widget::Space::new().width(Length::Fill),
                components::icon_button(
                    Icon::Trash,
                    "Remove document",
                    Some(Message::RequestRemoveDocument(document.id.clone())),
                    false,
                    colors,
                )
            ]
            .align_y(Alignment::Center),
            text(format!(
                "{} · {} · Indexed locally",
                document.file_type.to_ascii_uppercase(),
                format_size(document.file_size)
            ))
            .size(10)
            .color(colors.muted),
            checkbox(selected_document_context_ids.contains(&document.id))
                .label("Use this document for this conversation")
                .on_toggle(move |_| Message::ToggleDocumentChatContext(document.id.clone())),
            scrollable(
                text(content)
                    .size(13)
                    .color(colors.foreground)
                    .width(Length::Fill)
            )
            .height(Length::Fill)
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    } else if let Some(document) = selected_document {
        column![
            row![
                text(document.original_filename.as_str())
                    .size(16)
                    .color(colors.foreground),
                iced::widget::Space::new().width(Length::Fill),
                components::icon_button(
                    Icon::Trash,
                    "Remove document",
                    Some(Message::RequestRemoveDocument(document.id.clone())),
                    false,
                    colors,
                )
            ]
            .align_y(Alignment::Center),
            text("Stored text is unavailable.")
                .size(12)
                .color(colors.muted)
        ]
        .spacing(8)
        .into()
    } else {
        column![
            text("Document preview").size(16).color(colors.foreground),
            text("Select an imported document to read its locally stored text.")
                .size(12)
                .color(colors.muted)
        ]
        .spacing(8)
        .into()
    };

    let removal_confirmation: Element<'a, Message> = if let Some(id) = pending_removal
        && let Some(document) = documents.iter().find(|document| document.id == id)
    {
        let removing = removing_id == Some(id);
        column![
            text(format!(
                "{} “{}”?",
                if removing { "Removing" } else { "Remove" },
                truncate_document_name(&document.original_filename, 24)
            ))
            .size(12)
            .color(colors.foreground),
            row![
                components::icon_button(
                    Icon::Trash,
                    "Confirm document removal",
                    (!removing).then_some(Message::ConfirmRemoveDocument),
                    true,
                    colors,
                ),
                components::icon_button(
                    Icon::Close,
                    "Cancel document removal",
                    (!removing).then_some(Message::CancelRemoveDocument),
                    false,
                    colors,
                )
            ]
            .spacing(4)
        ]
        .spacing(5)
        .into()
    } else {
        column![].into()
    };

    let import_button = components::icon_button(
        Icon::Plus,
        "Import documents",
        (!importing).then_some(Message::ImportDocuments),
        true,
        colors,
    );
    column![
        row![
            column![
                text("DOCUMENT LIBRARY").size(10).color(colors.muted),
                text("Local documents").size(18).color(colors.foreground)
            ]
            .spacing(4),
            iced::widget::Space::new().width(Length::Fill),
            import_button
        ]
        .align_y(Alignment::Center),
        text(
            "Browse and search locally stored text. Chat retrieval is not included in this phase."
        )
        .size(12)
        .color(colors.muted),
        row![
            components::icon_button(Icon::Search, "Search local documents", None, false, colors),
            text_input("Search documents…", search_query)
                .on_input(Message::DocumentSearchEdited)
                .padding([10, 12])
                .size(13)
                .style(move |theme, status| {
                    let mut style = iced::widget::text_input::default(theme, status);
                    style.background = Background::Color(colors.surface);
                    style.border.color = colors.border;
                    style.value = colors.foreground;
                    style.placeholder = colors.muted;
                    style
                })
        ]
        .spacing(7)
        .align_y(Alignment::Center),
        row![
            components::card(
                column![
                    text(if search_query.trim().is_empty() {
                        "DOCUMENTS"
                    } else {
                        "SEARCH RESULTS"
                    })
                    .size(10)
                    .color(colors.muted),
                    scrollable(search_content).height(Length::Fill)
                ]
                .spacing(8)
                .height(Length::Fill),
                colors.surface,
                colors.border,
                10
            ),
            components::card(document_viewer, colors.surface, colors.border, 14)
        ]
        .spacing(12)
        .height(Length::Fill),
        removal_confirmation,
        if importing {
            text("Importing and indexing documents locally…")
                .size(11)
                .color(colors.muted)
        } else {
            text(notice.unwrap_or("")).size(11).color(colors.accent)
        },
        if let Some(error) = error {
            text(error).size(11).color(colors.accent)
        } else {
            text("").size(11)
        }
    ]
    .spacing(12)
    .height(Length::Fill)
    .into()
}

fn truncate_document_name(name: &str, max_chars: usize) -> String {
    let mut chars = name.chars();
    let visible = chars
        .by_ref()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    if chars.next().is_some() {
        format!("{visible}…")
    } else {
        name.to_owned()
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

pub(super) fn view<'a>(
    page: Page,
    theme_mode: ThemeMode,
    input: &'a text_editor::Content,
    messages: &'a [ChatMessage],
    models: &'a [String],
    selected_model: Option<&'a String>,
    model_status: ModelStatus,
    model_error: Option<&OllamaError>,
    chat_loading: bool,
    deep_think_supported: bool,
    deep_think_enabled: bool,
    composer_tools_open: bool,
    thinking_assistant_id: Option<u64>,
    thinking_frame: u8,
    retryable_assistant_id: Option<u64>,
    follow_chat_bottom: bool,
    history_loaded: bool,
    document_context_mode: DocumentContextMode,
    selected_document_context_ids: &'a [String],
    documents: &'a [DocumentMetadata],
    selected_document_id: Option<&'a str>,
    selected_document_text: Option<&'a str>,
    document_search_query: &'a str,
    document_search_results: &'a [SearchResult],
    documents_loading: bool,
    document_detail_loading: bool,
    document_search_loading: bool,
    document_importing: bool,
    pending_document_removal: Option<&'a str>,
    removing_document_id: Option<&'a str>,
    document_error: Option<&'a str>,
    document_notice: Option<&'a str>,
    media_backend_status: &'a BackendStatus,
    media_prompt: &'a str,
    media_steps: &'a str,
    media_result: Option<&'a MediaImage>,
    media_saved_path: Option<&'a std::path::Path>,
    media_auto_saving: bool,
    media_auto_save_error: Option<&'a str>,
    media_generating: bool,
    media_exporting: bool,
    media_error: Option<&'a str>,
    media_notice: Option<&'a str>,
    pending_media_overwrite: Option<&'a std::path::Path>,
    latest_release: Option<&'a crate::software_update::Release>,
    update_checking: bool,
    last_successful_update_check: Option<std::time::SystemTime>,
    update_error: Option<&'a str>,
    update_downloading: bool,
    update_download_progress: Option<crate::software_update::DownloadProgress>,
    verified_update: Option<&'a crate::software_update::VerifiedPackage>,
    pending_update_installation_confirmation: bool,
    update_installing: bool,
    update_installation_succeeded: bool,
    colors: Colors,
) -> Element<'a, Message> {
    match page {
        Page::Chat => chat_page(
            input,
            messages,
            models,
            selected_model,
            model_status,
            model_error,
            chat_loading,
            deep_think_supported,
            deep_think_enabled,
            composer_tools_open,
            thinking_assistant_id,
            thinking_frame,
            retryable_assistant_id,
            follow_chat_bottom,
            history_loaded,
            document_context_mode,
            documents.len(),
            selected_document_context_ids,
            theme_mode,
            colors,
        ),
        Page::Documents => documents_page(
            documents,
            selected_document_id,
            selected_document_text,
            selected_document_context_ids,
            document_search_query,
            document_search_results,
            documents_loading,
            document_detail_loading,
            document_search_loading,
            document_importing,
            pending_document_removal,
            removing_document_id,
            document_error,
            document_notice,
            colors,
        ),
        Page::MediaLab => media_lab_page(
            media_backend_status,
            media_prompt,
            media_steps,
            media_result,
            media_saved_path,
            media_auto_saving,
            media_auto_save_error,
            media_generating,
            media_exporting,
            media_error,
            media_notice,
            pending_media_overwrite,
            colors,
        ),
        Page::Settings => settings_page(
            theme_mode,
            models,
            selected_model,
            model_status,
            model_error,
            chat_loading,
            latest_release,
            update_checking,
            last_successful_update_check,
            update_error,
            update_downloading,
            update_download_progress,
            verified_update,
            pending_update_installation_confirmation,
            update_installing,
            update_installation_succeeded,
            colors,
        ),
    }
}

fn media_lab_page<'a>(
    backend_status: &'a BackendStatus,
    prompt: &'a str,
    steps: &'a str,
    result_image: Option<&'a MediaImage>,
    saved_path: Option<&'a std::path::Path>,
    auto_saving: bool,
    auto_save_error: Option<&'a str>,
    generating: bool,
    exporting: bool,
    error: Option<&'a str>,
    notice: Option<&'a str>,
    pending_overwrite: Option<&'a std::path::Path>,
    colors: Colors,
) -> Element<'a, Message> {
    let backend_ready = backend_status.is_available();
    let status_color = if backend_ready {
        colors.selected_icon
    } else {
        colors.muted
    };
    let prompt_control = text_input("Describe the image you want to create…", prompt)
        .on_input(Message::MediaPromptChanged)
        .size(14)
        .padding(10)
        .width(Length::Fill);
    let steps_control = text_input("20", steps)
        .on_input_maybe((!generating).then_some(Message::MediaStepsChanged))
        .size(14)
        .padding(10)
        .width(Length::Fixed(100.0));

    let generation_action: Element<'a, Message> = if generating {
        row![
            text("ComfyUI is generating on the CPU. Progress is not reported; generation may take several minutes.")
                .size(12)
                .color(colors.muted),
            button("Cancel")
                .padding([8, 12])
                .on_press(Message::CancelMediaGeneration)
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
    } else {
        button(text("Generate Image").size(13))
            .padding([9, 14])
            .on_press_maybe(
                (backend_ready && !prompt.trim().is_empty() && !auto_saving && !exporting)
                    .then_some(Message::GenerateMedia),
            )
            .into()
    };

    let backend_card = components::card(
        column![
            text("LOCAL IMAGE BACKEND")
                .size(10)
                .color(colors.selected_icon),
            text(backend_status.user_message())
                .size(12)
                .color(status_color),
            text(format!(
                "API endpoint: {}",
                crate::media::api_base_url_display()
            ))
            .size(11)
            .color(colors.muted),
            text("Override with ADHAM_AI_COMFYUI_URL (localhost only).")
                .size(11)
                .color(colors.muted),
            row![
                text("Text-to-image only · 512 × 512 · CPU")
                    .size(11)
                    .color(colors.muted),
                iced::widget::Space::new().width(Length::Fill),
                button("Check again").padding([6, 10]).on_press_maybe(
                    (!matches!(
                        backend_status,
                        BackendStatus::Checking | BackendStatus::Starting
                    ))
                    .then_some(Message::RefreshMediaBackend),
                )
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        ]
        .spacing(8),
        colors.surface,
        colors.border,
        14,
    );

    let result_content: Element<'a, Message> = if let Some(image) = result_image {
        let save_status: Element<'a, Message> = if auto_saving {
            text("Saving generated image…")
                .size(11)
                .color(colors.muted)
                .into()
        } else if let Some(path) = saved_path {
            column![
                text("Saved automatically:").size(11).color(colors.muted),
                text(path.display().to_string())
                    .size(11)
                    .color(colors.foreground),
                button("Open output folder")
                    .padding([6, 10])
                    .on_press(Message::OpenMediaOutputFolder)
            ]
            .spacing(6)
            .into()
        } else if let Some(error) = auto_save_error {
            column![
                text(format!("Automatic save failed: {error}"))
                    .size(11)
                    .color(iced::Color::from_rgb8(0xE5, 0x7B, 0x7B)),
                button("Retry save")
                    .padding([6, 10])
                    .on_press(Message::RetryMediaAutoSave)
            ]
            .spacing(6)
            .into()
        } else {
            iced::widget::Space::new().into()
        };
        column![
            row![
                text("Generated result").size(14).color(colors.foreground),
                iced::widget::Space::new().width(Length::Fill),
                button(if exporting { "Saving…" } else { "Export PNG" })
                    .padding([7, 11])
                    .on_press_maybe((!exporting).then_some(Message::ExportMediaImage))
            ]
            .align_y(Alignment::Center),
            iced::widget::image::Image::new(image.handle.clone())
                .width(Length::Fill)
                .height(Length::Fixed(360.0))
                .content_fit(iced::ContentFit::Contain),
            text("Generated locally with ComfyUI.")
                .size(11)
                .color(colors.muted),
            save_status
        ]
        .spacing(8)
        .into()
    } else {
        text("Generated images will appear here.")
            .size(12)
            .color(colors.muted)
            .into()
    };

    let overwrite_prompt: Element<'a, Message> = if let Some(path) = pending_overwrite {
        row![
            text(format!(
                "“{}” already exists. Replace it?",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("selected file")
            ))
            .size(12)
            .color(colors.foreground),
            button("Replace")
                .padding([6, 10])
                .on_press(Message::ConfirmMediaOverwrite),
            button("Cancel")
                .padding([6, 10])
                .on_press(Message::CancelMediaOverwrite)
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    } else {
        iced::widget::Space::new().into()
    };

    let feedback: Element<'a, Message> = if let Some(error) = error {
        text(error)
            .size(12)
            .color(iced::Color::from_rgb8(0xE5, 0x7B, 0x7B))
            .into()
    } else if let Some(notice) = notice {
        text(notice).size(12).color(colors.selected_icon).into()
    } else {
        iced::widget::Space::new().into()
    };

    scrollable(
        column![
            text("Create images locally through your existing ComfyUI installation.")
                .size(13)
                .color(colors.muted),
            backend_card,
            components::card(
                column![
                    text("PROMPT").size(10).color(colors.muted),
                    prompt_control,
                    row![
                        text("Steps").size(12).color(colors.foreground),
                        steps_control,
                        text("Choose 1–100 sampling steps.")
                            .size(11)
                            .color(colors.muted)
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                    text("Image-to-image editing is not supported by this ComfyUI workflow.")
                        .size(11)
                        .color(colors.muted),
                    row![generation_action].align_y(Alignment::Center),
                    feedback,
                    overwrite_prompt
                ]
                .spacing(10),
                colors.surface,
                colors.border,
                14,
            ),
            components::card(
                column![result_content].spacing(8),
                colors.surface,
                colors.border,
                14,
            )
        ]
        .spacing(14)
        .width(Length::Fill)
        .max_width(900),
    )
    .height(Length::Fill)
    .into()
}

fn chat_page<'a>(
    input: &'a text_editor::Content,
    messages: &'a [ChatMessage],
    models: &'a [String],
    selected_model: Option<&'a String>,
    model_status: ModelStatus,
    model_error: Option<&OllamaError>,
    chat_loading: bool,
    deep_think_supported: bool,
    deep_think_enabled: bool,
    composer_tools_open: bool,
    thinking_assistant_id: Option<u64>,
    thinking_frame: u8,
    retryable_assistant_id: Option<u64>,
    follow_chat_bottom: bool,
    history_loaded: bool,
    document_context_mode: DocumentContextMode,
    document_count: usize,
    selected_document_context_ids: &'a [String],
    theme_mode: ThemeMode,
    colors: Colors,
) -> Element<'a, Message> {
    let message_list = messages
        .iter()
        .fold(column![].spacing(14), |items, message| {
            items.push(message_card(
                message,
                is_thinking_message(message.id, thinking_assistant_id),
                thinking_frame,
                retryable_assistant_id,
                theme_mode,
                colors,
            ))
        });
    let message_list = message_list.width(Length::Fill).align_x(Alignment::Start);

    let model_selector: Element<'_, Message> = if chat_loading {
        container(text(selected_model.map_or("No model selected", String::as_str)).size(14))
            .padding([9, 12])
            .style(move |_| container::Style {
                background: Some(Background::Color(colors.hover_surface)),
                border: iced::Border {
                    color: colors.border,
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..container::Style::default()
            })
            .into()
    } else {
        pick_list(models, selected_model, Message::SelectModel)
            .placeholder(if model_status == ModelStatus::Loading {
                "Loading models..."
            } else {
                "Select a model"
            })
            .width(Length::FillPortion(3))
            .into()
    };
    let refresh = components::icon_button(
        Icon::Refresh,
        "Refresh models",
        (model_status != ModelStatus::Loading && !chat_loading).then_some(Message::RefreshModels),
        false,
        colors,
    );

    let model_status_text = match model_status {
        ModelStatus::Loading => Some("Connecting to local Ollama...".to_owned()),
        ModelStatus::Ready => None,
        ModelStatus::Unavailable => Some(
            model_error
                .unwrap_or(&OllamaError::NoModels)
                .user_message()
                .to_owned(),
        ),
    };

    let context_hint = match document_context_mode {
        DocumentContextMode::NoDocuments => None,
        DocumentContextMode::AllDocuments if document_count == 0 => {
            Some("Import documents to enable local document context.")
        }
        DocumentContextMode::SelectedDocuments if selected_document_context_ids.is_empty() => {
            Some("Choose documents on the Documents page to use them in this conversation.")
        }
        _ => None,
    };

    let composer_options: Element<'_, Message> = if composer_tools_open {
        let model_controls = row![
            text("Model").size(12).color(colors.muted),
            model_selector,
            refresh
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let context_controls = row![
            text("Document context").size(12).color(colors.muted),
            pick_list(
                DocumentContextMode::ALL,
                Some(document_context_mode),
                Message::SelectDocumentContextMode,
            )
            .width(Length::Fill)
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let options = column![
            model_controls,
            context_controls,
            if let Some(status) = model_status_text.as_deref() {
                text(status.to_owned()).size(11).color(colors.muted)
            } else {
                text("").size(11)
            },
            if let Some(hint) = context_hint {
                text(hint).size(11).color(colors.muted)
            } else {
                text("").size(11)
            }
        ]
        .spacing(6);

        container(options)
            .width(Length::Fill)
            .padding(10)
            .style(move |_| container::Style {
                background: Some(Background::Color(colors.hover_surface)),
                border: iced::Border {
                    color: colors.border,
                    width: 1.0,
                    radius: 14.0.into(),
                },
                ..container::Style::default()
            })
            .into()
    } else {
        row![].into()
    };

    let think = button(text("Think").size(14).color(if deep_think_enabled {
        colors.selected_icon
    } else {
        colors.muted
    }))
    .padding([8, 12])
    .on_press_maybe(
        (deep_think_supported && !chat_loading)
            .then_some(Message::SetDeepThink(!deep_think_enabled)),
    )
    .style(move |_, status| button::Style {
        background: Some(Background::Color(if deep_think_enabled {
            colors.selected_surface
        } else if matches!(status, button::Status::Hovered) {
            colors.hover_surface
        } else {
            colors.surface
        })),
        border: iced::Border {
            color: if deep_think_enabled {
                colors.accent
            } else {
                colors.border
            },
            width: 1.0,
            radius: 18.0.into(),
        },
        ..button::Style::default()
    });
    let think_hint = if chat_loading {
        "Think mode cannot be changed while generating"
    } else if deep_think_supported {
        "Toggle Think mode"
    } else {
        "Think mode is not supported by the selected model"
    };
    let think = tooltip::Tooltip::new(think, text(think_hint).size(12), tooltip::Position::Top)
        .delay(std::time::Duration::from_millis(350));

    let composer = column![
        composer_options,
        row![
            components::icon_button(
                Icon::Plus,
                if composer_tools_open {
                    "Close tools"
                } else {
                    "Add model or document context"
                },
                Some(Message::ToggleComposerTools),
                false,
                colors,
            ),
            text_editor(input)
                .placeholder("Ask Adham")
                .on_action(Message::ComposerEdited)
                .key_binding(|key_press| match key_press.key {
                    Key::Named(Named::Enter) if key_press.modifiers.shift() => {
                        Some(text_editor::Binding::Enter)
                    }
                    Key::Named(Named::Enter) => {
                        Some(text_editor::Binding::Custom(Message::Send))
                    }
                    _ => text_editor::Binding::from_key_press(key_press),
                })
                .min_height(36)
                .max_height(120)
                .size(16)
                .padding([7, 9])
                .style(move |theme, status| {
                    let mut style = text_editor::default(theme, status);
                    style.background = Background::Color(colors.surface);
                    style.value = colors.foreground;
                    style.placeholder = colors.muted;
                    style.selection = colors.accent;
                    style.border.color = colors.surface;
                    style
                }),
            think,
            if chat_loading {
                components::icon_button(
                    Icon::Square,
                    "Stop generation",
                    Some(Message::CancelChat),
                    true,
                    colors,
                )
            } else {
                components::icon_button(
                    Icon::ArrowUp,
                    "Send message",
                    (!input.text().trim().is_empty() && selected_model.is_some() && history_loaded)
                        .then_some(Message::Send),
                    true,
                    colors,
                )
            }
        ]
        .spacing(6)
        .align_y(Alignment::Center)
    ]
    .spacing(8);

    let jump_to_latest: Element<'_, Message> = if follow_chat_bottom {
        row![].into()
    } else {
        row![
            iced::widget::Space::new().width(Length::Fill),
            components::icon_button(
                Icon::ArrowDown,
                "Go to latest",
                Some(Message::JumpToLatest),
                false,
                colors,
            )
        ]
        .into()
    };

    column![
        scrollable(message_list)
            .id(CHAT_SCROLL_ID)
            .on_scroll(|viewport| Message::ChatScrolled {
                at_bottom: crate::update::at_bottom(viewport.relative_offset().y),
            })
            .width(Length::Fill)
            .height(Length::Fill),
        if !history_loaded {
            text("Loading chat history...").size(12).color(colors.muted)
        } else {
            text("").size(12)
        },
        jump_to_latest,
        container(composer)
            .width(Length::Fill)
            .padding(8)
            .style(move |_| container::Style {
                background: Some(Background::Color(colors.surface)),
                border: iced::Border {
                    color: colors.border,
                    width: 1.0,
                    radius: 22.0.into(),
                },
                ..container::Style::default()
            })
    ]
    .spacing(10)
    .height(Length::Fill)
    .into()
}

pub(super) fn is_thinking_message(message_id: u64, active_assistant_id: Option<u64>) -> bool {
    active_assistant_id == Some(message_id)
}

pub(super) fn deep_think_status(
    deep_think: bool,
    from_user: bool,
    thinking_received: bool,
    response_status: Option<ChatResponseStatus>,
) -> Option<&'static str> {
    if !deep_think || from_user {
        return None;
    }

    match response_status {
        Some(ChatResponseStatus::Generating) if thinking_received => Some("Thinking..."),
        Some(ChatResponseStatus::Generating) | None => None,
        Some(ChatResponseStatus::Completed | ChatResponseStatus::Replaced) if thinking_received => {
            Some("Deep Think completed")
        }
        Some(ChatResponseStatus::Completed | ChatResponseStatus::Replaced) => {
            Some("No thinking data received")
        }
        Some(ChatResponseStatus::Cancelled) => Some("Deep Think stopped"),
        Some(ChatResponseStatus::Failed) => Some("Deep Think failed"),
    }
}

fn message_card<'a>(
    message: &'a ChatMessage,
    show_thinking: bool,
    thinking_frame: u8,
    retryable_assistant_id: Option<u64>,
    theme_mode: ThemeMode,
    colors: Colors,
) -> Element<'a, Message> {
    let mut content = column![].spacing(8);

    if message.deep_think && !message.from_user {
        content = content.push(text("Deep Think").size(10).color(colors.selected_icon));
        if let Some(status) = deep_think_status(
            message.deep_think,
            message.from_user,
            message.thinking_received,
            message.response_status,
        ) {
            content = content.push(text(status).size(11).color(colors.muted));
        }
    } else if message.deep_think {
        content = content.push(text("Deep Think").size(10).color(colors.button_text));
    }

    if show_thinking && !message.from_user && (!message.deep_think || message.thinking_received) {
        let phase = f32::from(thinking_frame % 24) / 24.0 * std::f32::consts::TAU;
        let alpha = 0.28 + (phase.sin() + 1.0) * 0.25;
        let color =
            iced::Color::from_rgba(colors.accent.r, colors.accent.g, colors.accent.b, alpha);
        content = content.push(
            container(iced::widget::Space::new().width(8).height(8))
                .padding(1)
                .style(move |_| container::Style {
                    background: Some(Background::Color(color)),
                    border: iced::Border {
                        radius: 5.0.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                }),
        );
    }

    if message.from_user && !message.content.is_empty() {
        content = content.push(
            text(message.content.as_str())
                .size(15)
                .color(colors.button_text),
        );
    } else if !message.from_user
        && let Some(markdown_content) = message.markdown.as_ref()
    {
        content = content.push(markdown::view(markdown_content, theme_mode));
    }

    if message.response_status == Some(ChatResponseStatus::Failed)
        && let Some(detail) = message.status_detail.as_deref()
    {
        content = content.push(text(detail).size(12).color(colors.muted));
    }

    if !message.from_user && message.document_context {
        content = content.push(text("SOURCES").size(10).color(colors.muted));
        if let Some(status) = message.source_status.as_deref() {
            content = content.push(text(status).size(11).color(colors.muted));
        }
        for source in &message.sources {
            content = content.push(
                button(
                    column![
                        text(source.filename.as_str()).size(11).color(colors.accent),
                        text(source.excerpt.as_str()).size(11).color(colors.muted)
                    ]
                    .spacing(3),
                )
                .padding(7)
                .on_press(Message::OpenSourceDocument(source.document_id.clone()))
                .style(move |_, status| button::Style {
                    background: Some(Background::Color(
                        if matches!(status, button::Status::Hovered) {
                            colors.hover_surface
                        } else {
                            colors.surface
                        },
                    )),
                    border: iced::Border {
                        color: colors.border,
                        width: 1.0,
                        radius: 7.0.into(),
                    },
                    ..button::Style::default()
                }),
            );
        }
    }

    if !message.from_user && message.request_id == retryable_assistant_id {
        if let Some(turn_id) = message.turn_id {
            content = content.push(components::icon_button(
                Icon::Refresh,
                "Retry response",
                Some(Message::RetryPrompt { turn_id }),
                false,
                colors,
            ));
        }
    }

    if message.from_user {
        let bubble: Element<'_, Message> = container(content)
            .width(Length::Shrink)
            .max_width(840)
            .align_x(Alignment::Start)
            .padding([12, 16])
            .style(move |_| container::Style {
                background: Some(Background::Color(colors.accent)),
                border: iced::Border {
                    color: colors.border,
                    width: 0.5,
                    radius: 16.0.into(),
                },
                ..container::Style::default()
            })
            .into();

        row![iced::widget::Space::new().width(Length::Fill), bubble]
            .width(Length::Fill)
            .padding([0, 12])
            .align_y(Alignment::Start)
            .into()
    } else {
        container(content)
            .width(Length::Fill)
            .max_width(840)
            .align_x(Alignment::Start)
            .into()
    }
}

fn settings_page<'a>(
    theme_mode: ThemeMode,
    models: &'a [String],
    selected_model: Option<&'a String>,
    model_status: ModelStatus,
    model_error: Option<&OllamaError>,
    chat_loading: bool,
    latest_release: Option<&'a crate::software_update::Release>,
    update_checking: bool,
    last_successful_update_check: Option<std::time::SystemTime>,
    update_error: Option<&'a str>,
    update_downloading: bool,
    update_download_progress: Option<crate::software_update::DownloadProgress>,
    verified_update: Option<&'a crate::software_update::VerifiedPackage>,
    pending_update_installation_confirmation: bool,
    update_installing: bool,
    update_installation_succeeded: bool,
    colors: Colors,
) -> Element<'a, Message> {
    let theme_card = |mode: ThemeMode, title: &'static str, detail: &'static str| {
        let selected = theme_mode == mode;
        let selected_colors = mode.colors();
        button(
            column![
                text(title).size(15).color(if selected {
                    colors.selected_icon
                } else {
                    colors.foreground
                }),
                text(detail).size(12).color(colors.muted)
            ]
            .spacing(8),
        )
        .width(Length::Fill)
        .padding(16)
        .on_press(Message::SelectTheme(mode))
        .style(move |_, status| {
            let highlighted = selected || matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(Background::Color(if selected {
                    selected_colors.selected_surface
                } else if matches!(status, button::Status::Hovered) {
                    colors.hover_surface
                } else {
                    colors.surface
                })),
                text_color: colors.foreground,
                border: iced::Border {
                    color: if selected {
                        colors.accent
                    } else if highlighted {
                        colors.selected_icon
                    } else {
                        colors.border
                    },
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..button::Style::default()
            }
        })
    };

    let model_selector: Element<'a, Message> = if chat_loading {
        text(selected_model.map_or("No model selected", String::as_str))
            .size(13)
            .color(colors.foreground)
            .into()
    } else {
        pick_list(models, selected_model, Message::SelectModel)
            .placeholder(if model_status == ModelStatus::Loading {
                "Checking connection..."
            } else {
                "Select a model"
            })
            .width(Length::Fill)
            .into()
    };
    let connection_status = match model_status {
        ModelStatus::Loading => "Checking local Ollama connection…",
        ModelStatus::Ready => "Connected. Models are available.",
        ModelStatus::Unavailable => model_error.unwrap_or(&OllamaError::NoModels).user_message(),
    };
    let refresh_enabled = model_status != ModelStatus::Loading && !chat_loading;
    let current_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("Cargo package version must be valid SemVer");
    let has_newer_release = latest_release.is_some_and(|release| {
        crate::software_update::newer_version(&current_version, &release.version)
    });
    let public_key_configured = crate::software_update::embedded_public_key().is_ok();
    let update_status = if update_downloading {
        update_download_progress.map_or_else(
            || "Preparing verified download…".to_owned(),
            |progress| {
                let stage = match progress.stage {
                    crate::software_update::DownloadStage::Checksums => {
                        "Downloading signed checksums"
                    }
                    crate::software_update::DownloadStage::Signature => {
                        "Downloading checksum signature"
                    }
                    crate::software_update::DownloadStage::VerifyingSignature => {
                        "Verifying checksum signature"
                    }
                    crate::software_update::DownloadStage::Package => "Downloading Debian package",
                    crate::software_update::DownloadStage::VerifyingPackage => {
                        "Verifying update package"
                    }
                };
                let amount = progress.total.map_or_else(
                    || format_size(progress.downloaded),
                    |total| {
                        let percent = if total == 0 {
                            0
                        } else {
                            progress.downloaded.saturating_mul(100) / total
                        };
                        format!(
                            "{} / {} ({percent}%)",
                            format_size(progress.downloaded),
                            format_size(total)
                        )
                    },
                );
                format!("{stage}: {amount}")
            },
        )
    } else if update_installing {
        "Installing the verified update through PolicyKit and APT…".to_owned()
    } else if let Some(error) = update_error {
        error.to_owned()
    } else if update_installation_succeeded {
        "Update installed successfully. Restart Adham AI to use the new version.".to_owned()
    } else if verified_update.is_some() {
        "Update downloaded and cryptographically verified. Ready to install.".to_owned()
    } else if update_checking {
        "Checking for updates…".to_owned()
    } else if has_newer_release && !public_key_configured {
        "An update is available, but secure downloads are disabled until the trusted signing key is configured.".to_owned()
    } else if let Some(release) = latest_release {
        if crate::software_update::newer_version(&current_version, &release.version) {
            format!("Version {} is available.", release.version)
        } else {
            format!("You're up to date (latest stable: {}).", release.version)
        }
    } else {
        "Updates have not been checked yet.".to_owned()
    };
    let last_checked = last_successful_update_check.map_or_else(
        || "Never".to_owned(),
        |checked_at| {
            let elapsed = std::time::SystemTime::now()
                .duration_since(checked_at)
                .unwrap_or_default();
            if elapsed.as_secs() < 60 {
                "Just now".to_owned()
            } else if elapsed.as_secs() < 60 * 60 {
                format!("{} minutes ago", elapsed.as_secs() / 60)
            } else if elapsed.as_secs() < 24 * 60 * 60 {
                format!("{} hours ago", elapsed.as_secs() / (60 * 60))
            } else {
                format!("{} days ago", elapsed.as_secs() / (24 * 60 * 60))
            }
        },
    );
    let latest_version = latest_release.map_or_else(
        || "Not checked".to_owned(),
        |release| release.version.to_string(),
    );
    let release_notes = latest_release.map_or(
        "Release notes will appear after a successful check.",
        |release| {
            if release.notes.trim().is_empty() {
                "No release notes were provided."
            } else {
                release.notes.as_str()
            }
        },
    );
    let check_updates = button(
        text(if update_checking {
            "Checking…"
        } else {
            "Check for Updates"
        })
        .size(13),
    )
    .padding([9, 14])
    .on_press_maybe(
        (!update_checking
            && !update_downloading
            && !pending_update_installation_confirmation
            && !update_installing)
            .then_some(Message::CheckForUpdates),
    );
    let open_releases = button(text("Open GitHub Releases").size(13))
        .padding([9, 14])
        .on_press(Message::OpenReleasesPage);
    let can_download_verified_update = has_newer_release
        && !update_checking
        && public_key_configured
        && verified_update.is_none()
        && !update_installing
        && !update_installation_succeeded;
    let download_button = if update_downloading {
        button(text("Cancel Download").size(13))
            .padding([9, 14])
            .on_press(Message::CancelUpdateDownload)
    } else {
        button(text("Download Update").size(13))
            .padding([9, 14])
            .on_press_maybe(can_download_verified_update.then_some(Message::DownloadUpdate))
    };
    let downloaded_path_label: Element<'a, Message> =
        if let Some(package) = verified_update.filter(|_| !update_downloading) {
            text(package.path().display().to_string())
                .size(11)
                .color(colors.muted)
                .into()
        } else {
            text("").size(11).into()
        };
    let installation_action: Element<'a, Message> = if update_installation_succeeded {
        button(text("Restart Adham AI").size(13))
            .padding([9, 14])
            .on_press(Message::RestartUpdatedApplication)
            .into()
    } else if update_installing {
        button(text("Installing…").size(13)).padding([9, 14]).into()
    } else if verified_update.is_some() {
        button(text("Install Update").size(13))
            .padding([9, 14])
            .on_press_maybe((!update_checking).then_some(Message::RequestUpdateInstallation))
            .into()
    } else {
        text("").size(11).into()
    };
    let installation_confirmation: Element<'a, Message> =
        if pending_update_installation_confirmation {
            let version =
                verified_update.map_or_else(String::new, |package| package.version().to_string());
            components::card(
                column![
                    text("Confirm update installation")
                        .size(14)
                        .color(colors.foreground),
                    text(format!(
                        "Install Adham AI {version}? PolicyKit will request administrator authorization. APT may install required dependencies but is prevented from removing packages. Your chats, settings, databases, and model configuration will be preserved."
                    ))
                    .size(12)
                    .color(colors.muted),
                    row![
                        button(text("Confirm Install").size(13))
                            .padding([9, 14])
                            .on_press(Message::ConfirmUpdateInstallation),
                        button(text("Cancel").size(13))
                            .padding([9, 14])
                            .on_press(Message::CancelUpdateInstallation)
                    ]
                    .spacing(8)
                ]
                .spacing(8),
                colors.surface,
                colors.accent,
                16,
            )
            .into()
        } else {
            text("").size(11).into()
        };

    column![
        text("PERSONALIZATION").size(10).color(colors.muted),
        text("Appearance").size(18).color(colors.button_text),
        text("Choose a color scheme for this prototype.")
            .size(13)
            .color(colors.muted),
        row![
            theme_card(
                ThemeMode::Dark,
                "◐  Dark",
                "Pure black and graphite surfaces"
            ),
            theme_card(
                ThemeMode::Light,
                "☼  Light",
                "Soft neutral surfaces and dark text"
            )
        ]
        .spacing(12),
        components::card(
            column![
                text("LOCAL MODEL SERVICE")
                    .size(11)
                    .color(colors.selected_icon),
                text("Endpoint").size(12).color(colors.muted),
                text(crate::ollama::BASE_URL)
                    .size(13)
                    .color(colors.foreground),
                row![
                    text("Model").size(12).color(colors.muted),
                    model_selector,
                    components::icon_button(
                        Icon::Refresh,
                        "Test connection and refresh models",
                        refresh_enabled.then_some(Message::RefreshModels),
                        false,
                        colors,
                    )
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                text(connection_status).size(12).color(if model_status == ModelStatus::Ready {
                    colors.selected_icon
                } else {
                    colors.muted
                })
            ]
            .spacing(8),
            colors.surface,
            colors.border,
            16
        ),
        components::card(
            column![
                text("LOCAL BY DESIGN")
                    .size(11)
                    .color(colors.selected_icon),
                text("Messages are sent only to the local Ollama service. No cloud service is connected.")
                    .size(13)
                    .color(colors.muted)
            ]
            .spacing(8),
            colors.surface,
            colors.border,
            16
        ),
        components::card(
            column![
                text("SOFTWARE UPDATES")
                    .size(11)
                    .color(colors.selected_icon),
                row![
                    text("Current version").size(12).color(colors.muted),
                    iced::widget::Space::new().width(Length::Fill),
                    text(env!("CARGO_PKG_VERSION"))
                        .size(13)
                        .color(colors.foreground)
                ],
                row![
                    text("Latest stable version")
                        .size(12)
                        .color(colors.muted),
                    iced::widget::Space::new().width(Length::Fill),
                    text(latest_version).size(13).color(colors.foreground)
                ],
                row![
                    text("Last successful check")
                        .size(12)
                        .color(colors.muted),
                    iced::widget::Space::new().width(Length::Fill),
                    text(last_checked).size(12).color(colors.foreground)
                ],
                text("Channel: Stable").size(12).color(colors.muted),
                text(update_status)
                    .size(13)
                    .color(if update_error.is_some() {
                        colors.accent
                    } else {
                        colors.foreground
                    }),
                downloaded_path_label,
                installation_action,
                installation_confirmation,
                if let Some(release) = latest_release {
                    text(format!(
                        "Published: {}",
                        release.published_at.as_deref().unwrap_or("Unknown")
                    ))
                    .size(11)
                    .color(colors.muted)
                } else {
                    text("").size(11)
                },
                text("Release notes").size(12).color(colors.muted),
                scrollable(
                    text(release_notes)
                        .size(12)
                        .color(colors.foreground)
                        .width(Length::Fill)
                )
                .height(Length::Fixed(110.0)),
                row![check_updates, download_button, open_releases].spacing(8)
            ]
            .spacing(8),
            colors.surface,
            colors.border,
            16
        )
    ]
    .spacing(12)
    .into()
}

#[cfg(test)]
mod tests {
    use super::deep_think_status;
    use crate::app::ChatResponseStatus;

    #[test]
    fn deep_think_status_reflects_received_data_and_response_outcome() {
        assert_eq!(
            deep_think_status(true, false, false, Some(ChatResponseStatus::Generating)),
            None
        );
        assert_eq!(
            deep_think_status(true, false, true, Some(ChatResponseStatus::Generating)),
            Some("Thinking...")
        );
        assert_eq!(
            deep_think_status(true, false, true, Some(ChatResponseStatus::Completed)),
            Some("Deep Think completed")
        );
        assert_eq!(
            deep_think_status(true, false, false, Some(ChatResponseStatus::Completed)),
            Some("No thinking data received")
        );
        assert_eq!(
            deep_think_status(true, false, true, Some(ChatResponseStatus::Cancelled)),
            Some("Deep Think stopped")
        );
        assert_eq!(
            deep_think_status(true, false, true, Some(ChatResponseStatus::Failed)),
            Some("Deep Think failed")
        );
        assert_eq!(
            deep_think_status(false, false, true, Some(ChatResponseStatus::Completed)),
            None
        );
        assert_eq!(
            deep_think_status(true, true, true, Some(ChatResponseStatus::Completed)),
            None
        );
    }
}
