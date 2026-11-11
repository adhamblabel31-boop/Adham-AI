mod app;
mod components;
mod documents;
mod history;
mod icons;
mod markdown;
mod media;
mod media_process;
mod ollama;
mod pages;
mod software_update;
mod theme;
mod update;

use app::App;
use iced::application;

fn main() -> iced::Result {
    application(App::new, App::update, App::view)
        .title(|_: &App| "Adham AI".to_owned())
        .theme(App::theme)
        .subscription(App::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1100.0, 720.0),
            min_size: Some(iced::Size::new(760.0, 520.0)),
            ..iced::window::Settings::default()
        })
        .run()
}
