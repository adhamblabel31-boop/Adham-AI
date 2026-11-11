use iced::widget::{Space, button, column, container, row, scrollable, text, text_input, tooltip};
use iced::{Alignment, Background, Color, Element, Length};
use std::time::Duration;

use crate::app::{Message, Page};
use crate::history::Conversation;
use crate::icons::{self, Icon};
use crate::theme::Colors;

pub(super) fn application_shell<'a>(
    sidebar: Element<'a, Message>,
    header: Element<'a, Message>,
    content: Element<'a, Message>,
    colors: Colors,
) -> Element<'a, Message> {
    let main = container(
        column![
            header,
            container(content).height(Length::Fill).padding([14.0, 0.0])
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(20)
    .style(move |_| container::Style {
        background: Some(Background::Color(colors.background)),
        ..container::Style::default()
    });

    row![sidebar, main].height(Length::Fill).into()
}

pub(super) fn sidebar<'a>(
    collapsed: bool,
    active_page: Page,
    conversations: &'a [Conversation],
    selected_conversation_id: Option<&str>,
    pending_delete_conversation: Option<&str>,
    deleting_conversation: Option<&str>,
    editing_conversation_id: Option<&str>,
    conversation_title_draft: &'a str,
    saving_conversation_id: Option<&str>,
    rename_enabled: bool,
    history_error: Option<&'a str>,
    colors: Colors,
) -> Element<'a, Message> {
    let navigation = Page::ALL
        .into_iter()
        .filter(|(page, _)| *page != Page::Settings)
        .fold(column![].spacing(8), |items, (page, label)| {
            items.push(navigation_item(page, label, active_page, collapsed, colors))
        });
    let conversation_list =
        conversations
            .iter()
            .fold(column![].spacing(4), |items, conversation| {
                let selected = selected_conversation_id == Some(conversation.id.as_str());
                let editing = editing_conversation_id == Some(conversation.id.as_str());
                let saving = saving_conversation_id == Some(conversation.id.as_str());
                let title: Element<'a, Message> = if editing {
                    text_input("Conversation title", conversation_title_draft)
                        .on_input(Message::RenameTitleEdited)
                        .on_submit(Message::CommitRenameConversation)
                        .width(Length::Fill)
                        .padding([8, 7])
                        .size(12)
                        .into()
                } else {
                    let display_title = truncate_sidebar_title(&conversation.title);
                    button(text(display_title).size(12).color(if selected {
                        colors.foreground
                    } else {
                        colors.muted
                    }))
                    .width(Length::Fill)
                    .padding([8, 6])
                    .on_press(Message::OpenConversation(conversation.id.clone()))
                    .style(move |_, status| {
                        let highlighted = selected || matches!(status, button::Status::Hovered);
                        button::Style {
                            background: Some(Background::Color(if selected {
                                colors.selected_surface
                            } else if matches!(status, button::Status::Hovered) {
                                colors.hover_surface
                            } else {
                                Color::TRANSPARENT
                            })),
                            border: iced::Border {
                                color: if highlighted {
                                    colors.border
                                } else {
                                    Color::TRANSPARENT
                                },
                                width: 1.0,
                                radius: 7.0.into(),
                            },
                            ..button::Style::default()
                        }
                    })
                    .into()
                };
                let actions: Element<'a, Message> = if editing {
                    row![
                        compact_icon_button(
                            Icon::Check,
                            "Save conversation name",
                            (!saving).then_some(Message::CommitRenameConversation),
                            true,
                            colors,
                        ),
                        compact_icon_button(
                            Icon::Close,
                            "Cancel rename",
                            (!saving).then_some(Message::CancelRenameConversation),
                            false,
                            colors,
                        )
                    ]
                    .spacing(2)
                    .into()
                } else {
                    row![
                        compact_icon_button(
                            Icon::Edit,
                            "Rename conversation",
                            rename_enabled
                                .then(|| Message::BeginRenameConversation(conversation.id.clone())),
                            false,
                            colors,
                        ),
                        compact_icon_button(
                            Icon::Trash,
                            "Delete conversation",
                            Some(Message::RequestDeleteConversation(conversation.id.clone())),
                            false,
                            colors,
                        )
                    ]
                    .spacing(2)
                    .into()
                };
                items.push(row![title, actions].spacing(2).align_y(Alignment::Center))
            });

    let confirmation = pending_delete_conversation
        .and_then(|id| {
            conversations
                .iter()
                .find(|conversation| conversation.id == id)
        })
        .map(|conversation| {
            let deleting = deleting_conversation == Some(conversation.id.as_str());
            container(
                column![
                    text(if deleting {
                        format!("Deleting “{}”…", conversation.title)
                    } else {
                        format!("Delete “{}”?", conversation.title)
                    })
                    .size(12)
                    .color(colors.foreground),
                    row![
                        icon_button(
                            Icon::Trash,
                            "Confirm delete conversation",
                            (!deleting).then_some(Message::ConfirmDeleteConversation),
                            true,
                            colors,
                        ),
                        icon_button(
                            Icon::Close,
                            "Cancel deletion",
                            (!deleting).then_some(Message::CancelDeleteConversation),
                            false,
                            colors,
                        )
                    ]
                    .spacing(6)
                ]
                .spacing(6),
            )
            .padding(8)
            .style(move |_| container::Style {
                background: Some(Background::Color(colors.hover_surface)),
                border: iced::Border {
                    color: colors.border,
                    width: 1.0,
                    radius: 7.0.into(),
                },
                ..container::Style::default()
            })
        });

    let brand: Element<'_, Message> = if collapsed {
        Space::new().width(Length::Shrink).into()
    } else {
        row![
            container(text("A").size(18).color(colors.selected_icon))
                .padding([7, 11])
                .style(move |_| container::Style {
                    background: Some(Background::Color(colors.selected_surface)),
                    ..container::Style::default()
                }),
            column![
                text("Adham AI").size(16).color(colors.foreground),
                text("LOCAL WORKSPACE").size(9).color(colors.muted)
            ]
            .spacing(2)
        ]
        .spacing(11)
        .align_y(Alignment::Center)
        .into()
    };
    let sidebar_toggle = icon_button(
        if collapsed {
            Icon::SidebarExpand
        } else {
            Icon::SidebarCollapse
        },
        if collapsed {
            "Expand sidebar"
        } else {
            "Collapse sidebar"
        },
        Some(Message::ToggleSidebar),
        false,
        colors,
    );
    let header: Element<'_, Message> = if collapsed {
        row![Space::new().width(Length::Fill), sidebar_toggle]
            .align_y(Alignment::Center)
            .into()
    } else {
        column![
            brand,
            row![Space::new().width(Length::Fill), sidebar_toggle].align_y(Alignment::Center)
        ]
        .spacing(4)
        .into()
    };
    let settings = navigation_item(Page::Settings, "Settings", active_page, collapsed, colors);

    let content: Element<'_, Message> = if collapsed {
        column![
            header,
            container(navigation).padding([18, 0]),
            icon_button(
                Icon::Plus,
                "New chat",
                Some(Message::StartNewConversation),
                false,
                colors,
            ),
            Space::new().height(Length::Fill),
            settings
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    } else {
        column![
            header,
            column![text("WORKSPACE").size(11).color(colors.muted), navigation]
                .spacing(8)
                .padding([20, 0]),
            row![
                text("CHATS").size(11).color(colors.muted),
                Space::new().width(Length::Fill),
                icon_button(
                    Icon::Plus,
                    "New chat",
                    Some(Message::StartNewConversation),
                    false,
                    colors,
                )
            ]
            .align_y(Alignment::Center),
            scrollable(conversation_list).height(Length::Fill),
            if let Some(confirmation) = confirmation {
                confirmation
            } else {
                container(Space::new().height(Length::Shrink))
            },
            if let Some(error) = history_error {
                text(error).size(11).color(colors.accent)
            } else {
                text("").size(11)
            },
            settings
        ]
        .spacing(7)
        .height(Length::Fill)
        .into()
    };

    container(content)
        .width(if collapsed { 72 } else { 185 })
        .height(Length::Fill)
        .padding(if collapsed { 8 } else { 18 })
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.surface)),
            ..container::Style::default()
        })
        .into()
}

fn truncate_sidebar_title(title: &str) -> String {
    const MAX_DISPLAY_CHARS: usize = 12;
    let mut chars = title.chars();
    let visible = chars
        .by_ref()
        .take(MAX_DISPLAY_CHARS - 1)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{visible}…")
    } else {
        title.to_owned()
    }
}

fn navigation_item(
    page: Page,
    label: &'static str,
    active_page: Page,
    collapsed: bool,
    colors: Colors,
) -> Element<'static, Message> {
    let selected = active_page == page;
    let icon_color = if selected {
        colors.selected_icon
    } else {
        colors.icon
    };

    let icon = match page {
        Page::Chat => Icon::Chat,
        Page::Documents => Icon::FileText,
        Page::MediaLab => Icon::Sparkles,
        Page::Settings => Icon::Settings,
    };

    let content: Element<'static, Message> = if collapsed {
        container(icons::view(icon, icon_color, 18.0))
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .into()
    } else {
        row![
            icons::view(icon, icon_color, 18.0),
            text(label).size(14).color(if selected {
                colors.foreground
            } else {
                colors.muted
            })
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
    };
    let control = button(content)
        .width(Length::Fill)
        .padding(11)
        .on_press(Message::Navigate(page))
        .style(move |_, status| {
            let highlighted = selected || matches!(status, button::Status::Hovered);
            button::Style {
                background: Some(Background::Color(if selected {
                    colors.selected_surface
                } else if matches!(status, button::Status::Hovered) {
                    colors.hover_surface
                } else {
                    Color::TRANSPARENT
                })),
                text_color: colors.foreground,
                border: iced::Border {
                    color: if selected {
                        colors.accent
                    } else if highlighted {
                        colors.selected_icon
                    } else {
                        Color::TRANSPARENT
                    },
                    width: if highlighted { 1.0 } else { 0.0 },
                    radius: 7.0.into(),
                },
                ..button::Style::default()
            }
        });

    if collapsed {
        tooltip::Tooltip::new(control, text(label).size(12), tooltip::Position::Right)
            .delay(Duration::from_millis(350))
            .into()
    } else {
        control.into()
    }
}

pub(super) fn icon_button(
    icon: Icon,
    label: &'static str,
    message: Option<Message>,
    accent: bool,
    colors: Colors,
) -> Element<'static, Message> {
    icon_button_with_size(icon, label, message, accent, colors, 40, 18.0)
}

fn compact_icon_button(
    icon: Icon,
    label: &'static str,
    message: Option<Message>,
    accent: bool,
    colors: Colors,
) -> Element<'static, Message> {
    icon_button_with_size(icon, label, message, accent, colors, 32, 16.0)
}

fn icon_button_with_size(
    icon: Icon,
    label: &'static str,
    message: Option<Message>,
    accent: bool,
    colors: Colors,
    size: u32,
    icon_size: f32,
) -> Element<'static, Message> {
    let icon_color = if message.is_none() {
        colors.muted
    } else if accent {
        colors.button_text
    } else {
        colors.selected_icon
    };
    let control = button(icons::view(icon, icon_color, icon_size))
        .width(size)
        .height(size)
        .padding((size as f32 - icon_size) / 2.0)
        .on_press_maybe(message)
        .style(move |_, status| {
            let (background, border) = match status {
                button::Status::Disabled => (colors.surface, colors.border),
                button::Status::Hovered => (colors.hover_surface, colors.selected_icon),
                button::Status::Pressed => (colors.selected_surface, colors.accent),
                button::Status::Active if accent => (colors.accent, colors.accent),
                button::Status::Active => (Color::TRANSPARENT, colors.border),
            };

            button::Style {
                background: Some(Background::Color(background)),
                text_color: colors.foreground,
                border: iced::Border {
                    color: border,
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..button::Style::default()
            }
        });

    tooltip::Tooltip::new(control, text(label).size(12), tooltip::Position::Top)
        .delay(Duration::from_millis(350))
        .into()
}

pub(super) fn page_header(title: &'static str, colors: Colors) -> Element<'static, Message> {
    row![
        column![
            text("ADHAM AI  /  WORKSPACE").size(10).color(colors.muted),
            text(title).size(25).color(colors.foreground)
        ]
        .spacing(5),
        Space::new().width(Length::Fill)
    ]
    .align_y(Alignment::Center)
    .into()
}

pub(super) fn card<'a>(
    content: impl Into<Element<'a, Message>>,
    fill: Color,
    border: Color,
    padding: u16,
) -> Element<'a, Message> {
    container(content)
        .width(Length::Fill)
        .padding(padding)
        .style(move |_| container::Style {
            background: Some(Background::Color(fill)),
            border: iced::Border {
                color: border,
                width: 1.0,
                radius: 8.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::truncate_sidebar_title;

    #[test]
    fn sidebar_title_truncation_is_display_only_and_unicode_safe() {
        assert_eq!(truncate_sidebar_title("Short title"), "Short title");
        let long_title = "界".repeat(30);
        let shown = truncate_sidebar_title(&long_title);
        assert_eq!(shown.chars().count(), 12);
        assert!(shown.ends_with('…'));
        assert_eq!(long_title.chars().count(), 30);
    }
}
