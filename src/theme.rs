use iced::{Color, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ThemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Colors {
    pub(super) background: Color,
    pub(super) surface: Color,
    pub(super) foreground: Color,
    pub(super) muted: Color,
    pub(super) border: Color,
    pub(super) accent: Color,
    pub(super) icon: Color,
    pub(super) selected_icon: Color,
    pub(super) selected_surface: Color,
    pub(super) hover_surface: Color,
    pub(super) button_text: Color,
}

impl ThemeMode {
    pub(super) fn colors(self) -> Colors {
        match self {
            Self::Dark => Colors {
                background: Color::BLACK,
                surface: Color::from_rgb8(0x12, 0x11, 0x15),
                foreground: Color::from_rgb8(0xE8, 0xE3, 0xEE),
                muted: Color::from_rgb8(0x99, 0x92, 0xA2),
                border: Color::from_rgb8(0x2E, 0x2A, 0x34),
                accent: Color::from_rgb8(0x8B, 0x5C, 0xF6),
                icon: Color::from_rgb8(0xA8, 0x55, 0xF7),
                selected_icon: Color::from_rgb8(0xC4, 0xB5, 0xFD),
                selected_surface: Color::from_rgb8(0x21, 0x16, 0x2D),
                hover_surface: Color::from_rgb8(0x24, 0x20, 0x2B),
                button_text: Color::from_rgb8(0x0D, 0x07, 0x18),
            },
            Self::Light => Colors {
                background: Color::from_rgb8(0xF4, 0xF2, 0xF7),
                surface: Color::WHITE,
                foreground: Color::from_rgb8(0x24, 0x20, 0x2B),
                muted: Color::from_rgb8(0x79, 0x73, 0x81),
                border: Color::from_rgb8(0xE4, 0xDF, 0xE9),
                accent: Color::from_rgb8(0x6D, 0x28, 0xD9),
                icon: Color::from_rgb8(0x7E, 0x22, 0xCE),
                selected_icon: Color::from_rgb8(0x6D, 0x28, 0xD9),
                selected_surface: Color::from_rgb8(0xF0, 0xE9, 0xFF),
                hover_surface: Color::from_rgb8(0xF0, 0xED, 0xF3),
                button_text: Color::WHITE,
            },
        }
    }

    pub(super) fn iced_theme(self) -> Theme {
        let colors = self.colors();
        Theme::custom(
            "Adham AI",
            iced::theme::Palette {
                background: colors.background,
                text: colors.foreground,
                primary: colors.accent,
                success: Color::from_rgb8(0x4A, 0x9B, 0x78),
                warning: Color::from_rgb8(0xD6, 0xA5, 0x52),
                danger: Color::from_rgb8(0xD9, 0x65, 0x65),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ThemeMode;
    use iced::Color;

    #[test]
    fn dark_palette_keeps_black_background_and_visible_accents() {
        let colors = ThemeMode::Dark.colors();

        assert_eq!(colors.background, Color::BLACK);
        assert_eq!(colors.icon, Color::from_rgb8(0xA8, 0x55, 0xF7));
        assert_eq!(colors.accent, Color::from_rgb8(0x8B, 0x5C, 0xF6));
        assert_eq!(colors.selected_icon, Color::from_rgb8(0xC4, 0xB5, 0xFD));
        assert_eq!(colors.button_text, Color::from_rgb8(0x0D, 0x07, 0x18));
    }

    #[test]
    fn light_palette_uses_darker_violet_accents() {
        let colors = ThemeMode::Light.colors();

        assert_eq!(colors.background, Color::from_rgb8(0xF4, 0xF2, 0xF7));
        assert_eq!(colors.icon, Color::from_rgb8(0x7E, 0x22, 0xCE));
        assert_eq!(colors.accent, Color::from_rgb8(0x6D, 0x28, 0xD9));
    }
}
