//! Values of the inputs of an open view and the `state` sent with
//! `viewSubmit` / `viewClosed` (docs/uikit.md §5.2).

use std::collections::BTreeMap;

use rocketchat::uikit::{ENGINE, Element, LayoutBlock, resolve_conditionals};
use serde_json::{Map, Value};

/// Block id used for values without one.
pub const DEFAULT_BLOCK_ID: &str = "default";

/// Values keyed by action id (duplicate action ids share one value, as apps
/// expect), each with the block id it is reported under.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewState {
    values: BTreeMap<String, (String, Value)>,
}

/// Elements whose initial values seed the state: `input.element` and the
/// elements of `actions` / `context` blocks (not section accessories), after
/// resolving conditionals, with inherited block ids.
fn seeded_elements(blocks: &[LayoutBlock]) -> Vec<Element> {
    resolve_conditionals(blocks, ENGINE)
        .iter()
        .filter(|b| {
            matches!(
                b,
                LayoutBlock::Input { .. }
                    | LayoutBlock::Actions { .. }
                    | LayoutBlock::Context { .. }
            )
        })
        .flat_map(LayoutBlock::inherited_elements)
        .collect()
}

fn block_of(e: &Element) -> String {
    e.actionable()
        .and_then(|a| a.block_id.clone())
        .unwrap_or_else(|| DEFAULT_BLOCK_ID.to_owned())
}

impl ViewState {
    /// Initial values of a freshly opened view.
    pub fn from_blocks(blocks: &[LayoutBlock]) -> Self {
        let mut s = Self::default();
        s.merge_initials(blocks);
        s
    }

    /// After `modal.update` / `contextual_bar.update`: adds initial values for
    /// action ids not present yet; keeps every existing value.
    pub fn merge_initials(&mut self, blocks: &[LayoutBlock]) {
        for e in seeded_elements(blocks) {
            let Some(action_id) = e.action_id().filter(|a| !a.is_empty()) else {
                continue;
            };
            if self.values.contains_key(action_id) {
                continue;
            }
            if let Some(v) = e.initial_value() {
                self.values.insert(action_id.to_owned(), (block_of(&e), v));
            }
        }
    }

    pub fn set(&mut self, action_id: &str, block_id: Option<&str>, value: Value) {
        let block = block_id
            .filter(|b| !b.is_empty())
            .unwrap_or(DEFAULT_BLOCK_ID)
            .to_owned();
        self.values.insert(action_id.to_owned(), (block, value));
    }

    /// Checkbox / toggle switch: toggles `option` in the selected values and
    /// keeps the whole array (the web keeps only the last toggled value).
    /// Returns whether the option is now selected.
    pub fn toggle(&mut self, action_id: &str, block_id: Option<&str>, option: &str) -> bool {
        let mut selected: Vec<String> = match self.get(action_id) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => Vec::new(),
        };
        let now_selected = if let Some(i) = selected.iter().position(|v| v == option) {
            selected.remove(i);
            false
        } else {
            selected.push(option.to_owned());
            true
        };
        let block = block_id
            .map(str::to_owned)
            .or_else(|| self.values.get(action_id).map(|(b, _)| b.clone()));
        self.set(
            action_id,
            block.as_deref(),
            Value::Array(selected.into_iter().map(Value::String).collect()),
        );
        now_selected
    }

    pub fn get(&self, action_id: &str) -> Option<&Value> {
        self.values.get(action_id).map(|(_, v)| v)
    }

    /// Selected values of a multi-value input (empty when unset).
    pub fn selected(&self, action_id: &str) -> Vec<String> {
        match self.get(action_id) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// `{blockId: {actionId: value}}`.
    pub fn to_state_json(&self) -> Value {
        let mut out: Map<String, Value> = Map::new();
        for (action, (block, value)) in &self.values {
            if let Value::Object(m) = out
                .entry(block.clone())
                .or_insert_with(|| Value::Object(Map::new()))
            {
                m.insert(action.clone(), value.clone());
            }
        }
        Value::Object(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::uikit::decode_blocks;
    use serde_json::json;

    fn blocks() -> Vec<LayoutBlock> {
        decode_blocks(&json!([
            {"type": "input", "blockId": "b_name", "label": {"type": "plain_text", "text": "Name"},
             "element": {"type": "plain_text_input", "actionId": "name", "initialValue": ""}},
            {"type": "input", "blockId": "b_n", "label": {"type": "plain_text", "text": "N"},
             "element": {"type": "linear_scale", "actionId": "n", "initialValue": 0}},
            {"type": "actions", "blockId": "b_act", "elements": [
                {"type": "static_select", "actionId": "sel", "initialValue": "b", "options": []},
                {"type": "checkbox", "actionId": "chk", "blockId": "own", "options": [],
                 "initialOptions": [{"text": {"type": "plain_text", "text": "C1"}, "value": "c1"}]},
                {"type": "toggle_switch", "actionId": "tgl", "options": []},
                {"type": "button", "actionId": "btn", "value": "v"}]},
            {"type": "section", "blockId": "b_sec",
             "accessory": {"type": "static_select", "actionId": "acc", "initialValue": "x", "options": []}},
            {"type": "actions", "elements": [{"type": "datepicker", "actionId": "date", "initialDate": "2026-11-08"}]},
            {"type": "conditional", "when": {"engine": ["rocket.chat"]}, "render": [
                {"type": "input", "blockId": "b_cond", "label": {"type": "plain_text", "text": "C"},
                 "element": {"type": "radio_button", "actionId": "rad", "initialOption": {"text": {"type": "plain_text", "text": "R"}, "value": "r1"}}}]},
            {"type": "conditional", "when": {"engine": ["livechat"]}, "render": [
                {"type": "input", "label": {"type": "plain_text", "text": "L"},
                 "element": {"type": "plain_text_input", "actionId": "lc", "initialValue": "x"}}]}
        ]))
    }

    #[test]
    fn initial_state_extraction() {
        let s = ViewState::from_blocks(&blocks());
        assert_eq!(
            s.to_state_json(),
            json!({
                "b_name": {"name": ""},
                "b_n": {"n": 0},
                "b_act": {"sel": "b"},
                "own": {"chk": ["c1"]},
                "default": {"date": "2026-11-08"},
                "b_cond": {"rad": "r1"}
            })
        );
        // Section accessories, buttons, toggles without initialOptions and
        // non-matching conditionals seed nothing.
        for absent in ["acc", "btn", "tgl", "lc"] {
            assert!(s.get(absent).is_none(), "{absent}");
        }
    }

    #[test]
    fn toggles_keep_the_whole_array() {
        let mut s = ViewState::from_blocks(&blocks());
        assert!(s.toggle("chk", None, "c2"));
        assert_eq!(s.get("chk"), Some(&json!(["c1", "c2"])));
        assert!(!s.toggle("chk", None, "c1"));
        assert_eq!(s.to_state_json()["own"]["chk"], json!(["c2"]));
        // No initialOptions: starts empty.
        assert!(s.toggle("tgl", Some("b_act"), "t1"));
        assert_eq!(s.to_state_json()["b_act"]["tgl"], json!(["t1"]));
    }

    #[test]
    fn set_groups_by_block_and_defaults() {
        let mut s = ViewState::default();
        s.set("a", Some("blk"), json!("1"));
        s.set("b", None, json!("2"));
        s.set("c", Some(""), json!(["x"]));
        assert_eq!(
            s.to_state_json(),
            json!({"blk": {"a": "1"}, "default": {"b": "2", "c": ["x"]}})
        );
        // Duplicate action ids share one value.
        s.set("a", Some("other"), json!("3"));
        assert_eq!(s.to_state_json()["other"]["a"], "3");
        assert!(s.to_state_json().get("blk").is_none());
    }

    #[test]
    fn merge_initials_adds_only_missing() {
        let mut s = ViewState::from_blocks(&blocks());
        s.set("sel", Some("b_act"), json!("a"));
        let updated = decode_blocks(&json!([
            {"type": "actions", "blockId": "b_act", "elements": [
                {"type": "static_select", "actionId": "sel", "initialValue": "b", "options": []},
                {"type": "plain_text_input", "actionId": "new", "initialValue": "fresh"}]}
        ]));
        s.merge_initials(&updated);
        assert_eq!(s.get("sel"), Some(&json!("a")));
        assert_eq!(s.get("new"), Some(&json!("fresh")));
        // Values of elements no longer in the view are kept.
        assert_eq!(s.get("name"), Some(&json!("")));
    }
}
