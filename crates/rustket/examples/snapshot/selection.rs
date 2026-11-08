//! Text selection: posts a message with a paragraph and a code block to the
//! open dev channel, drags across both with real mouse events in one
//! persistent `UserInterface` and shoots light and dark (the selection lives
//! in the widget tree, so the theme switch rebuilds the view from the same
//! cache). The paragraph's hidden inline spoiler stays covered and is not
//! copied. Prints what Ctrl+C would copy.

use std::path::Path;
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless as _;
use iced::advanced::widget::{Id, Operation, operation::Scrollable};
use iced::{Event, Pixels, Point, Rectangle, Size, Vector, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rocketchat::ServerUrl;
use rustket_core::ServerId;
use rustket_ui::widget::selectable::Copied;
use serde_json::{Value, json};

use super::Driver;

const PARAGRAPH: &str = "Selectable text: drag from this paragraph into the code block below";
const CODE: &str = "fn main() {\n    println!(\"copied as plain text\");\n}";

/// Text widgets and their on-screen bounds (scrollable offsets applied).
#[derive(Default)]
struct Finder {
    stack: Vec<Vector>,
    pending: Vector,
    found: Vec<(Rectangle, String)>,
}

impl Operation for Finder {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        let base = self.stack.last().copied().unwrap_or(Vector::ZERO);
        let t = base + std::mem::replace(&mut self.pending, Vector::ZERO);
        self.stack.push(t);
        operate(self);
        self.stack.pop();
    }

    fn scrollable(
        &mut self,
        _: Option<&Id>,
        _: Rectangle,
        _: Rectangle,
        translation: Vector,
        _: &mut dyn Scrollable,
    ) {
        self.pending = translation;
    }

    fn text(&mut self, _: Option<&Id>, bounds: Rectangle, text: &str) {
        let t = self.stack.last().copied().unwrap_or(Vector::ZERO);
        self.found.push((bounds - t, text.to_owned()));
    }
}

fn renderer() -> iced::Renderer {
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::DEFAULT_FONT,
        Pixels(rustket_ui::theme::TEXT_BODY),
    ))
}

fn draw(
    d: &Driver,
    ui: &mut UserInterface<'_, rustket::message::Message, iced::Theme, iced::Renderer>,
    renderer: &mut iced::Renderer,
    cursor: mouse::Cursor,
    dir: &Path,
    name: &str,
    size: Size,
) {
    let theme = d.app.theme();
    let t = rustket_ui::tokens(&theme);
    ui.draw(
        renderer,
        &theme,
        &iced::advanced::renderer::Style {
            text_color: t.text_primary,
        },
        cursor,
    );
    let rgba = renderer.screenshot(
        Size::new(size.width as u32, size.height as u32),
        1.0,
        t.surface_room,
    );
    let path = dir.join(format!("{name}.png"));
    super::write_png(&path, size.width as u32, size.height as u32, &rgba);
    println!("wrote {}", path.display());
}

pub async fn selection_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    rid: &str,
) {
    let creds = d.app.servers[sid].credentials.clone().expect("creds");
    let rest = super::rest(server).await;
    rest.set_credentials(Some(creds));
    let body =
        format!("{PARAGRAPH}, past a ||hidden spoiler|| then copy it.\n```rust\n{CODE}\n```");
    let v: Value = rest
        .post("chat.postMessage", &json!({"roomId": rid, "text": body}))
        .await
        .expect("post");
    let mid = v["message"]["_id"].as_str().expect("id").to_owned();
    d.pump_until("selection message shown", Duration::from_secs(15), |a| {
        a.servers[sid]
            .workspace
            .timeline(rid)
            .is_some_and(|t| t.iter().any(|i| i.message.id == mid))
    })
    .await;
    d.settle(Duration::from_millis(500)).await;

    let size = super::shot_size();
    let mut renderer = renderer();
    let mut ui = UserInterface::build(d.app.view(), size, Cache::default(), &mut renderer);
    let mut msgs = Vec::new();
    let mut step = |ui: &mut UserInterface<'_, _, _, _>, r: &mut iced::Renderer, e: Event, at| {
        let _ = ui.update(
            &[e],
            mouse::Cursor::Available(at),
            r,
            &mut super::NoClip,
            &mut msgs,
        );
    };
    step(
        &mut ui,
        &mut renderer,
        Event::Window(iced::window::Event::RedrawRequested(Instant::now())),
        Point::new(-10.0, -10.0),
    );
    let mut finder = Finder::default();
    ui.operate(&renderer, &mut finder);
    let find = |needle: &str| {
        finder
            .found
            .iter()
            .rev()
            .find(|(_, t)| t.starts_with(needle))
            .map(|(b, _)| *b)
            .unwrap_or_else(|| panic!("{needle:?} not on screen"))
    };
    let para = find(PARAGRAPH);
    let code = find("fn main()");
    let from = Point::new(para.x + 110.0, para.y + 8.0);
    let to = Point::new(code.x + 120.0, code.y + code.height * 0.5);
    println!("selection drag {from:?} -> {to:?}");
    let mv = |p: Point| Event::Mouse(mouse::Event::CursorMoved { position: p });
    step(&mut ui, &mut renderer, mv(from), from);
    step(
        &mut ui,
        &mut renderer,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        from,
    );
    let mid_point = Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
    step(&mut ui, &mut renderer, mv(mid_point), mid_point);
    step(&mut ui, &mut renderer, mv(to), to);
    step(
        &mut ui,
        &mut renderer,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        to,
    );
    let mut copied = Copied::default();
    ui.operate(&renderer, &mut copied);
    let text: Vec<_> = copied.texts.iter().filter(|t| !t.is_empty()).collect();
    println!("selection copies {text:?}");
    assert_eq!(text.len(), 1, "one message selected");
    assert!(text[0].contains("paragraph") && text[0].contains("fn main"));
    assert!(
        !text[0].contains("hidden spoiler"),
        "a hidden spoiler is copied"
    );
    let cursor = mouse::Cursor::Available(to);
    draw(
        d,
        &mut ui,
        &mut renderer,
        cursor,
        out,
        "selection-light",
        size,
    );
    let cache = ui.into_cache();

    let theme = d.app.config.ui.theme;
    d.app.config.ui.theme = rustket_core::config::Theme::Dark;
    let mut renderer2 = self::renderer();
    let mut ui = UserInterface::build(d.app.view(), size, cache, &mut renderer2);
    let _ = ui.update(
        &[Event::Window(iced::window::Event::RedrawRequested(
            Instant::now(),
        ))],
        cursor,
        &mut renderer2,
        &mut super::NoClip,
        &mut Vec::new(),
    );
    draw(
        d,
        &mut ui,
        &mut renderer2,
        cursor,
        out,
        "selection-dark",
        size,
    );
    drop(ui);
    d.app.config.ui.theme = theme;
}
