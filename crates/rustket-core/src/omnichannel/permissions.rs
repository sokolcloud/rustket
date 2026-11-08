//! Omnichannel permission ids, roles and setting ids (`docs/omnichannel.md` §20, §21).

pub const VIEW_L_ROOM: &str = "view-l-room";
pub const VIEW_LIVECHAT_QUEUE: &str = "view-livechat-queue";
pub const CLOSE_LIVECHAT_ROOM: &str = "close-livechat-room";
pub const CLOSE_OTHERS_LIVECHAT_ROOM: &str = "close-others-livechat-room";
pub const TRANSFER_LIVECHAT_GUEST: &str = "transfer-livechat-guest";
pub const SAVE_OTHERS_LIVECHAT_ROOM_INFO: &str = "save-others-livechat-room-info";
pub const VIEW_LIVECHAT_ROOM_CUSTOMFIELDS: &str = "view-livechat-room-customfields";
pub const EDIT_LIVECHAT_ROOM_CUSTOMFIELDS: &str = "edit-livechat-room-customfields";
pub const VIEW_LIVECHAT_ROOMS: &str = "view-livechat-rooms";
pub const VIEW_LIVECHAT_ROOM_CLOSED_BY_ANOTHER_AGENT: &str =
    "view-livechat-room-closed-by-another-agent";
pub const VIEW_LIVECHAT_ROOM_CLOSED_SAME_DEPARTMENT: &str =
    "view-livechat-room-closed-same-department";
pub const VIEW_LIVECHAT_MANAGER: &str = "view-livechat-manager";
pub const VIEW_LIVECHAT_DEPARTMENTS: &str = "view-livechat-departments";
pub const VIEW_OMNICHANNEL_CONTACT_CENTER: &str = "view-omnichannel-contact-center";
pub const VIEW_LIVECHAT_CONTACT: &str = "view-livechat-contact";
pub const CREATE_LIVECHAT_CONTACT: &str = "create-livechat-contact";
pub const UPDATE_LIVECHAT_CONTACT: &str = "update-livechat-contact";
pub const DELETE_LIVECHAT_CONTACT: &str = "delete-livechat-contact";
pub const VIEW_LIVECHAT_CONTACT_HISTORY: &str = "view-livechat-contact-history";
pub const EDIT_OMNICHANNEL_CONTACT: &str = "edit-omnichannel-contact";
pub const SEND_OMNICHANNEL_CHAT_TRANSCRIPT: &str = "send-omnichannel-chat-transcript";
pub const ON_HOLD_LIVECHAT_ROOM: &str = "on-hold-livechat-room";
pub const ON_HOLD_OTHERS_LIVECHAT_ROOM: &str = "on-hold-others-livechat-room";
pub const REMOVE_CLOSED_LIVECHAT_ROOM: &str = "remove-closed-livechat-room";
pub const MANAGE_LIVECHAT_AGENTS: &str = "manage-livechat-agents";
pub const CHANGE_LIVECHAT_ROOM_VISITOR: &str = "change-livechat-room-visitor";
// Enterprise only (absent on CE servers).
pub const VIEW_CANNED_RESPONSES: &str = "view-canned-responses";
pub const VIEW_ALL_CANNED_RESPONSES: &str = "view-all-canned-responses";
pub const VIEW_AGENT_CANNED_RESPONSES: &str = "view-agent-canned-responses";
pub const SAVE_CANNED_RESPONSES: &str = "save-canned-responses";
pub const SAVE_ALL_CANNED_RESPONSES: &str = "save-all-canned-responses";
pub const SAVE_DEPARTMENT_CANNED_RESPONSES: &str = "save-department-canned-responses";
pub const REMOVE_CANNED_RESPONSES: &str = "remove-canned-responses";
pub const REQUEST_PDF_TRANSCRIPT: &str = "request-pdf-transcript";
pub const BLOCK_LIVECHAT_CONTACT: &str = "block-livechat-contact";
pub const UNBLOCK_LIVECHAT_CONTACT: &str = "unblock-livechat-contact";
pub const MANAGE_LIVECHAT_TAGS: &str = "manage-livechat-tags";
pub const MANAGE_LIVECHAT_PRIORITIES: &str = "manage-livechat-priorities";
pub const MANAGE_LIVECHAT_SLA: &str = "manage-livechat-sla";

pub const ROLE_AGENT: &str = "livechat-agent";
pub const ROLE_MANAGER: &str = "livechat-manager";
/// EE only.
pub const ROLE_MONITOR: &str = "livechat-monitor";

/// Public settings the omnichannel UI reads.
pub mod settings {
    pub const ENABLED: &str = "Livechat_enabled";
    pub const ROUTING_METHOD: &str = "Livechat_Routing_Method";
    pub const POOL_MAX_DISPLAYED: &str =
        "Livechat_guest_pool_max_number_incoming_livechats_displayed";
    pub const SHOW_QUEUE_LIST_LINK: &str = "Livechat_show_queue_list_link";
    pub const ENABLED_WHEN_AGENT_IDLE: &str = "Livechat_enabled_when_agent_idle";
    pub const REQUEST_COMMENT_WHEN_CLOSING: &str =
        "Livechat_request_comment_when_closing_conversation";
    pub const ALLOW_FORCE_CLOSE: &str = "Omnichannel_allow_force_close_conversations";
    pub const ENABLE_TRANSCRIPT: &str = "Livechat_enable_transcript";
    pub const TRANSCRIPT_SEND_ALWAYS: &str = "Livechat_transcript_send_always";
    pub const TRANSCRIPT_EMAIL_SUBJECT: &str = "Livechat_transcript_email_subject";
    pub const CONTINUOUS_SOUND: &str = "Livechat_continuous_sound_notification_new_livechat_room";
    pub const BUSINESS_HOURS: &str = "Livechat_enable_business_hours";
    pub const CALL_PROVIDER: &str = "Omnichannel_call_provider";
    pub const EXTERNAL_FRAME: &str = "Omnichannel_External_Frame_Enabled";
    // Enterprise only.
    pub const ALLOW_MANUAL_ON_HOLD: &str = "Livechat_allow_manual_on_hold";
    pub const MANUAL_ON_HOLD_ENGAGEMENT_ONLY: &str =
        "Livechat_allow_manual_on_hold_upon_agent_engagement_only";
    pub const CANNED_RESPONSES_ENABLE: &str = "Canned_Responses_Enable";
    pub const SORTING_MECHANISM: &str = "Omnichannel_sorting_mechanism";
}
