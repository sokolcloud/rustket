//! The process-wide indicator look. A binary of its own: switching the
//! global look would race with tests that draw under the default look.

use rustket_ui::theme::{IndicatorLook, generation, indicator_look, set_indicator_look};

#[test]
fn set_indicator_look_bumps_the_generation_only_on_change() {
    assert_eq!(indicator_look(), IndicatorLook::Rustket);
    let g = generation();
    set_indicator_look(IndicatorLook::Rustket);
    assert_eq!(generation(), g);
    set_indicator_look(IndicatorLook::RocketChat);
    assert_eq!(indicator_look(), IndicatorLook::RocketChat);
    assert_eq!(generation(), g + 1);
    set_indicator_look(IndicatorLook::RocketChat);
    assert_eq!(generation(), g + 1);
    set_indicator_look(IndicatorLook::Rustket);
    assert_eq!(indicator_look(), IndicatorLook::Rustket);
    assert_eq!(generation(), g + 2);
}
