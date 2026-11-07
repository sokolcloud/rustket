//! REST/method calls of the account screen (profile, preferences, 2FA,
//! personal access tokens). Verified against Rocket.Chat 8.9.1; see
//! `tests/account_live.rs`.

use rocketchat::auth::TwoFactorCode;
use rocketchat::models::User;
use rocketchat::rest::Query;
use rocketchat::{Error, RcDate, RestClient, Result};
use serde::Deserialize;
use serde_json::{Value, json};

/// `2fa:enable` result: the TOTP secret (base32) and its `otpauth://` URL.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TotpSetup {
    pub secret: String,
    pub url: String,
}

/// One personal access token (`users.getPersonalAccessTokens`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalToken {
    pub name: String,
    #[serde(default)]
    pub created_at: Option<RcDate>,
    #[serde(default)]
    pub last_token_part: String,
    #[serde(default)]
    pub bypass_two_factor: bool,
}

/// `pw.getPolicy`: `[["get-password-policy-minLength", {"minLength": 14}], …]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PasswordPolicy {
    pub enabled: bool,
    pub rules: Vec<PolicyRule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRule {
    MinLength(usize),
    MaxLength(usize),
    /// No run of the same character longer than this.
    MaxRepeating(usize),
    Lowercase,
    Uppercase,
    Number,
    Special,
}

impl PasswordPolicy {
    pub fn parse(v: &Value) -> Self {
        let enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        let mut rules = Vec::new();
        let mut forbid_repeating = false;
        let mut repeat_count = None;
        for entry in v
            .get("policy")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(key) = entry.get(0).and_then(Value::as_str) else {
                continue;
            };
            let num = |k: &str| {
                entry
                    .get(1)
                    .and_then(|p| p.get(k))
                    .and_then(Value::as_u64)
                    .map(|n| n as usize)
            };
            match key.trim_start_matches("get-password-policy-") {
                "minLength" => rules.extend(num("minLength").map(PolicyRule::MinLength)),
                "maxLength" => rules.extend(num("maxLength").map(PolicyRule::MaxLength)),
                "forbidRepeatingCharacters" => forbid_repeating = true,
                "forbidRepeatingCharactersCount" => {
                    repeat_count = num("forbidRepeatingCharactersCount")
                }
                "mustContainAtLeastOneLowercase" => rules.push(PolicyRule::Lowercase),
                "mustContainAtLeastOneUppercase" => rules.push(PolicyRule::Uppercase),
                "mustContainAtLeastOneNumber" => rules.push(PolicyRule::Number),
                "mustContainAtLeastOneSpecialCharacter" => rules.push(PolicyRule::Special),
                _ => {}
            }
        }
        if forbid_repeating {
            rules.push(PolicyRule::MaxRepeating(repeat_count.unwrap_or(3)));
        }
        Self { enabled, rules }
    }

    /// Rules `password` does not satisfy (empty when the policy is off).
    pub fn unmet(&self, password: &str) -> Vec<PolicyRule> {
        if !self.enabled {
            return Vec::new();
        }
        self.rules
            .iter()
            .copied()
            .filter(|r| !r.satisfied(password))
            .collect()
    }
}

impl PolicyRule {
    pub fn satisfied(self, p: &str) -> bool {
        match self {
            Self::MinLength(n) => p.chars().count() >= n,
            Self::MaxLength(n) => p.chars().count() <= n,
            Self::MaxRepeating(n) => {
                let mut run = 0;
                let mut prev = None;
                for c in p.chars() {
                    run = if prev == Some(c) { run + 1 } else { 1 };
                    prev = Some(c);
                    if run > n {
                        return false;
                    }
                }
                true
            }
            Self::Lowercase => p.chars().any(|c| c.is_ascii_lowercase()),
            Self::Uppercase => p.chars().any(|c| c.is_ascii_uppercase()),
            Self::Number => p.chars().any(|c| c.is_ascii_digit()),
            Self::Special => p
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && !c.is_whitespace()),
        }
    }

    /// Localised hint.
    pub fn hint(self) -> String {
        use rust_i18n::t;
        match self {
            Self::MinLength(n) => t!("account.pw_min_length", n = n).into_owned(),
            Self::MaxLength(n) => t!("account.pw_max_length", n = n).into_owned(),
            Self::MaxRepeating(n) => t!("account.pw_max_repeating", n = n).into_owned(),
            Self::Lowercase => t!("account.pw_lowercase").into_owned(),
            Self::Uppercase => t!("account.pw_uppercase").into_owned(),
            Self::Number => t!("account.pw_number").into_owned(),
            Self::Special => t!("account.pw_special").into_owned(),
        }
    }
}

pub async fn password_policy(rest: &RestClient) -> Result<PasswordPolicy> {
    let v: Value = rest.get("pw.getPolicy", &Query::new()).await?;
    Ok(PasswordPolicy::parse(&v))
}

/// `users.updateOwnBasicInfo` with `data` (only the changed fields). E-mail and
/// password changes ask for a second factor (`TwoFactorRequired`; method
/// `password` when the user has no 2FA): repeat with `code`.
pub async fn update_own_basic_info(
    rest: &RestClient,
    data: Value,
    code: Option<TwoFactorCode>,
) -> Result<User> {
    let v: Value = rest
        .post_with(
            "users.updateOwnBasicInfo",
            &json!({ "data": data }),
            code.as_ref(),
        )
        .await?;
    field(v, "user")
}

/// `users.setPreferences` with the changed preferences.
pub async fn set_preferences(rest: &RestClient, data: Value) -> Result<()> {
    rest.post_ok("users.setPreferences", &json!({ "data": data }))
        .await
}

pub async fn username_available(rest: &RestClient, username: &str) -> Result<bool> {
    let r: Result<Value> = rest
        .get(
            "users.checkUsernameAvailability",
            &Query::new().push("username", username),
        )
        .await;
    match r {
        Ok(v) => Ok(v.get("result").and_then(Value::as_bool).unwrap_or(false)),
        // Reserved names (`admin`, …) are not available either.
        Err(Error::Api {
            error_type: Some(t),
            ..
        }) if t == "error-blocked-username" => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn send_confirmation_email(rest: &RestClient, email: &str) -> Result<()> {
    rest.post_ok("users.sendConfirmationEmail", &json!({ "email": email }))
        .await
}

pub async fn totp_enable(rest: &RestClient) -> Result<TotpSetup> {
    rest.method_call("2fa:enable", json!([])).await
}

/// Confirms the TOTP setup; the backup codes (shown once), or `None` for a wrong
/// code.
pub async fn totp_validate(rest: &RestClient, code: &str) -> Result<Option<Vec<String>>> {
    match rest
        .method_call_raw("2fa:validateTempToken", json!([code]), false)
        .await
    {
        Ok(v) => Ok(codes(v)),
        Err(e) if is_invalid_totp(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `false` for a wrong code.
pub async fn totp_disable(rest: &RestClient, code: &str) -> Result<bool> {
    let v = rest
        .method_call_raw("2fa:disable", json!([code]), false)
        .await?;
    Ok(v.as_bool().unwrap_or(false))
}

/// New backup codes, or `None` for a wrong code (the server returns nothing).
pub async fn totp_regenerate_codes(rest: &RestClient, code: &str) -> Result<Option<Vec<String>>> {
    let v = rest
        .method_call_raw("2fa:regenerateCodes", json!([code]), false)
        .await?;
    Ok(codes(v))
}

pub async fn totp_codes_remaining(rest: &RestClient) -> Result<u64> {
    let v = rest
        .method_call_raw("2fa:checkCodesRemaining", json!([]), false)
        .await?;
    Ok(v.get("remaining").and_then(Value::as_u64).unwrap_or(0))
}

/// Enables e-mail 2FA (needs a verified e-mail; logs out the user's other
/// sessions).
pub async fn email_2fa_enable(rest: &RestClient) -> Result<()> {
    rest.post_ok("users.2fa.enableEmail", &json!({})).await
}

/// Asks for an e-mailed code first (`TwoFactorRequired`, method `email`).
pub async fn email_2fa_disable(rest: &RestClient, code: Option<TwoFactorCode>) -> Result<()> {
    let _: Value = rest
        .post_with("users.2fa.disableEmail", &json!({}), code.as_ref())
        .await?;
    Ok(())
}

pub async fn list_tokens(rest: &RestClient) -> Result<Vec<PersonalToken>> {
    let v: Value = rest
        .get("users.getPersonalAccessTokens", &Query::new())
        .await?;
    field(v, "tokens")
}

pub async fn generate_token(
    rest: &RestClient,
    name: &str,
    bypass_two_factor: bool,
    code: Option<TwoFactorCode>,
) -> Result<String> {
    let v: Value = rest
        .post_with(
            "users.generatePersonalAccessToken",
            &json!({ "tokenName": name, "bypassTwoFactor": bypass_two_factor }),
            code.as_ref(),
        )
        .await?;
    field(v, "token")
}

pub async fn regenerate_token(
    rest: &RestClient,
    name: &str,
    code: Option<TwoFactorCode>,
) -> Result<String> {
    let v: Value = rest
        .post_with(
            "users.regeneratePersonalAccessToken",
            &json!({ "tokenName": name }),
            code.as_ref(),
        )
        .await?;
    field(v, "token")
}

pub async fn remove_token(
    rest: &RestClient,
    name: &str,
    code: Option<TwoFactorCode>,
) -> Result<()> {
    let _: Value = rest
        .post_with(
            "users.removePersonalAccessToken",
            &json!({ "tokenName": name }),
            code.as_ref(),
        )
        .await?;
    Ok(())
}

fn codes(v: Value) -> Option<Vec<String>> {
    let list: Vec<String> = v
        .get("codes")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    (!list.is_empty()).then_some(list)
}

fn is_invalid_totp(e: &Error) -> bool {
    let text = e.to_string();
    text.contains("invalid-totp") || matches!(e, Error::TwoFactorInvalid(_))
}

fn field<T: serde::de::DeserializeOwned>(mut v: Value, key: &str) -> Result<T> {
    let inner = v
        .get_mut(key)
        .map(Value::take)
        .ok_or_else(|| Error::Protocol(format!("missing `{key}` in response")))?;
    Ok(serde_json::from_value(inner)?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn policy_from_server_json() {
        let v = json!({"enabled":true,"policy":[
            ["get-password-policy-minLength",{"minLength":14}],
            ["get-password-policy-forbidRepeatingCharacters"],
            ["get-password-policy-forbidRepeatingCharactersCount",{"forbidRepeatingCharactersCount":3}],
            ["get-password-policy-mustContainAtLeastOneLowercase"],
            ["get-password-policy-mustContainAtLeastOneUppercase"],
            ["get-password-policy-mustContainAtLeastOneNumber"],
            ["get-password-policy-mustContainAtLeastOneSpecialCharacter"]],"success":true});
        let p = PasswordPolicy::parse(&v);
        assert!(p.enabled);
        assert_eq!(
            p.rules,
            vec![
                PolicyRule::MinLength(14),
                PolicyRule::Lowercase,
                PolicyRule::Uppercase,
                PolicyRule::Number,
                PolicyRule::Special,
                PolicyRule::MaxRepeating(3),
            ]
        );
        assert_eq!(
            p.unmet("short"),
            vec![
                PolicyRule::MinLength(14),
                PolicyRule::Uppercase,
                PolicyRule::Number,
                PolicyRule::Special
            ]
        );
        assert!(p.unmet("Another-pass-5678!").is_empty());
        assert_eq!(
            p.unmet("Aaaaa-pass-5678!"),
            vec![PolicyRule::MaxRepeating(3)]
        );
        assert!(p.unmet("Aaaa-pass-56789!").is_empty());
        let off = PasswordPolicy::parse(&json!({"enabled": false, "policy": []}));
        assert!(off.unmet("x").is_empty());
    }

    #[test]
    fn backup_codes() {
        assert_eq!(
            codes(json!({"codes": ["a", "b"]})),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(codes(Value::Null), None);
        assert_eq!(codes(json!({"codes": []})), None);
    }

    #[test]
    fn token_list_decodes() {
        let v = json!({"tokens":[{"name":"t1","createdAt":"2026-11-07T21:45:25.692Z","lastTokenPart":"nDvwtL","bypassTwoFactor":false}],"success":true});
        let t: Vec<PersonalToken> = field(v, "tokens").unwrap();
        assert_eq!(t[0].name, "t1");
        assert_eq!(t[0].last_token_part, "nDvwtL");
        assert!(t[0].created_at.is_some());
    }
}
