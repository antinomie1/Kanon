//! Typed OneBot v11 calls over the adapter's existing universal WebSocket.
//!
//! The client resolves the live session on every call, so a retained handle survives reconnects.
//! All methods share correlation, queue limits and a deadline. Mutating calls are never retried:
//! a timeout or disconnect after sending leaves their outcome unknown.

use std::sync::{Arc, RwLock};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::{State, protocol::*, transport};

/// Failures of an API call, preserving the implementation's numeric return code.
#[derive(Debug, Error)]
pub enum OneBotError {
    /// No universal session is currently available.
    #[error("OneBot is disabled or not connected")]
    NotConnected,
    /// Queue admission, socket I/O or disconnect failure.
    #[error("OneBot transport error: {0}")]
    Transport(String),
    /// An API deadline expired; the operation may have happened remotely.
    #[error("OneBot API '{action}' timed out; operation outcome is unknown")]
    Timeout {
        /// Action whose response did not arrive.
        action: String,
    },
    /// The implementation returned a non-success response.
    #[error("OneBot API '{action}' failed (retcode: {retcode:?})")]
    Api {
        /// Failed action.
        action: String,
        /// Numeric OneBot code, absent when the envelope itself was malformed.
        retcode: Option<i64>,
    },
    /// Request serialization or typed response decoding failed.
    #[error("OneBot API '{action}' payload error: {reason}")]
    Payload {
        /// Action being encoded or decoded.
        action: String,
        /// Local serialization/decoding diagnostic, never the full peer response.
        reason: String,
    },
}

/// Cloneable API handle obtained from [`crate::OneBotAdapter::client`].
///
/// This handle shares the adapter's connection; it neither opens nor owns another socket.
#[derive(Clone)]
pub struct OneBotClient {
    state: Arc<RwLock<State>>,
}

impl OneBotClient {
    pub(super) fn new(state: Arc<RwLock<State>>) -> Self {
        Self { state }
    }

    /// Calls an action and decodes its required `data` payload into the requested type.
    ///
    /// This also supports implementation-specific actions without a second transport. An
    /// asynchronous acknowledgement (`status: async`) is not reported as completed success.
    pub async fn call<In: Serialize + ?Sized, Out: DeserializeOwned>(
        &self,
        action: &str,
        input: &In,
    ) -> Result<Out, OneBotError> {
        Self::call_on(self.session()?, action, input).await
    }

    /// Pins a session before an adapter performs asynchronous attachment preparation.
    pub(super) fn session(&self) -> Result<mpsc::Sender<transport::Command>, OneBotError> {
        self.state
            .read()
            .expect("OneBot state poisoned")
            .sender
            .clone()
            .ok_or(OneBotError::NotConnected)
    }

    /// Uses an already selected session; reconnects must never move an in-flight delivery.
    pub(super) async fn call_on<In: Serialize + ?Sized, Out: DeserializeOwned>(
        sender: mpsc::Sender<transport::Command>,
        action: &str,
        input: &In,
    ) -> Result<Out, OneBotError> {
        let response = Self::request(sender, action, input).await?;
        let data = response
            .get("data")
            .filter(|data| !data.is_null())
            .cloned()
            .ok_or_else(|| OneBotError::Payload {
                action: action.into(),
                reason: "response has no data".into(),
            })?;
        serde_json::from_value(data).map_err(|_| OneBotError::Payload {
            action: action.into(),
            reason: "response data does not match the expected type".into(),
        })
    }

    /// Calls any action and returns its `data` untouched (`null` when the peer sent none).
    ///
    /// Used for plugin pass-through calls, whose response shape only the plugin knows.
    pub async fn call_raw(&self, action: &str, params: &Value) -> Result<Value, OneBotError> {
        let mut response = Self::request(self.session()?, action, params).await?;
        Ok(response
            .get_mut("data")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }

    /// Calls an action with no response data, accepting null or omitted `data` on success.
    pub async fn call_void<In: Serialize + ?Sized>(
        &self,
        action: &str,
        input: &In,
    ) -> Result<(), OneBotError> {
        Self::request(self.session()?, action, input)
            .await
            .map(|_| ())
    }

    pub(super) async fn request<In: Serialize + ?Sized>(
        sender: mpsc::Sender<transport::Command>,
        action: &str,
        input: &In,
    ) -> Result<Value, OneBotError> {
        let params = serde_json::to_value(input).map_err(|error| OneBotError::Payload {
            action: action.into(),
            reason: error.to_string(),
        })?;
        let (reply, receive) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + transport::REQUEST_TIMEOUT;
        sender
            .try_send(transport::Command {
                action: action.into(),
                params,
                reply,
                deadline,
            })
            .map_err(|_| {
                OneBotError::Transport("connection closed or outbound queue is full".into())
            })?;
        let response = tokio::time::timeout_at(deadline, receive)
            .await
            .map_err(|_| OneBotError::Timeout {
                action: action.into(),
            })?
            .map_err(|_| {
                OneBotError::Transport(
                    "disconnected before replying; operation outcome is unknown".into(),
                )
            })??;
        if response["status"] != "ok" || response["retcode"].as_i64() != Some(0) {
            // Peer wording can include credentials or message contents; retain only the code.
            return Err(OneBotError::Api {
                action: action.into(),
                retcode: response["retcode"].as_i64(),
            });
        }
        Ok(response)
    }

    /// Sends a private message. `auto_escape` applies only to a text/CQ string.
    pub async fn send_private_msg(
        &self,
        user_id: i64,
        message: &Message,
        auto_escape: bool,
    ) -> Result<SendMessageOutput, OneBotError> {
        self.call(
            "send_private_msg",
            &json!({"user_id": user_id, "message": message, "auto_escape": auto_escape}),
        )
        .await
    }

    /// Sends a group message. `auto_escape` applies only to a text/CQ string.
    pub async fn send_group_msg(
        &self,
        group_id: i64,
        message: &Message,
        auto_escape: bool,
    ) -> Result<SendMessageOutput, OneBotError> {
        self.call(
            "send_group_msg",
            &json!({"group_id": group_id, "message": message, "auto_escape": auto_escape}),
        )
        .await
    }

    /// Recalls a message, subject to the account's permissions.
    pub async fn delete_msg(&self, message_id: i64) -> Result<(), OneBotError> {
        self.call_void("delete_msg", &json!({"message_id": message_id}))
            .await
    }

    /// Fetches a message and its sender/content by ID.
    pub async fn get_msg(&self, message_id: i64) -> Result<MessageInfo, OneBotError> {
        self.call("get_msg", &json!({"message_id": message_id}))
            .await
    }

    /// Fetches the node segments of a merged forward message.
    pub async fn get_forward_msg(&self, id: &str) -> Result<ForwardMessage, OneBotError> {
        self.call("get_forward_msg", &json!({"id": id})).await
    }

    /// Sends profile likes; platform limits are enforced by the implementation.
    pub async fn send_like(&self, user_id: i64, times: u32) -> Result<(), OneBotError> {
        self.call_void("send_like", &json!({"user_id": user_id, "times": times}))
            .await
    }

    /// Kicks a group member and optionally rejects their subsequent join requests.
    pub async fn set_group_kick(
        &self,
        group_id: i64,
        user_id: i64,
        reject_add_request: bool,
    ) -> Result<(), OneBotError> {
        self.call_void("set_group_kick", &json!({"group_id": group_id, "user_id": user_id, "reject_add_request": reject_add_request})).await
    }

    /// Mutes a member for `duration` seconds; zero removes the mute.
    pub async fn set_group_ban(
        &self,
        group_id: i64,
        user_id: i64,
        duration: u32,
    ) -> Result<(), OneBotError> {
        self.call_void(
            "set_group_ban",
            &json!({"group_id": group_id, "user_id": user_id, "duration": duration}),
        )
        .await
    }

    /// Enables or disables whole-group mute.
    pub async fn set_group_whole_ban(
        &self,
        group_id: i64,
        enable: bool,
    ) -> Result<(), OneBotError> {
        self.call_void(
            "set_group_whole_ban",
            &json!({"group_id": group_id, "enable": enable}),
        )
        .await
    }

    /// Grants or removes administrator status.
    pub async fn set_group_admin(
        &self,
        group_id: i64,
        user_id: i64,
        enable: bool,
    ) -> Result<(), OneBotError> {
        self.call_void(
            "set_group_admin",
            &json!({"group_id": group_id, "user_id": user_id, "enable": enable}),
        )
        .await
    }

    /// Changes a member's group card; an empty card clears it.
    pub async fn set_group_card(
        &self,
        group_id: i64,
        user_id: i64,
        card: &str,
    ) -> Result<(), OneBotError> {
        self.call_void(
            "set_group_card",
            &json!({"group_id": group_id, "user_id": user_id, "card": card}),
        )
        .await
    }

    /// Changes the group name.
    pub async fn set_group_name(&self, group_id: i64, group_name: &str) -> Result<(), OneBotError> {
        self.call_void(
            "set_group_name",
            &json!({"group_id": group_id, "group_name": group_name}),
        )
        .await
    }

    /// Leaves a group; `is_dismiss` requests disbanding when the account is its owner.
    pub async fn set_group_leave(
        &self,
        group_id: i64,
        is_dismiss: bool,
    ) -> Result<(), OneBotError> {
        self.call_void(
            "set_group_leave",
            &json!({"group_id": group_id, "is_dismiss": is_dismiss}),
        )
        .await
    }

    /// Sets a member's title. Empty clears it; `duration = -1` requests permanence.
    pub async fn set_group_special_title(
        &self,
        group_id: i64,
        user_id: i64,
        special_title: &str,
        duration: i64,
    ) -> Result<(), OneBotError> {
        self.call_void("set_group_special_title", &json!({"group_id": group_id, "user_id": user_id, "special_title": special_title, "duration": duration})).await
    }

    /// Approves or rejects a friend request identified by its event flag.
    pub async fn set_friend_add_request(
        &self,
        flag: &str,
        approve: bool,
        remark: &str,
    ) -> Result<(), OneBotError> {
        self.call_void(
            "set_friend_add_request",
            &json!({"flag": flag, "approve": approve, "remark": remark}),
        )
        .await
    }

    /// Handles a group join request or invitation; use the subtype from its event.
    pub async fn set_group_add_request(
        &self,
        flag: &str,
        sub_type: GroupRequestType,
        approve: bool,
        reason: &str,
    ) -> Result<(), OneBotError> {
        self.call_void(
            "set_group_add_request",
            &json!({"flag": flag, "sub_type": sub_type, "approve": approve, "reason": reason}),
        )
        .await
    }

    /// Reads the logged-in account's ID and nickname.
    pub async fn get_login_info(&self) -> Result<LoginInfo, OneBotError> {
        self.call("get_login_info", &json!({})).await
    }

    /// Reads another account's public profile, optionally bypassing the cache.
    pub async fn get_stranger_info(
        &self,
        user_id: i64,
        no_cache: bool,
    ) -> Result<StrangerInfo, OneBotError> {
        self.call(
            "get_stranger_info",
            &json!({"user_id": user_id, "no_cache": no_cache}),
        )
        .await
    }

    /// Lists the account's friends.
    pub async fn get_friend_list(&self) -> Result<Vec<FriendInfo>, OneBotError> {
        self.call("get_friend_list", &json!({})).await
    }

    /// Reads a group's name and member counts, optionally bypassing the cache.
    pub async fn get_group_info(
        &self,
        group_id: i64,
        no_cache: bool,
    ) -> Result<GroupInfo, OneBotError> {
        self.call(
            "get_group_info",
            &json!({"group_id": group_id, "no_cache": no_cache}),
        )
        .await
    }

    /// Lists groups joined by the account.
    pub async fn get_group_list(&self) -> Result<Vec<GroupInfo>, OneBotError> {
        self.call("get_group_list", &json!({})).await
    }

    /// Reads detailed group-member information, optionally bypassing the cache.
    pub async fn get_group_member_info(
        &self,
        group_id: i64,
        user_id: i64,
        no_cache: bool,
    ) -> Result<GroupMemberInfo, OneBotError> {
        self.call(
            "get_group_member_info",
            &json!({"group_id": group_id, "user_id": user_id, "no_cache": no_cache}),
        )
        .await
    }

    /// Lists group members; some detail-only fields may be unavailable in list results.
    pub async fn get_group_member_list(
        &self,
        group_id: i64,
    ) -> Result<Vec<GroupMemberInfo>, OneBotError> {
        self.call("get_group_member_list", &json!({"group_id": group_id}))
            .await
    }

    /// Converts a cached voice file; the returned path belongs to the OneBot host.
    pub async fn get_record(&self, file: &str, out_format: &str) -> Result<MediaFile, OneBotError> {
        self.call(
            "get_record",
            &json!({"file": file, "out_format": out_format}),
        )
        .await
    }

    /// Resolves a cached image to a file on the OneBot host, not the Kanon host.
    pub async fn get_image(&self, file: &str) -> Result<MediaFile, OneBotError> {
        self.call("get_image", &json!({"file": file})).await
    }

    /// Checks whether this implementation can currently send images.
    pub async fn can_send_image(&self) -> Result<Capability, OneBotError> {
        self.call("can_send_image", &json!({})).await
    }

    /// Checks whether this implementation can currently send voice messages.
    pub async fn can_send_record(&self) -> Result<Capability, OneBotError> {
        self.call("can_send_record", &json!({})).await
    }

    /// Reads account/implementation health; `online: None` means unknown, not offline.
    pub async fn get_status(&self) -> Result<RuntimeStatus, OneBotError> {
        self.call("get_status", &json!({})).await
    }

    /// Reads implementation and protocol versions, preserving extra implementation fields.
    pub async fn get_version_info(&self) -> Result<VersionInfo, OneBotError> {
        self.call("get_version_info", &json!({})).await
    }
}
