//! Headless tests: real layout and events through `UserInterface` with the
//! tiny-skia renderer; positions come from the laid-out blocks, so they do not
//! depend on font metrics.

#![allow(clippy::unwrap_used)]

use std::ops::Range;

use iced::advanced::Clipboard;
use iced::advanced::clipboard::Kind;
use iced::advanced::widget::{Id, Operation};
use iced::event::Status;
use iced::keyboard::{self, Modifiers, key};
use iced::widget::{column, space, span};
use iced::{Element, Event, Font, Length, Pixels, Point, Rectangle, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};

use super::core::Core;
use super::{rich, scope};
use crate::widget::context_menu;
use crate::widget::{MenuEntry, MenuItem};

type El = Element<'static, Msg>;

#[derive(Debug, Clone, PartialEq)]
enum Msg {
    Link(String),
    Menu,
}

#[derive(Default)]
struct Clip(Option<String>);

impl Clipboard for Clip {
    fn read(&self, _: Kind) -> Option<String> {
        self.0.clone()
    }
    fn write(&mut self, _: Kind, contents: String) {
        self.0 = Some(contents);
    }
}

/// One block per entry.
fn blocks(texts: &[&'static str]) -> El {
    let mut col = column![].spacing(8).width(Length::Fill);
    for t in texts {
        col = col.push(
            rich::<String, Msg>(vec![span(*t)])
                .on_link_click(Msg::Link)
                .size(14)
                .width(Length::Fill),
        );
    }
    col.into()
}

struct Harness {
    view: Box<dyn Fn() -> El>,
    cache: Option<Cache>,
    renderer: iced::Renderer,
    clip: Clip,
    msgs: Vec<Msg>,
    cursor: Point,
    interaction: mouse::Interaction,
}

impl Harness {
    fn new(view: impl Fn() -> El + 'static) -> Self {
        let mut h = Self {
            view: Box::new(view),
            cache: Some(Cache::default()),
            renderer: iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                Font::DEFAULT,
                Pixels(14.0),
            )),
            clip: Clip::default(),
            msgs: Vec::new(),
            cursor: Point::new(-100.0, -100.0),
            interaction: mouse::Interaction::None,
        };
        h.events(&[]);
        h
    }

    fn with_ui<T>(
        &mut self,
        f: impl FnOnce(&mut UserInterface<'_, Msg, iced::Theme, iced::Renderer>, &mut Self) -> T,
    ) -> T {
        let cache = self.cache.take().unwrap_or_default();
        let mut ui = UserInterface::build(
            (self.view)(),
            Size::new(400.0, 600.0),
            cache,
            &mut self.renderer,
        );
        let out = f(&mut ui, self);
        self.cache = Some(ui.into_cache());
        out
    }

    fn events(&mut self, events: &[Event]) -> Vec<Status> {
        self.with_ui(|ui, h| {
            let (state, status) = ui.update(
                events,
                mouse::Cursor::Available(h.cursor),
                &mut h.renderer,
                &mut h.clip,
                &mut h.msgs,
            );
            if let iced_runtime::user_interface::State::Updated {
                mouse_interaction, ..
            } = state
            {
                h.interaction = mouse_interaction;
            }
            status
        })
    }

    fn move_to(&mut self, p: Point) {
        self.cursor = p;
        self.events(&[Event::Mouse(mouse::Event::CursorMoved { position: p })]);
    }

    fn press(&mut self, button: mouse::Button) -> Status {
        self.events(&[Event::Mouse(mouse::Event::ButtonPressed(button))])[0]
    }

    fn release(&mut self) -> Status {
        self.events(&[Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        ))])[0]
    }

    fn click(&mut self, p: Point) {
        self.move_to(p);
        self.press(mouse::Button::Left);
        self.release();
    }

    fn drag(&mut self, from: Point, to: Point) -> Status {
        self.move_to(from);
        self.press(mouse::Button::Left);
        let mid = Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
        self.move_to(mid);
        self.move_to(to);
        self.release()
    }

    fn modifiers(&mut self, m: Modifiers) {
        self.events(&[Event::Keyboard(keyboard::Event::ModifiersChanged(m))]);
    }

    fn key(&mut self, k: keyboard::Key, modifiers: Modifiers) -> Status {
        let physical = match k.as_ref() {
            keyboard::Key::Character("c") => key::Physical::Code(key::Code::KeyC),
            keyboard::Key::Character("a") => key::Physical::Code(key::Code::KeyA),
            _ => key::Physical::Unidentified(key::NativeCode::Unidentified),
        };
        self.events(&[Event::Keyboard(keyboard::Event::KeyPressed {
            key: k.clone(),
            modified_key: k,
            physical_key: physical,
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })])[0]
    }

    fn ctrl(&mut self, c: &'static str) -> Status {
        self.key(keyboard::Key::Character(c.into()), Modifiers::CTRL)
    }

    /// Bounds and selection of every block, in order.
    fn blocks(&mut self) -> Vec<(Rectangle, Option<Range<usize>>, String)> {
        struct Probe(Vec<(Rectangle, Option<Range<usize>>, String)>);
        impl Operation for Probe {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(&mut self, _: Option<&Id>, bounds: Rectangle, state: &mut dyn std::any::Any) {
                if let Some(core) = state.downcast_mut::<Core>() {
                    self.0
                        .push((bounds, core.selection.clone(), core.text.clone()));
                }
            }
        }
        self.with_ui(|ui, h| {
            let mut p = Probe(Vec::new());
            ui.operate(&h.renderer, &mut p);
            p.0
        })
    }

    fn selections(&mut self) -> Vec<Option<String>> {
        self.blocks()
            .into_iter()
            .map(|(_, sel, text)| sel.map(|r| text[r].to_owned()))
            .collect()
    }

    fn copy(&mut self) -> Option<String> {
        self.clip.0 = None;
        self.ctrl("c");
        self.clip.0.clone()
    }
}

/// A point inside block `i` at fraction `fx` of its width, mid height.
fn at(h: &mut Harness, i: usize, fx: f32) -> Point {
    let b = h.blocks()[i].0;
    Point::new(b.x + b.width * fx, b.y + b.height / 2.0)
}

/// A point at the start of block `i`'s text.
fn start_of(h: &mut Harness, i: usize) -> Point {
    let b = h.blocks()[i].0;
    Point::new(b.x + 0.5, b.y + b.height / 2.0)
}

#[test]
fn drag_selects_across_blocks_and_copies() {
    let mut h = Harness::new(|| scope(blocks(&["Hello world", "second block", "third"])).into());
    let from = start_of(&mut h, 0);
    // Past everything: clamps to the end of the last block.
    let status = h.drag(from, Point::new(390.0, 590.0));
    assert_eq!(status, Status::Captured, "a drag is not a click");
    assert_eq!(
        h.selections(),
        [
            Some("Hello world".into()),
            Some("second block".into()),
            Some("third".into())
        ]
    );
    assert_eq!(
        h.copy().as_deref(),
        Some("Hello world\nsecond block\nthird")
    );
    // Into the middle of the second block only (not a double click).
    std::thread::sleep(std::time::Duration::from_millis(350));
    let to = start_of(&mut h, 1);
    h.drag(from, to);
    assert_eq!(h.selections(), [Some("Hello world".into()), None, None]);
    // Backwards works the same.
    let end = at(&mut h, 1, 0.99);
    let start = start_of(&mut h, 1);
    h.drag(end, start);
    let sel = h.selections();
    assert_eq!(sel[0], None);
    assert!(
        sel[1]
            .as_deref()
            .is_some_and(|s| "second block".ends_with(s) && s.len() > 6)
    );
}

#[test]
fn click_without_drag_selects_nothing() {
    let mut h = Harness::new(|| scope(blocks(&["Hello world"])).into());
    let p = at(&mut h, 0, 0.3);
    h.move_to(p);
    assert_eq!(h.press(mouse::Button::Left), Status::Ignored);
    // A tiny jitter is still a click.
    h.move_to(Point::new(p.x + 1.0, p.y));
    assert_eq!(h.release(), Status::Ignored);
    assert_eq!(h.selections(), [None]);
    assert_eq!(h.copy(), None);
}

#[test]
fn double_and_triple_click() {
    let mut h = Harness::new(|| scope(blocks(&["alpha beta", "gamma"])).into());
    let b = h.blocks()[0].0;
    // Find "beta" with the block's own hit test: the last 10 % of the text.
    let p = Point::new(b.x + 2.0, b.y + b.height / 2.0);
    h.click(p);
    h.click(p);
    assert_eq!(h.selections(), [Some("alpha".into()), None]);
    h.click(p);
    assert_eq!(h.selections(), [Some("alpha beta".into()), None]);
    assert_eq!(h.copy().as_deref(), Some("alpha beta"));
}

#[test]
fn shift_click_extends() {
    let mut h = Harness::new(|| scope(blocks(&["one", "two", "three"])).into());
    let a = start_of(&mut h, 0);
    let b = Point::new(390.0, h.blocks()[2].0.center_y());
    h.click(a);
    h.modifiers(Modifiers::SHIFT);
    h.click(b);
    h.modifiers(Modifiers::empty());
    assert_eq!(
        h.selections(),
        [Some("one".into()), Some("two".into()), Some("three".into())]
    );
}

#[test]
fn ctrl_a_selects_the_focused_scope_and_escape_clears() {
    let mut h = Harness::new(|| {
        column![
            scope(blocks(&["first a", "first b"])),
            scope(blocks(&["second"])),
        ]
        .spacing(20)
        .into()
    });
    // Nothing focused yet: Ctrl+A does nothing.
    assert_eq!(h.ctrl("a"), Status::Ignored);
    let p = at(&mut h, 0, 0.5);
    h.click(p);
    assert_eq!(h.ctrl("a"), Status::Captured);
    assert_eq!(
        h.selections(),
        [Some("first a".into()), Some("first b".into()), None]
    );
    assert_eq!(h.copy().as_deref(), Some("first a\nfirst b"));
    h.key(keyboard::Key::Named(key::Named::Escape), Modifiers::empty());
    assert_eq!(h.selections(), [None, None, None]);
    // A press in the other scope moves the focus there.
    h.ctrl("a");
    let q = at(&mut h, 2, 0.5);
    h.click(q);
    h.ctrl("a");
    assert_eq!(h.selections(), [None, None, Some("second".into())]);
}

#[test]
fn press_elsewhere_clears() {
    let mut h =
        Harness::new(|| column![scope(blocks(&["Hello world"])), space().height(200)].into());
    let from = start_of(&mut h, 0);
    let to = at(&mut h, 0, 0.99);
    h.drag(from, to);
    assert!(h.selections()[0].is_some());
    h.click(Point::new(200.0, 400.0));
    assert_eq!(h.selections(), [None]);
}

#[test]
fn link_click_versus_drag() {
    let view = || -> El {
        scope(
            rich::<String, Msg>(vec![
                span("see "),
                span("the docs").link("https://docs.example".to_owned()),
                span(" now"),
            ])
            .on_link_click(Msg::Link)
            .width(Length::Fill),
        )
        .into()
    };
    let mut h = Harness::new(view);
    let b = h.blocks()[0].0;
    // Find a point over the link: scan the line with the paragraph's span hit test.
    let link_at = (1..200)
        .map(|x| Point::new(b.x + x as f32, b.center_y()))
        .find(|p| {
            h.move_to(*p);
            h.interaction == mouse::Interaction::Pointer
        })
        .expect("a point over the link");
    h.click(link_at);
    assert_eq!(h.msgs, [Msg::Link("https://docs.example".into())]);
    assert_eq!(h.selections(), [None]);
    h.msgs.clear();
    // A drag starting on the link selects instead.
    h.drag(link_at, Point::new(link_at.x + 60.0, link_at.y));
    assert!(h.msgs.is_empty(), "{:?}", h.msgs);
    assert!(h.selections()[0].is_some());
    // The text cursor shows over plain text inside a scope.
    let plain = start_of(&mut h, 0);
    h.move_to(plain);
    assert_eq!(h.interaction, mouse::Interaction::Text);
}

#[test]
fn right_click_copy_menu_takes_over_the_context_menu() {
    let view = || -> El {
        context_menu(
            scope(blocks(&["copy me", "and me"])).copy_label("Copy"),
            vec![MenuEntry::Item(MenuItem::new("Message action", Msg::Menu))],
        )
    };
    let mut h = Harness::new(view);
    // No selection: the context menu takes the right press.
    let p = at(&mut h, 0, 0.5);
    h.move_to(p);
    assert_eq!(h.press(mouse::Button::Right), Status::Captured);
    h.events(&[Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(key::Named::Escape),
        modified_key: keyboard::Key::Named(key::Named::Escape),
        physical_key: key::Physical::Code(key::Code::Escape),
        location: keyboard::Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    })]);
    // With a selection the scope's Copy menu opens at the cursor.
    let from = start_of(&mut h, 0);
    h.drag(from, Point::new(390.0, 590.0));
    let p = at(&mut h, 0, 0.3);
    h.move_to(p);
    assert_eq!(h.press(mouse::Button::Right), Status::Captured);
    h.clip.0 = None;
    let item = Point::new(p.x + 30.0, p.y + 16.0);
    h.click(item);
    assert_eq!(h.clip.0.as_deref(), Some("copy me\nand me"));
    assert!(
        h.msgs.is_empty(),
        "the message menu did not open: {:?}",
        h.msgs
    );
}

#[test]
fn selection_survives_rebuilds_and_new_siblings() {
    use std::cell::Cell;
    use std::rc::Rc;
    let extra = Rc::new(Cell::new(false));
    let e = extra.clone();
    let mut h = Harness::new(move || {
        let mut col = column![scope(blocks(&["kept text"]))];
        if e.get() {
            col = col.push(scope(blocks(&["a new message"])));
        }
        col.into()
    });
    let from = start_of(&mut h, 0);
    let y = h.blocks()[0].0.center_y();
    h.drag(from, Point::new(390.0, y));
    assert_eq!(h.selections(), [Some("kept text".into())]);
    extra.set(true);
    h.events(&[]);
    assert_eq!(h.selections(), [Some("kept text".into()), None]);
    assert_eq!(h.copy().as_deref(), Some("kept text"));
}

#[test]
fn copy_uses_prefixes_and_joiners() {
    let mut h = Harness::new(|| {
        scope(
            column![
                rich::<String, Msg>(vec![span("item")]).copy_prefix("• "),
                iced::widget::row![
                    rich::<String, Msg>(vec![span("a")]),
                    rich::<String, Msg>(vec![span("b")]).joiner("\t"),
                ]
                .spacing(20),
            ]
            .width(Length::Fill),
        )
        .into()
    });
    let p = start_of(&mut h, 0);
    h.click(p);
    h.ctrl("a");
    assert_eq!(h.copy().as_deref(), Some("• item\na\tb"));
}

#[test]
fn highlight_rects_follow_lines() {
    let mut h = Harness::new(|| scope(blocks(&["one\ntwo\n\nfour"])).into());
    let p = start_of(&mut h, 0);
    h.click(p);
    h.ctrl("a");
    let rects = h.with_ui(|ui, h| {
        struct R(Vec<Rectangle>);
        impl Operation for R {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(&mut self, _: Option<&Id>, _: Rectangle, state: &mut dyn std::any::Any) {
                if let Some(core) = state.downcast_mut::<Core>() {
                    let sel = core.selection.clone().unwrap();
                    self.0 = core.rects(&sel);
                }
            }
        }
        let mut r = R(Vec::new());
        ui.operate(&h.renderer, &mut r);
        r.0
    });
    // One rectangle per line, the empty line included, top to bottom.
    assert_eq!(rects.len(), 4, "{rects:?}");
    assert!(rects.windows(2).all(|w| w[0].y < w[1].y));
    // Partial selection: only the second line.
    let sel = h.with_ui(|ui, h| {
        struct S(Vec<Rectangle>);
        impl Operation for S {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(&mut self, _: Option<&Id>, _: Rectangle, state: &mut dyn std::any::Any) {
                if let Some(core) = state.downcast_mut::<Core>() {
                    self.0 = core.rects(&(4..6));
                }
            }
        }
        let mut s = S(Vec::new());
        ui.operate(&h.renderer, &mut s);
        s.0
    });
    assert_eq!(sel.len(), 1);
    assert_eq!(sel[0].y, rects[1].y);
}

#[test]
fn hit_testing_maps_to_byte_offsets() {
    let mut h = Harness::new(|| scope(blocks(&["ab\nжё😀"])).into());
    let b = h.blocks()[0].0;
    let mut offsets = Vec::new();
    h.with_ui(|ui, h| {
        struct O(Vec<usize>, Rectangle);
        impl Operation for O {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(&mut self, _: Option<&Id>, _: Rectangle, state: &mut dyn std::any::Any) {
                if let Some(core) = state.downcast_mut::<Core>() {
                    let line = core.paragraph.buffer().metrics().line_height;
                    for (x, y) in [
                        (-5.0, -5.0),
                        (0.0, 1.0),
                        (500.0, 1.0),
                        (0.0, line + 1.0),
                        (500.0, line + 1.0),
                        (0.0, 999.0),
                    ] {
                        self.0.push(core.hit(Point::new(x, y)));
                    }
                    assert!(self.1.width > 0.0);
                }
            }
        }
        let mut o = O(Vec::new(), b);
        ui.operate(&h.renderer, &mut o);
        offsets = o.0;
    });
    let len = "ab\nжё😀".len();
    assert_eq!(offsets, [0, 0, 2, 3, len, len]);
}
