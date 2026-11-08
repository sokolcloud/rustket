use super::{KeyedColumn, keyed_list_keys};
use iced::advanced::layout;
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget, tree};
use iced::widget::{container, lazy, sensor, space, text};
use iced::{Element, Length, Rectangle, Size, Theme, mouse};
use std::cell::Cell;

type El = Element<'static, (), Theme, ()>;

const SENSOR: u64 = 0;

thread_local! {
    static BUILDS: Cell<usize> = const { Cell::new(0) };
}

/// A stateful leaf whose state remembers the key it was created for, like the
/// `show` flag of a row's context menu.
struct Probe(u64);
struct ProbeState(u64);

impl Widget<(), Theme, ()> for Probe {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, _: &mut Tree, _: &(), _: &layout::Limits) -> layout::Node {
        layout::Node::new(Size::ZERO)
    }

    fn draw(
        &self,
        _: &Tree,
        _: &mut (),
        _: &Theme,
        _: &renderer::Style,
        _: layout::Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ProbeState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ProbeState(self.0))
    }
}

fn column(rows: impl IntoIterator<Item = (u64, El)>) -> El {
    rows.into_iter()
        .fold(KeyedColumn::new(), |col, (key, el)| col.push(key, el))
        .into()
}

/// Key 0 is a sensor (the list's top trigger), other keys are lazy rows
/// holding a probe; every lazy build is counted.
fn rows(keys: &[u64]) -> El {
    column(keys.iter().map(|&k| {
        let el: El = if k == SENSOR {
            sensor(space().height(1)).key(k).into()
        } else {
            lazy(k, |k| -> El {
                BUILDS.with(|b| b.set(b.get() + 1));
                Element::new(Probe(*k))
            })
            .into()
        };
        (k, el)
    }))
}

/// Diffs `to` over the tree of `from`; returns the tree and the lazy builds.
fn step(from: &[u64], to: &[u64]) -> (Tree, usize) {
    let mut tree = Tree::new(rows(from).as_widget());
    let new = rows(to);
    BUILDS.with(|b| b.set(0));
    tree.diff(new.as_widget());
    (tree, BUILDS.with(Cell::get))
}

fn tags_match(tree: &Tree, fresh: &Tree) -> bool {
    tree.tag == fresh.tag
        && tree.children.len() == fresh.children.len()
        && tree
            .children
            .iter()
            .zip(&fresh.children)
            .all(|(a, b)| tags_match(a, b))
}

/// For every lazy row: the key its probe state was created for.
fn owners(tree: &Tree, keys: &[u64]) -> Vec<(u64, u64)> {
    keys.iter()
        .zip(&tree.children)
        .filter(|(k, _)| **k != SENSOR)
        .map(|(k, t)| (*k, t.children[0].state.downcast_ref::<ProbeState>().0))
        .collect()
}

type Case = (&'static str, &'static [u64], &'static [u64]);

const SHAPES: &[Case] = &[
    ("steady", &[0, 1, 2, 3], &[0, 1, 2, 3]),
    ("append", &[0, 1, 2, 3], &[0, 1, 2, 3, 4]),
    ("prepend", &[0, 3, 4], &[0, 1, 2, 3, 4]),
    ("slide", &[0, 1, 2, 3], &[0, 2, 3, 4]),
    ("remove middle", &[0, 1, 2, 3], &[0, 1, 3]),
    ("reorder", &[0, 1, 2, 3], &[0, 3, 1, 2]),
    ("first reply", &[7, 0], &[7, 0, 1]),
    ("new reply", &[7, 0, 1, 2], &[7, 0, 1, 2, 3]),
    ("thread switch, more", &[7, 0, 1], &[8, 0, 2, 3, 4]),
    ("thread switch, fewer", &[7, 0, 1, 2, 3], &[8, 0, 4, 5]),
];

#[test]
fn state_follows_key() {
    for (name, from, to) in SHAPES {
        let (tree, _) = step(from, to);
        assert_eq!(keyed_list_keys::<u64>(&tree), Some(*to), "{name}");
        let owners = owners(&tree, to);
        assert!(owners.iter().all(|(k, s)| k == s), "{name}: {owners:?}");
        assert!(
            tags_match(&tree, &Tree::new(rows(to).as_widget())),
            "{name}"
        );
    }
}

#[test]
fn only_new_rows_are_built() {
    let window: Vec<u64> = (1..=60).collect();
    let with_top = |keys: &[u64]| [&[SENSOR][..], keys].concat();
    let shift = |by: u64| -> Vec<u64> { window.iter().map(|k| k + by).collect() };

    let steady = step(&with_top(&window), &with_top(&window)).1;
    let append = step(&with_top(&window), &with_top(&(1..=61).collect::<Vec<_>>())).1;
    let slide = step(&with_top(&window), &with_top(&shift(1))).1;
    // An older page of 50 above the loaded rows.
    let older: Vec<u64> = (1..=110).collect();
    let prepend = step(&with_top(&shift(50)), &with_top(&older)).1;
    let reply = step(&[100, SENSOR, 1, 2, 3], &[100, SENSOR, 1, 2, 3, 4]).1;
    assert_eq!((steady, append, slide, prepend, reply), (0, 1, 1, 50, 1));
}

#[test]
fn widget_type_changes_under_one_key_rebuild_the_tree() {
    #[derive(Clone, Copy, Debug)]
    enum Kind {
        Lazy,
        Container,
        Sensor,
    }
    fn child(kind: Kind, n: u64) -> El {
        match kind {
            Kind::Lazy => lazy(n, |n| -> El { text(n.to_string()).into() }).into(),
            Kind::Container => container(text(n.to_string())).into(),
            Kind::Sensor => sensor(space().height(1)).key(n).into(),
        }
    }
    fn frame(rows: &[(u64, Kind)]) -> El {
        column(rows.iter().map(|&(k, kind)| (k, child(kind, k))))
    }
    let kinds = [Kind::Lazy, Kind::Container, Kind::Sensor];
    for a in kinds {
        for b in kinds {
            let from = frame(&[(0, Kind::Sensor), (1, a), (2, Kind::Lazy)]);
            let to = frame(&[(0, Kind::Sensor), (1, b), (2, Kind::Lazy)]);
            let mut tree = Tree::new(from.as_widget());
            tree.diff(to.as_widget());
            assert!(
                tags_match(&tree, &Tree::new(to.as_widget())),
                "{a:?} -> {b:?}"
            );
            // Diffing again must not touch foreign state.
            tree.diff(to.as_widget());
            tree.diff(from.as_widget());
            assert!(
                tags_match(&tree, &Tree::new(from.as_widget())),
                "{b:?} -> {a:?}"
            );
        }
    }
}

#[test]
fn duplicate_keys_match_in_order() {
    // Debug builds assert on duplicates outside these tests.
    for (from, to) in [
        (&[1u64, 1, 2][..], &[1u64, 1, 2, 3][..]),
        (&[1, 2, 3], &[2, 2, 2]),
        (&[2, 2], &[1]),
    ] {
        let (tree, _) = step(from, to);
        assert!(
            tags_match(&tree, &Tree::new(rows(to).as_widget())),
            "{from:?} -> {to:?}"
        );
        assert_eq!(tree.children.len(), to.len());
    }
    // Every row, repeated or not, still gets a tree of its own key.
    let (tree, _) = step(&[1, 1, 2, 3], &[3, 1, 1, 2]);
    assert!(owners(&tree, &[3, 1, 1, 2]).iter().all(|(k, s)| k == s));
}

#[test]
fn empty_lists() {
    let (tree, builds) = step(&[], &[1, 2]);
    assert_eq!((tree.children.len(), builds), (2, 2));
    let (tree, builds) = step(&[1, 2], &[]);
    assert_eq!((tree.children.len(), builds), (0, 0));
}
