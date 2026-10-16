use iced::widget::{button, column, container, image, markdown, row, scrollable, text, text_editor};
use iced::{Element, Length, Subscription, Task};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct App {
    vault: vaultsel::Vault,
    picks: Vec<usize>,
    md: Vec<markdown::Item>,
    editor: text_editor::Content,
    start: Instant,
    next_auto: usize,
    auto: bool,
    paste_done: bool,
    paste: bool,
}

#[derive(Debug, Clone)]
enum Msg { Open(usize), Edit(text_editor::Action), Tick, Link(String) }

struct Viewer;
impl<'a> markdown::Viewer<'a, Msg> for Viewer {
    fn on_link_click(url: String) -> Msg { Msg::Link(url) }
    fn image(&self, _s: markdown::Settings, url: &'a String, _t: &'a str, _a: &markdown::Text) -> Element<'a, Msg> {
        let p = url.strip_prefix("file://").unwrap_or(url).replace("%20", " ");
        image(image::Handle::from_path(p)).width(Length::Fill).into()
    }
}

fn boot() -> (App, Task<Msg>) {
    let t = Instant::now();
    let vault = vaultsel::load(&vaultsel::vault_root());
    let picks = vaultsel::pick(&vault, 20);
    eprintln!("[spike] load+pick {:?} {}", t.elapsed(), vaultsel::describe(&vault, &picks));
    (App { vault, picks, md: vec![], editor: text_editor::Content::new(), start: Instant::now(), next_auto: 0,
        auto: std::env::var("SPIKE_AUTO").is_ok(), paste: std::env::var("SPIKE_PASTE").is_ok(), paste_done: false }, Task::none())
}

fn schedule(k: usize) -> Duration { if k == 0 { Duration::from_secs(6) } else { Duration::from_secs(15 + k as u64 - 1) } }

impl App {
    fn open(&mut self, i: usize) {
        let src = std::fs::read_to_string(&self.vault.notes[i]).unwrap_or_default();
        let rewritten = vaultsel::rewrite_embeds(&self.vault, &src, "file://");
        self.md = markdown::parse(&rewritten).collect();
        self.editor = text_editor::Content::with_text(&src);
    }
}

fn update(a: &mut App, m: Msg) {
    match m {
        Msg::Open(i) => a.open(i),
        Msg::Edit(act) => a.editor.perform(act),
        Msg::Link(_) => {}
        Msg::Tick => {
            let el = a.start.elapsed();
            while a.auto && a.next_auto < a.picks.len() && el >= schedule(a.next_auto) {
                let i = a.picks[a.next_auto];
                let t = Instant::now();
                a.open(i);
                eprintln!("[spike] opened #{} at {:?} parse {:?}", a.next_auto + 1, el, t.elapsed());
                a.next_auto += 1;
            }
            if a.paste && !a.paste_done && el >= Duration::from_secs(40) {
                let big: String = "lorem ipsum dolor sit amet ".repeat(1_048_576 / 27);
                let t = Instant::now();
                a.editor.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                a.editor.perform(text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(big))));
                eprintln!("[spike] paste 1MB perform {:?} lines={}", t.elapsed(), a.editor.line_count());
                a.paste_done = true;
            }
        }
    }
}

fn view(a: &App) -> Element<'_, Msg> {
    let list = scrollable(column(a.vault.notes.iter().enumerate().map(|(i, _)| {
        button(text(format!("note {i}")).size(12)).on_press(Msg::Open(i)).padding(1).into()
    }))).width(260);
    let md = scrollable(container(markdown::view_with(&a.md, markdown::Settings::with_style(iced::Theme::Light), &Viewer)).padding(8)).width(Length::FillPortion(1));
    let ed = text_editor(&a.editor).on_action(Msg::Edit).height(Length::Fill);
    row![list, md, container(ed).width(Length::FillPortion(1))].into()
}

fn subscription(a: &App) -> Subscription<Msg> {
    let pending = (a.auto && a.next_auto < a.picks.len()) || (a.paste && !a.paste_done);
    if pending { iced::time::every(Duration::from_millis(100)).map(|_| Msg::Tick) } else { Subscription::none() }
}

fn main() -> iced::Result {
    iced::application(boot, update, view).title("spike-iced").subscription(subscription).window_size((1400.0, 900.0)).run()
}
