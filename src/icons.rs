use iced::widget::svg;
use iced::{Color, Element, Length};

use crate::app::Message;

#[derive(Debug, Clone, Copy)]
pub(super) enum Icon {
    ArrowUp,
    ArrowDown,
    Square,
    Refresh,
    Chat,
    FileText,
    Search,
    Sparkles,
    Settings,
    SidebarCollapse,
    SidebarExpand,
    Plus,
    Check,
    Edit,
    Trash,
    Close,
}

pub(super) fn view(icon: Icon, color: Color, size: f32) -> Element<'static, Message> {
    let path = match icon {
        Icon::ArrowUp => r#"<path d="M12 19V5M5 12l7-7 7 7"/>"#,
        Icon::ArrowDown => r#"<path d="M12 5v14m7-7-7 7-7-7"/>"#,
        Icon::Square => r#"<rect x="5" y="5" width="14" height="14" rx="2"/>"#,
        Icon::Refresh => {
            r#"<path d="M20 7v5h-5M4 17v-5h5"/><path d="M5.6 9a7 7 0 0 1 11.55-2.6L20 12M4 12l2.85 5.6A7 7 0 0 0 18.4 15"/>"#
        }
        Icon::Chat => {
            r#"<path d="M21 11.5a8.4 8.4 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.4 8.4 0 0 1-3.8-.9L3 21l1.9-5.7a8.4 8.4 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.4 8.4 0 0 1 3.8-.9h.5a8.5 8.5 0 0 1 8 8z"/>"#
        }
        Icon::FileText => {
            r#"<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6M8 13h8M8 17h8"/>"#
        }
        Icon::Search => r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.35-4.35"/>"#,
        Icon::Sparkles => {
            r#"<path d="m12 3 1.9 5.8L20 11l-6.1 2.2L12 19l-1.9-5.8L4 11l6.1-2.2L12 3z"/><path d="m19 14 1.2 2.8L23 18l-2.8 1.2L19 22l-1.2-2.8L15 18l2.8-1.2L19 14z"/><path d="m5 2 .9 2.1L8 5l-2.1.9L5 8l-.9-2.1L2 5l2.1-.9L5 2z"/>"#
        }
        Icon::Settings => {
            r#"<path d="M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z"/><path d="m19.4 15 .1.1 1.4 1.1-1.4 2.4-1.8-.6a8 8 0 0 1-1.5.9l-.4 1.9h-2.8l-.4-1.9a8 8 0 0 1-1.5-.9l-1.8.6-1.4-2.4 1.4-1.1a7 7 0 0 1 0-1.8l-1.4-1.1 1.4-2.4 1.8.6a8 8 0 0 1 1.5-.9l.4-1.9h2.8l.4 1.9a8 8 0 0 1 1.5.9l1.8-.6 1.4 2.4-1.4 1.1a7 7 0 0 1 0 1.8z"/>"#
        }
        Icon::SidebarCollapse => {
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16m7-11-3 3 3 3"/>"#
        }
        Icon::SidebarExpand => {
            r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16m3-8 3-3m-3 3 3 3"/>"#
        }
        Icon::Plus => r#"<path d="M12 5v14M5 12h14"/>"#,
        Icon::Check => r#"<path d="m5 12 4 4L19 6"/>"#,
        Icon::Edit => {
            r#"<path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L8 18l-4 1 1-4Z"/>"#
        }
        Icon::Trash => r#"<path d="M3 6h18M8 6V4h8v2m3 0-1 14H6L5 6m4 4v6m6-6v6"/>"#,
        Icon::Close => r#"<path d="m18 6-12 12M6 6l12 12"/>"#,
    };
    let color = format!(
        "#{:02X}{:02X}{:02X}",
        (color.r * 255.0).round() as u8,
        (color.g * 255.0).round() as u8,
        (color.b * 255.0).round() as u8,
    );
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="{color}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{path}</svg>"#
    );

    svg::Svg::new(svg::Handle::from_memory(svg.into_bytes()))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}
