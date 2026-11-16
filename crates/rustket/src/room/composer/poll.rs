//! "Create poll": the dialog that writes a poll message by the convention
//! of `rustket_core::poll` (`docs/polls.md`) and, optionally, adds the
//! option reactions once it is sent so voters can click them in any client.

use iced::widget::{checkbox, column, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::poll::{self, MAX_OPTIONS, MIN_OPTIONS, PollError};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{dialog, icon_button_with, primary_button, secondary_button};

/// Id of the question field (focused when the dialog opens).
pub const QUESTION_ID: &str = "poll-question";

#[derive(Debug, Clone)]
pub enum Msg {
    Question(String),
    Option(usize, String),
    AddOption,
    RemoveOption(usize),
    AddReactions(bool),
    Submit,
    Cancel,
}

/// The dialog's state.
#[derive(Debug, Clone, PartialEq)]
pub struct PollDraft {
    pub question: String,
    pub options: Vec<String>,
    pub add_reactions: bool,
    /// Shown after a submit that did not validate.
    error: Option<PollError>,
}

impl Default for PollDraft {
    fn default() -> Self {
        Self {
            question: String::new(),
            options: vec![String::new(); MIN_OPTIONS],
            add_reactions: true,
            error: None,
        }
    }
}

/// What the composer does after an update.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    Keep,
    Close,
    /// Send this text; add these reactions once it is sent.
    Send {
        text: String,
        reactions: Vec<&'static str>,
    },
}

impl PollDraft {
    pub fn update(&mut self, m: Msg) -> Done {
        match m {
            Msg::Question(q) => self.question = q,
            Msg::Option(i, v) => {
                if let Some(o) = self.options.get_mut(i) {
                    *o = v;
                }
            }
            Msg::AddOption => {
                if self.options.len() < MAX_OPTIONS {
                    self.options.push(String::new());
                }
            }
            Msg::RemoveOption(i) => {
                if self.options.len() > MIN_OPTIONS && i < self.options.len() {
                    self.options.remove(i);
                }
            }
            Msg::AddReactions(on) => self.add_reactions = on,
            Msg::Cancel => return Done::Close,
            Msg::Submit => {
                return match poll::generate(&self.question, &self.options) {
                    Ok(text) => {
                        let n = self.options.iter().filter(|o| !o.trim().is_empty()).count();
                        let reactions = if self.add_reactions {
                            (0..n).filter_map(poll::reaction).collect()
                        } else {
                            Vec::new()
                        };
                        Done::Send { text, reactions }
                    }
                    Err(e) => {
                        self.error = Some(e);
                        Done::Keep
                    }
                };
            }
        }
        self.error = None;
        Done::Keep
    }

    pub fn view<'a, M: Clone + 'a>(
        &'a self,
        wrap: impl Fn(Msg) -> M + Copy + 'a,
    ) -> Element<'a, M> {
        let mut options = column![].spacing(SPACE_1);
        for (i, o) in self.options.iter().enumerate() {
            let mut line = row![
                text(poll::keycap(i).unwrap_or_default().to_owned())
                    .size(16)
                    .shaping(text::Shaping::Advanced),
                text_input(&t!("poll.option_placeholder", n = i + 1), o)
                    .on_input(move |v| wrap(Msg::Option(i, v)))
                    .on_submit(wrap(Msg::Submit))
                    .padding([6, 10])
                    .style(style::text_input),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
            if self.options.len() > MIN_OPTIONS {
                line = line.push(icon_button_with(
                    Icon::X,
                    Some(wrap(Msg::RemoveOption(i))),
                    t!("poll.remove_option"),
                    false,
                    26.0,
                ));
            }
            options = options.push(line);
        }
        let add = (self.options.len() < MAX_OPTIONS).then(|| wrap(Msg::AddOption));
        let mut content = column![
            text_input(&t!("poll.question_placeholder"), &self.question)
                .id(iced::widget::Id::new(QUESTION_ID))
                .on_input(move |v| wrap(Msg::Question(v)))
                .on_submit(wrap(Msg::Submit))
                .padding([6, 10])
                .style(style::text_input),
            scrollable(options).height(Length::Shrink),
            secondary_button(t!("poll.add_option"), add).icon(Icon::Plus),
            checkbox(self.add_reactions)
                .label(t!("poll.add_reactions").into_owned())
                .on_toggle(move |on| wrap(Msg::AddReactions(on)))
                .text_size(TEXT_SMALL),
            text(t!("poll.convention_hint"))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        ]
        .spacing(SPACE_3);
        if let Some(e) = self.error {
            let key = match e {
                PollError::NoQuestion => "poll.error_question",
                PollError::TooFewOptions => "poll.error_options",
                PollError::TooManyOptions => "poll.error_too_many",
            };
            content = content.push(text(t!(key)).size(TEXT_SMALL).style(style::text_danger));
        }
        dialog(
            t!("poll.create_title").into_owned(),
            content,
            vec![
                secondary_button(t!("room_composer.cancel"), Some(wrap(Msg::Cancel))).into(),
                primary_button(t!("poll.send"), Some(wrap(Msg::Submit))).into(),
            ],
            Some(wrap(Msg::Cancel)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_edits_and_submits() {
        let mut d = PollDraft::default();
        assert_eq!(d.update(Msg::Submit), Done::Keep);
        assert_eq!(d.error, Some(PollError::NoQuestion));
        d.update(Msg::Question("Lunch?".into()));
        assert!(d.error.is_none());
        d.update(Msg::Option(0, "Pizza".into()));
        assert_eq!(d.update(Msg::Submit), Done::Keep);
        assert_eq!(d.error, Some(PollError::TooFewOptions));
        d.update(Msg::AddOption);
        d.update(Msg::Option(2, "Sushi".into()));
        d.update(Msg::RemoveOption(1));
        assert_eq!(d.options, ["Pizza", "Sushi"]);
        d.update(Msg::RemoveOption(0));
        assert_eq!(
            d.options.len(),
            MIN_OPTIONS,
            "never fewer fields than the minimum"
        );
        for _ in 0..20 {
            d.update(Msg::AddOption);
        }
        assert_eq!(d.options.len(), MAX_OPTIONS);
        match d.update(Msg::Submit) {
            Done::Send { text, reactions } => {
                assert_eq!(text, "📊 Lunch?\n1️⃣ Pizza\n2️⃣ Sushi");
                assert_eq!(reactions, [":one:", ":two:"]);
            }
            other => panic!("{other:?}"),
        }
        d.update(Msg::AddReactions(false));
        assert!(
            matches!(d.update(Msg::Submit), Done::Send { reactions, .. } if reactions.is_empty())
        );
        assert_eq!(d.update(Msg::Cancel), Done::Close);
    }
}
