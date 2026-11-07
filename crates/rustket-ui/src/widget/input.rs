//! Labelled text field with help/error text.

use iced::widget::{column, text, text_input};
use iced::{Element, Length};

use crate::style;
use crate::theme::{FONT_MEDIUM, SPACE_1, TEXT_BODY, TEXT_CAPTION};

/// Text field builder; convert into an `Element` when done.
pub struct TextField<'a, Message> {
    label: String,
    value: &'a str,
    placeholder: String,
    help: Option<String>,
    error: Option<String>,
    secure: bool,
    disabled: bool,
    id: Option<iced::widget::Id>,
    width: Length,
    on_input: Box<dyn Fn(String) -> Message + 'a>,
    on_submit: Option<Message>,
}

/// Creates a labelled text field. An empty label hides the label row.
pub fn text_field<'a, Message>(
    label: impl Into<String>,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> TextField<'a, Message> {
    TextField {
        label: label.into(),
        value,
        placeholder: String::new(),
        help: None,
        error: None,
        secure: false,
        disabled: false,
        id: None,
        width: Length::Fill,
        on_input: Box::new(on_input),
        on_submit: None,
    }
}

impl<'a, Message> TextField<'a, Message> {
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Hint shown under the field when there is no error.
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Error text; also turns the border red.
    pub fn error(mut self, error: Option<String>) -> Self {
        self.error = error;
        self
    }

    /// Password mode.
    pub fn password(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_submit(mut self, message: Message) -> Self {
        self.on_submit = Some(message);
        self
    }

    /// Widget id, for focusing with `iced::widget::operation::focus`.
    pub fn id(mut self, id: impl Into<iced::widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

impl<'a, Message: Clone + 'a> From<TextField<'a, Message>> for Element<'a, Message> {
    fn from(f: TextField<'a, Message>) -> Self {
        let has_error = f.error.is_some();
        let mut input = text_input(&f.placeholder, f.value)
            .secure(f.secure)
            .size(TEXT_BODY)
            .padding([8, 12])
            .style(if has_error {
                style::text_input_error
            } else {
                style::text_input
            });
        if !f.disabled {
            input = input.on_input(f.on_input);
            if let Some(msg) = f.on_submit {
                input = input.on_submit(msg);
            }
        }
        if let Some(id) = f.id {
            input = input.id(id);
        }
        let mut col = column![].spacing(SPACE_1 + 2.0).width(f.width);
        if !f.label.is_empty() {
            col = col.push(text(f.label).size(TEXT_BODY - 1.0).font(FONT_MEDIUM));
        }
        col = col.push(input);
        if let Some(error) = f.error {
            col = col.push(text(error).size(TEXT_CAPTION).style(style::text_danger));
        } else if let Some(help) = f.help {
            col = col.push(text(help).size(TEXT_CAPTION).style(style::text_hint));
        }
        col.into()
    }
}
