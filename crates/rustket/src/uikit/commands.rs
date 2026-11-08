//! Slash commands (docs/uikit.md §7): every `commands.run` gets a trigger id
//! registered in the store (app commands may answer by pushing a view with
//! it; pushes for built-in commands are ignored), app commands show the busy
//! pill, and a failure leaves a local ephemeral message from `rocket.cat`
//! like the web's "Something went wrong while executing command".

use std::time::Instant;

use iced::Task;
use rocketchat::models::{Message as RcMessage, UserRef};
use rocketchat::{RcDate, RestClient};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::uikit::trust::ROCKET_CAT;

use super::UiKitMsg;
use crate::error::{Failure, Res};

/// The end of a `commands.run` call.
#[derive(Debug, Clone)]
pub struct Ran {
    pub trigger: String,
    pub command: String,
    pub rid: String,
    pub tmid: Option<String>,
    pub result: Res<serde_json::Value>,
}

/// The request a composer hands to the controller (`Effect::UiKit`) to run
/// `/command params` in `rid` (thread `tmid`); `app_id` from `commands.list`
/// (`None` for built-in commands).
pub fn request(
    rid: &str,
    tmid: Option<&str>,
    command: String,
    params: String,
    app_id: Option<String>,
) -> UiKitMsg {
    UiKitMsg::Command {
        rid: rid.to_owned(),
        tmid: tmid.map(str::to_owned),
        command,
        params,
        app_id,
    }
}

/// Runs the command with a trigger registered in the store.
pub fn run(
    ws: &mut Workspace,
    rest: Option<RestClient>,
    rid: String,
    tmid: Option<String>,
    command: String,
    params: String,
    app_id: Option<String>,
) -> Task<Ran> {
    let trigger = ws.uikit_mut().slash_command_trigger(
        app_id.as_deref(),
        &rid,
        tmid.as_deref(),
        Instant::now(),
    );
    let Some(rest) = rest else {
        return Task::done(Ran {
            trigger,
            command,
            rid,
            tmid,
            result: Err(Failure::from(rocketchat::Error::Unauthorized {
                message: None,
                code: None,
            })),
        });
    };
    Task::perform(
        async move {
            let result = rest
                .commands_run(&command, &params, &rid, tmid.as_deref(), Some(&trigger))
                .await
                .map_err(Failure::from);
            Ran {
                trigger,
                command,
                rid,
                tmid,
                result,
            }
        },
        |r| r,
    )
}

/// The call ended: settles the trigger; a failure is shown as a local
/// ephemeral message in the room (or thread).
pub fn finished(ws: &mut Workspace, ran: &Ran) {
    ws.uikit_mut().finish_command(&ran.trigger, Instant::now());
    if let Err(e) = &ran.result {
        tracing::debug!(error = %e, "commands.run failed");
        ws.apply_ephemeral_message(failure_message(ran));
    }
}

/// The local `rocket.cat` message for a failed command.
pub fn failure_message(ran: &Ran) -> RcMessage {
    let command = ran.command.trim_start_matches('/');
    let msg = t!("uikit.command_failed", command = command).into_owned();
    RcMessage {
        id: rocketchat::ids::random_id(),
        rid: ran.rid.clone(),
        tmid: ran.tmid.clone(),
        // Markdown like the web's ephemeral (the command shows as code).
        md: Some(super::mrkdwn::parse(&msg)),
        msg,
        ts: Some(RcDate::now()),
        u: UserRef {
            id: ROCKET_CAT.into(),
            username: Some(ROCKET_CAT.into()),
            name: None,
        },
        groupable: Some(false),
        ..Default::default()
    }
}
