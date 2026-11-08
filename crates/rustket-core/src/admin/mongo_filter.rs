//! A small Mongo filter evaluator for settings' `enableQuery` / `displayQuery`
//! (docs/admin.md §14.4), like `createPredicateFromFilter` of
//! `@rocket.chat/mongo-adapter`.
//!
//! Supported: field equality (deep; an array field also matches when it contains
//! the value; `null` matches a missing field), `$eq`, `$ne`, `$in`, `$nin`,
//! `$exists`, `$gt`, `$gte`, `$lt`, `$lte`, `$not`, `$and`, `$or`, `$nor`, dotted
//! paths. Unknown operators evaluate to `true` (never lock the admin out) and are
//! logged at debug level.

use std::cmp::Ordering;

use serde_json::Value;

/// Whether `doc` matches `filter`.
pub fn matches(filter: &Value, doc: &Value) -> bool {
    let Value::Object(f) = filter else {
        tracing::debug!("setting query filter is not an object");
        return true;
    };
    f.iter().all(|(key, cond)| match key.as_str() {
        "$and" => as_list(cond).iter().all(|c| matches(c, doc)),
        "$or" => {
            let list = as_list(cond);
            list.is_empty() || list.iter().any(|c| matches(c, doc))
        }
        "$nor" => !as_list(cond).iter().any(|c| matches(c, doc)),
        k if k.starts_with('$') => {
            tracing::debug!(operator = k, "unsupported top-level query operator");
            true
        }
        path => field_matches(lookup(doc, path), cond),
    })
}

/// A query (`[filter, …]`) is satisfied when every filter matches at least one of
/// `docs`. `docs_for` returns the candidate documents of a filter (e.g. only the
/// setting named by its `_id`).
pub fn query_satisfied<'a, F, I>(filters: &[Value], mut docs_for: F) -> bool
where
    F: FnMut(&Value) -> I,
    I: IntoIterator<Item = &'a Value>,
{
    filters
        .iter()
        .all(|f| docs_for(f).into_iter().any(|d| matches(f, d)))
}

/// Operators of `filter` this evaluator does not know (for tests and diagnostics).
pub fn unknown_operators(filter: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_unknown(filter, &mut out);
    out
}

const TOP_OPS: [&str; 3] = ["$and", "$or", "$nor"];
const FIELD_OPS: [&str; 10] = [
    "$eq", "$ne", "$in", "$nin", "$exists", "$gt", "$gte", "$lt", "$lte", "$not",
];

fn collect_unknown(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                if k.starts_with('$')
                    && !TOP_OPS.contains(&k.as_str())
                    && !FIELD_OPS.contains(&k.as_str())
                {
                    out.push(k.clone());
                }
                collect_unknown(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_unknown(x, out)),
        _ => {}
    }
}

fn as_list(v: &Value) -> &[Value] {
    match v {
        Value::Array(a) => a,
        _ => &[],
    }
}

fn lookup<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(doc, |cur, seg| match cur {
        Value::Object(m) => m.get(seg),
        Value::Array(a) => seg.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => None,
    })
}

fn is_operator_object(cond: &Value) -> bool {
    matches!(cond, Value::Object(m) if !m.is_empty() && m.keys().all(|k| k.starts_with('$')))
}

fn field_matches(value: Option<&Value>, cond: &Value) -> bool {
    if is_operator_object(cond) {
        let Value::Object(ops) = cond else {
            return true;
        };
        ops.iter().all(|(op, arg)| operator(value, op, arg))
    } else {
        equals(value, cond)
    }
}

fn operator(value: Option<&Value>, op: &str, arg: &Value) -> bool {
    match op {
        "$eq" => equals(value, arg),
        "$ne" => !equals(value, arg),
        "$in" => as_list(arg).iter().any(|a| equals(value, a)),
        "$nin" => !as_list(arg).iter().any(|a| equals(value, a)),
        "$exists" => value.is_some() == truthy(arg),
        "$gt" => compare_any(value, arg, |o| o == Ordering::Greater),
        "$gte" => compare_any(value, arg, |o| o != Ordering::Less),
        "$lt" => compare_any(value, arg, |o| o == Ordering::Less),
        "$lte" => compare_any(value, arg, |o| o != Ordering::Greater),
        "$not" => !field_matches(value, arg),
        other => {
            tracing::debug!(operator = other, "unsupported query operator");
            true
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => true,
    }
}

/// Mongo equality: deep equality (numbers compared numerically), an array field
/// containing the value, and `null` matching a missing field.
fn equals(value: Option<&Value>, expected: &Value) -> bool {
    match value {
        None => expected.is_null(),
        Some(v) => {
            json_eq(v, expected)
                || matches!(v, Value::Array(items) if items.iter().any(|i| json_eq(i, expected)))
        }
    }
}

fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64()?.partial_cmp(&y.as_f64()?),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn compare_any(value: Option<&Value>, arg: &Value, ok: impl Fn(Ordering) -> bool) -> bool {
    match value {
        None => false,
        Some(Value::Array(items)) => items.iter().any(|i| compare(i, arg).is_some_and(&ok)),
        Some(v) => compare(v, arg).is_some_and(ok),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn equality_and_operators() {
        let d =
            json!({"_id": "LDAP_Enable", "value": true, "tags": ["a", "b"], "n": 5, "o": {"x": 1}});
        assert!(matches(&json!({"_id": "LDAP_Enable", "value": true}), &d));
        assert!(!matches(&json!({"_id": "LDAP_Enable", "value": false}), &d));
        assert!(matches(&json!({"tags": "a"}), &d));
        assert!(matches(&json!({"tags": ["a", "b"]}), &d));
        assert!(matches(&json!({"n": 5.0}), &d));
        assert!(matches(&json!({"o.x": 1}), &d));
        assert!(matches(&json!({"missing": null}), &d));
        assert!(matches(&json!({"value": {"$ne": false}}), &d));
        assert!(matches(&json!({"value": {"$exists": 1, "$ne": ""}}), &d));
        assert!(!matches(&json!({"missing": {"$exists": true}}), &d));
        assert!(matches(&json!({"missing": {"$exists": false}}), &d));
        assert!(matches(&json!({"n": {"$in": [1, 5]}}), &d));
        assert!(matches(&json!({"n": {"$nin": [1, 2]}}), &d));
        assert!(matches(&json!({"n": {"$gt": 4, "$lte": 5}}), &d));
        assert!(!matches(&json!({"n": {"$lt": 5}}), &d));
        assert!(matches(&json!({"n": {"$not": {"$lt": 5}}}), &d));
        assert!(matches(&json!({"$or": [{"n": 1}, {"n": 5}]}), &d));
        assert!(!matches(&json!({"$and": [{"n": 1}, {"n": 5}]}), &d));
        assert!(matches(&json!({"$nor": [{"n": 1}]}), &d));
        // Unknown operators never lock the editor.
        assert!(matches(&json!({"n": {"$regex": "x"}}), &d));
        assert_eq!(
            unknown_operators(&json!({"n": {"$regex": "x"}})),
            vec!["$regex"]
        );
        assert!(unknown_operators(&json!({"$or": [{"n": {"$in": [1]}}]})).is_empty());
    }

    #[test]
    fn query_semantics() {
        let docs = [
            json!({"_id": "a", "value": true}),
            json!({"_id": "b", "value": "x"}),
        ];
        let all = |_: &Value| docs.iter();
        assert!(query_satisfied(&[], all));
        assert!(query_satisfied(
            &[
                json!({"_id": "a", "value": true}),
                json!({"_id": "b", "value": "x"})
            ],
            all
        ));
        assert!(!query_satisfied(
            &[
                json!({"_id": "a", "value": true}),
                json!({"_id": "b", "value": "y"})
            ],
            all
        ));
        // A filter naming a setting that does not exist is never satisfied.
        assert!(!query_satisfied(
            &[json!({"_id": "zz", "value": false})],
            all
        ));
    }
}
