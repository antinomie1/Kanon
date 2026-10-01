//! Typed client for the Milky API surface.
//!
//! # Shape of the protocol
//! A Milky protocol implementation exposes one HTTP endpoint per operation at
//! `POST /api/:api`. The request body is the operation's parameter object — an empty JSON
//! object when the operation takes no parameters — and the answer is a `status`/`retcode`
//! envelope whose `data` field carries the result. Authentication, when configured, is a
//! `Bearer` token in the `Authorization` header.
//!
//! # Why one generic call path instead of 65 hand-written ones
//! Every operation differs only in its endpoint name and its two payload types, so the
//! transport, authentication, envelope unwrapping and error mapping live in exactly one
//! place ([`MilkyClient::call`]). The typed methods below are deliberately thin: they exist to
//! give callers compile-time checked parameters and results, not to re-implement transport
//! policy 65 times.
//!
//! # Why the client never retries
//! Milky offers no idempotency key, so a retry after a timeout can duplicate a message the
//! user already sees. Delivery failures are therefore reported upward, where the core's
//! outbound dispatcher records them in the per-platform dead-letter log for operator
//! reconciliation instead of guessing whether the first attempt landed.

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::config::{MilkyConfig, REQUEST_TIMEOUT};
use crate::protocol::*;

/// Errors raised while talking to a Milky protocol implementation.
#[derive(Debug, Error)]
pub enum MilkyError {
    /// The implementation could not be reached, the connection failed, or the request timed out.
    #[error("milky transport error: {0}")]
    Transport(String),
    /// A payload could not be serialized, or the answer could not be decoded.
    #[error("milky payload error: {0}")]
    Payload(String),
    /// The implementation answered with an explicit failure `retcode`.
    #[error("milky API '{endpoint}' failed (retcode {retcode}): {message}")]
    Api {
        /// Endpoint that reported the failure.
        endpoint: String,
        /// Milky `retcode` describing the failure class.
        retcode: i32,
        /// Human-readable reason supplied by the implementation.
        message: String,
    },
    /// The implementation answered with an HTTP status other than `200`.
    #[error("milky API '{endpoint}' returned HTTP {status}")]
    Http {
        /// Endpoint that was called.
        endpoint: String,
        /// HTTP status code returned by the implementation.
        status: u16,
    },
}

impl MilkyError {
    /// Describes this failure as an adapter error, so the core reports one consistent
    /// vocabulary for every platform.
    pub fn into_adapter_error(self, platform: &str) -> kanon_core::AdapterError {
        kanon_core::AdapterError::Delivery {
            platform: platform.to_string(),
            reason: self.to_string(),
        }
    }
}

/// Reusable HTTP client for one Milky protocol implementation.
///
/// The client is cheap to clone internally (the underlying `reqwest::Client` pools
/// connections) and holds no mutable state, so a single instance serves every concurrent
/// delivery and management call for its platform.
pub struct MilkyClient {
    /// Connection settings, including the credential sent with every call.
    config: MilkyConfig,
    /// Shared connection pool.
    http: reqwest::Client,
}

impl MilkyClient {
    /// Builds a client for a prepared (validated and normalized) configuration.
    pub fn new(config: MilkyConfig) -> Result<Self, MilkyError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| MilkyError::Transport(format!("failed to build HTTP client: {err}")))?;
        Ok(Self { config, http })
    }

    /// Returns the configuration this client was built from.
    pub fn config(&self) -> &MilkyConfig {
        &self.config
    }

    /// Calls an endpoint and requires a result payload.
    ///
    /// A `200` answer whose envelope is missing `data` is treated as a protocol violation
    /// rather than as an empty result: the caller asked for a value the implementation did
    /// not supply, and inventing a default would hide a real disagreement.
    pub async fn call<In, Out>(&self, endpoint: &str, input: &In) -> Result<Out, MilkyError>
    where
        In: Serialize + ?Sized,
        Out: DeserializeOwned,
    {
        let envelope: ApiGeneralResponse<Out> = self.dispatch(endpoint, input).await?;
        envelope.data.ok_or_else(|| {
            MilkyError::Payload(format!(
                "milky API '{endpoint}' succeeded but returned no data"
            ))
        })
    }

    /// Calls an endpoint whose result is an empty object and discards it.
    ///
    /// Milky implementations are allowed to omit `data` entirely for operations that return
    /// nothing, so a missing payload is not an error here.
    pub async fn call_void<In>(&self, endpoint: &str, input: &In) -> Result<(), MilkyError>
    where
        In: Serialize + ?Sized,
    {
        let _: ApiGeneralResponse<ApiEmptyStruct> = self.dispatch(endpoint, input).await?;
        Ok(())
    }

    /// Calls any endpoint and returns its `data` untouched (`null` when the implementation
    /// omitted it).
    ///
    /// Used for plugin pass-through calls, whose response shape only the plugin knows.
    pub async fn call_raw(
        &self,
        endpoint: &str,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, MilkyError> {
        let envelope: ApiGeneralResponse<serde_json::Value> =
            self.dispatch(endpoint, input).await?;
        Ok(envelope.data.unwrap_or(serde_json::Value::Null))
    }

    /// Sends one request and unwraps the transport-level part of the envelope.
    ///
    /// Kept private on purpose: every public entry point goes through the two typed helpers
    /// above so that envelope handling exists in exactly one place.
    async fn dispatch<Out>(
        &self,
        endpoint: &str,
        input: &(impl Serialize + ?Sized),
    ) -> Result<ApiGeneralResponse<Out>, MilkyError>
    where
        Out: DeserializeOwned,
    {
        let mut request = self.http.post(self.config.api_url(endpoint)).json(input);
        if let Some(header) = self.config.authorization_header() {
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }

        let response = request
            .send()
            .await
            .map_err(|err| MilkyError::Transport(err.to_string()))?;

        let status = response.status();
        if status != reqwest::StatusCode::OK {
            return Err(MilkyError::Http {
                endpoint: endpoint.to_string(),
                status: status.as_u16(),
            });
        }

        let envelope = response
            .json::<ApiGeneralResponse<Out>>()
            .await
            .map_err(|err| MilkyError::Payload(err.to_string()))?;

        if envelope.retcode != 0 {
            return Err(MilkyError::Api {
                endpoint: endpoint.to_string(),
                retcode: envelope.retcode,
                message: envelope
                    .message
                    .unwrap_or_else(|| "no reason given".to_string()),
            });
        }

        Ok(envelope)
    }

    // --- system API ---

    /// Fetches the logged-in account information (uin and nickname).
    ///
    /// Calls `get_login_info`.
    pub async fn get_login_info(&self) -> Result<GetLoginInfoOutput, MilkyError> {
        self.call("get_login_info", &ApiEmptyStruct {}).await
    }

    // --- system API ---

    /// Fetches information about the protocol implementation itself (name, version, QQ protocol version and type, Milky version).
    ///
    /// Calls `get_impl_info`.
    pub async fn get_impl_info(&self) -> Result<GetImplInfoOutput, MilkyError> {
        self.call("get_impl_info", &ApiEmptyStruct {}).await
    }

    // --- system API ---

    /// Fetches a user's profile (QID, age, sex, level, location and school).
    ///
    /// Calls `get_user_profile`.
    pub async fn get_user_profile(
        &self,
        input: &GetUserProfileInput,
    ) -> Result<GetUserProfileOutput, MilkyError> {
        self.call("get_user_profile", input).await
    }

    // --- system API ---

    /// Lists the account's friends, optionally bypassing the implementation's cache.
    ///
    /// Calls `get_friend_list`.
    pub async fn get_friend_list(
        &self,
        input: &GetFriendListInput,
    ) -> Result<GetFriendListOutput, MilkyError> {
        self.call("get_friend_list", input).await
    }

    // --- system API ---

    /// Fetches one friend's information, optionally bypassing the implementation's cache.
    ///
    /// Calls `get_friend_info`.
    pub async fn get_friend_info(
        &self,
        input: &GetFriendInfoInput,
    ) -> Result<GetFriendInfoOutput, MilkyError> {
        self.call("get_friend_info", input).await
    }

    // --- system API ---

    /// Lists the groups the account has joined, optionally bypassing the implementation's cache.
    ///
    /// Calls `get_group_list`.
    pub async fn get_group_list(
        &self,
        input: &GetGroupListInput,
    ) -> Result<GetGroupListOutput, MilkyError> {
        self.call("get_group_list", input).await
    }

    // --- system API ---

    /// Fetches one group's information, optionally bypassing the implementation's cache.
    ///
    /// Calls `get_group_info`.
    pub async fn get_group_info(
        &self,
        input: &GetGroupInfoInput,
    ) -> Result<GetGroupInfoOutput, MilkyError> {
        self.call("get_group_info", input).await
    }

    // --- system API ---

    /// Lists the members of a group, optionally bypassing the implementation's cache.
    ///
    /// Calls `get_group_member_list`.
    pub async fn get_group_member_list(
        &self,
        input: &GetGroupMemberListInput,
    ) -> Result<GetGroupMemberListOutput, MilkyError> {
        self.call("get_group_member_list", input).await
    }

    // --- system API ---

    /// Fetches one group member's information, optionally bypassing the implementation's cache.
    ///
    /// Calls `get_group_member_info`.
    pub async fn get_group_member_info(
        &self,
        input: &GetGroupMemberInfoInput,
    ) -> Result<GetGroupMemberInfoOutput, MilkyError> {
        self.call("get_group_member_info", input).await
    }

    // --- system API ---

    /// Lists the friends and groups pinned in the account's conversation list.
    ///
    /// Calls `get_peer_pins`.
    pub async fn get_peer_pins(&self) -> Result<GetPeerPinsOutput, MilkyError> {
        self.call("get_peer_pins", &ApiEmptyStruct {}).await
    }

    // --- system API ---

    /// Pins or unpins a friend or group conversation.
    ///
    /// Calls `set_peer_pin`.
    pub async fn set_peer_pin(&self, input: &SetPeerPinInput) -> Result<(), MilkyError> {
        self.call_void("set_peer_pin", input).await
    }

    // --- system API ---

    /// Sets the account's avatar from an image URI.
    ///
    /// Calls `set_avatar`.
    pub async fn set_avatar(&self, input: &SetAvatarInput) -> Result<(), MilkyError> {
        self.call_void("set_avatar", input).await
    }

    // --- system API ---

    /// Sets the account's nickname.
    ///
    /// Calls `set_nickname`.
    pub async fn set_nickname(&self, input: &SetNicknameInput) -> Result<(), MilkyError> {
        self.call_void("set_nickname", input).await
    }

    // --- system API ---

    /// Sets the account's personal signature.
    ///
    /// Calls `set_bio`.
    pub async fn set_bio(&self, input: &SetBioInput) -> Result<(), MilkyError> {
        self.call_void("set_bio", input).await
    }

    // --- system API ---

    /// Lists the URLs of the account's custom emoji.
    ///
    /// Calls `get_custom_face_url_list`.
    pub async fn get_custom_face_url_list(&self) -> Result<GetCustomFaceUrlListOutput, MilkyError> {
        self.call("get_custom_face_url_list", &ApiEmptyStruct {})
            .await
    }

    // --- system API ---

    /// Fetches the account's cookies for a domain.
    ///
    /// Calls `get_cookies`.
    pub async fn get_cookies(
        &self,
        input: &GetCookiesInput,
    ) -> Result<GetCookiesOutput, MilkyError> {
        self.call("get_cookies", input).await
    }

    // --- system API ---

    /// Fetches the CSRF token used by the account's web session.
    ///
    /// Calls `get_csrf_token`.
    pub async fn get_csrf_token(&self) -> Result<GetCsrfTokenOutput, MilkyError> {
        self.call("get_csrf_token", &ApiEmptyStruct {}).await
    }

    // --- message API ---

    /// Sends a message to a friend.
    ///
    /// Calls `send_private_message`.
    pub async fn send_private_message(
        &self,
        input: &SendPrivateMessageInput,
    ) -> Result<SendPrivateMessageOutput, MilkyError> {
        self.call("send_private_message", input).await
    }

    // --- message API ---

    /// Sends a message to a group.
    ///
    /// Calls `send_group_message`.
    pub async fn send_group_message(
        &self,
        input: &SendGroupMessageInput,
    ) -> Result<SendGroupMessageOutput, MilkyError> {
        self.call("send_group_message", input).await
    }

    // --- message API ---

    /// Recalls a private message.
    ///
    /// Calls `recall_private_message`.
    pub async fn recall_private_message(
        &self,
        input: &RecallPrivateMessageInput,
    ) -> Result<(), MilkyError> {
        self.call_void("recall_private_message", input).await
    }

    // --- message API ---

    /// Recalls a group message.
    ///
    /// Calls `recall_group_message`.
    pub async fn recall_group_message(
        &self,
        input: &RecallGroupMessageInput,
    ) -> Result<(), MilkyError> {
        self.call_void("recall_group_message", input).await
    }

    // --- message API ---

    /// Fetches a single message by scene, peer and sequence number.
    ///
    /// Calls `get_message`.
    pub async fn get_message(
        &self,
        input: &GetMessageInput,
    ) -> Result<GetMessageOutput, MilkyError> {
        self.call("get_message", input).await
    }

    // --- message API ---

    /// Fetches one page of history messages for a conversation.
    ///
    /// Calls `get_history_messages`.
    pub async fn get_history_messages(
        &self,
        input: &GetHistoryMessagesInput,
    ) -> Result<GetHistoryMessagesOutput, MilkyError> {
        self.call("get_history_messages", input).await
    }

    // --- message API ---

    /// Resolves a resource ID into a temporary download URL.
    ///
    /// Calls `get_resource_temp_url`.
    pub async fn get_resource_temp_url(
        &self,
        input: &GetResourceTempUrlInput,
    ) -> Result<GetResourceTempUrlOutput, MilkyError> {
        self.call("get_resource_temp_url", input).await
    }

    // --- message API ---

    /// Expands a merged-forward ID into its individual messages.
    ///
    /// Calls `get_forwarded_messages`.
    pub async fn get_forwarded_messages(
        &self,
        input: &GetForwardedMessagesInput,
    ) -> Result<GetForwardedMessagesOutput, MilkyError> {
        self.call("get_forwarded_messages", input).await
    }

    // --- message API ---

    /// Marks a message and every older message in the conversation as read.
    ///
    /// Calls `mark_message_as_read`.
    pub async fn mark_message_as_read(
        &self,
        input: &MarkMessageAsReadInput,
    ) -> Result<(), MilkyError> {
        self.call_void("mark_message_as_read", input).await
    }

    // --- friend API ---

    /// Sends a nudge ("poke") to a friend.
    ///
    /// Calls `send_friend_nudge`.
    pub async fn send_friend_nudge(&self, input: &SendFriendNudgeInput) -> Result<(), MilkyError> {
        self.call_void("send_friend_nudge", input).await
    }

    // --- friend API ---

    /// Likes a user's profile card.
    ///
    /// Calls `send_profile_like`.
    pub async fn send_profile_like(&self, input: &SendProfileLikeInput) -> Result<(), MilkyError> {
        self.call_void("send_profile_like", input).await
    }

    // --- friend API ---

    /// Removes a friend.
    ///
    /// Calls `delete_friend`.
    pub async fn delete_friend(&self, input: &DeleteFriendInput) -> Result<(), MilkyError> {
        self.call_void("delete_friend", input).await
    }

    // --- friend API ---

    /// Lists pending (and filtered) friend requests.
    ///
    /// Calls `get_friend_requests`.
    pub async fn get_friend_requests(
        &self,
        input: &GetFriendRequestsInput,
    ) -> Result<GetFriendRequestsOutput, MilkyError> {
        self.call("get_friend_requests", input).await
    }

    // --- friend API ---

    /// Accepts a friend request.
    ///
    /// Calls `accept_friend_request`.
    pub async fn accept_friend_request(
        &self,
        input: &AcceptFriendRequestInput,
    ) -> Result<(), MilkyError> {
        self.call_void("accept_friend_request", input).await
    }

    // --- friend API ---

    /// Rejects a friend request.
    ///
    /// Calls `reject_friend_request`.
    pub async fn reject_friend_request(
        &self,
        input: &RejectFriendRequestInput,
    ) -> Result<(), MilkyError> {
        self.call_void("reject_friend_request", input).await
    }

    // --- group API ---

    /// Renames a group.
    ///
    /// Calls `set_group_name`.
    pub async fn set_group_name(&self, input: &SetGroupNameInput) -> Result<(), MilkyError> {
        self.call_void("set_group_name", input).await
    }

    // --- group API ---

    /// Sets a group's avatar from an image URI.
    ///
    /// Calls `set_group_avatar`.
    pub async fn set_group_avatar(&self, input: &SetGroupAvatarInput) -> Result<(), MilkyError> {
        self.call_void("set_group_avatar", input).await
    }

    // --- group API ---

    /// Sets a group member's card (the name shown inside that group).
    ///
    /// Calls `set_group_member_card`.
    pub async fn set_group_member_card(
        &self,
        input: &SetGroupMemberCardInput,
    ) -> Result<(), MilkyError> {
        self.call_void("set_group_member_card", input).await
    }

    // --- group API ---

    /// Sets a group member's special title.
    ///
    /// Calls `set_group_member_special_title`.
    pub async fn set_group_member_special_title(
        &self,
        input: &SetGroupMemberSpecialTitleInput,
    ) -> Result<(), MilkyError> {
        self.call_void("set_group_member_special_title", input)
            .await
    }

    // --- group API ---

    /// Grants or revokes a group member's administrator role.
    ///
    /// Calls `set_group_member_admin`.
    pub async fn set_group_member_admin(
        &self,
        input: &SetGroupMemberAdminInput,
    ) -> Result<(), MilkyError> {
        self.call_void("set_group_member_admin", input).await
    }

    // --- group API ---

    /// Mutes a group member for a duration, or lifts an existing mute.
    ///
    /// Calls `set_group_member_mute`.
    pub async fn set_group_member_mute(
        &self,
        input: &SetGroupMemberMuteInput,
    ) -> Result<(), MilkyError> {
        self.call_void("set_group_member_mute", input).await
    }

    // --- group API ---

    /// Enables or disables whole-group mute.
    ///
    /// Calls `set_group_whole_mute`.
    pub async fn set_group_whole_mute(
        &self,
        input: &SetGroupWholeMuteInput,
    ) -> Result<(), MilkyError> {
        self.call_void("set_group_whole_mute", input).await
    }

    // --- group API ---

    /// Removes a member from a group.
    ///
    /// Calls `kick_group_member`.
    pub async fn kick_group_member(&self, input: &KickGroupMemberInput) -> Result<(), MilkyError> {
        self.call_void("kick_group_member", input).await
    }

    // --- group API ---

    /// Lists a group's announcements.
    ///
    /// Calls `get_group_announcements`.
    pub async fn get_group_announcements(
        &self,
        input: &GetGroupAnnouncementsInput,
    ) -> Result<GetGroupAnnouncementsOutput, MilkyError> {
        self.call("get_group_announcements", input).await
    }

    // --- group API ---

    /// Publishes a group announcement.
    ///
    /// Calls `send_group_announcement`.
    pub async fn send_group_announcement(
        &self,
        input: &SendGroupAnnouncementInput,
    ) -> Result<(), MilkyError> {
        self.call_void("send_group_announcement", input).await
    }

    // --- group API ---

    /// Deletes a group announcement.
    ///
    /// Calls `delete_group_announcement`.
    pub async fn delete_group_announcement(
        &self,
        input: &DeleteGroupAnnouncementInput,
    ) -> Result<(), MilkyError> {
        self.call_void("delete_group_announcement", input).await
    }

    // --- group API ---

    /// Lists a group's essence messages.
    ///
    /// Calls `get_group_essence_messages`.
    pub async fn get_group_essence_messages(
        &self,
        input: &GetGroupEssenceMessagesInput,
    ) -> Result<GetGroupEssenceMessagesOutput, MilkyError> {
        self.call("get_group_essence_messages", input).await
    }

    // --- group API ---

    /// Marks a group message as essence, or removes that mark.
    ///
    /// Calls `set_group_essence_message`.
    pub async fn set_group_essence_message(
        &self,
        input: &SetGroupEssenceMessageInput,
    ) -> Result<(), MilkyError> {
        self.call_void("set_group_essence_message", input).await
    }

    // --- group API ---

    /// Leaves a group.
    ///
    /// Calls `quit_group`.
    pub async fn quit_group(&self, input: &QuitGroupInput) -> Result<(), MilkyError> {
        self.call_void("quit_group", input).await
    }

    // --- group API ---

    /// Adds or removes an emoji reaction on a group message.
    ///
    /// Calls `send_group_message_reaction`.
    pub async fn send_group_message_reaction(
        &self,
        input: &SendGroupMessageReactionInput,
    ) -> Result<(), MilkyError> {
        self.call_void("send_group_message_reaction", input).await
    }

    // --- group API ---

    /// Sends a nudge ("poke") to a group member.
    ///
    /// Calls `send_group_nudge`.
    pub async fn send_group_nudge(&self, input: &SendGroupNudgeInput) -> Result<(), MilkyError> {
        self.call_void("send_group_nudge", input).await
    }

    // --- group API ---

    /// Lists a group's join and invitation notifications.
    ///
    /// Calls `get_group_notifications`.
    pub async fn get_group_notifications(
        &self,
        input: &GetGroupNotificationsInput,
    ) -> Result<GetGroupNotificationsOutput, MilkyError> {
        self.call("get_group_notifications", input).await
    }

    // --- group API ---

    /// Accepts a join request, including one that invites another user.
    ///
    /// Calls `accept_group_request`.
    pub async fn accept_group_request(
        &self,
        input: &AcceptGroupRequestInput,
    ) -> Result<(), MilkyError> {
        self.call_void("accept_group_request", input).await
    }

    // --- group API ---

    /// Rejects a join request, including one that invites another user.
    ///
    /// Calls `reject_group_request`.
    pub async fn reject_group_request(
        &self,
        input: &RejectGroupRequestInput,
    ) -> Result<(), MilkyError> {
        self.call_void("reject_group_request", input).await
    }

    // --- group API ---

    /// Accepts an invitation for the account itself to join a group.
    ///
    /// Calls `accept_group_invitation`.
    pub async fn accept_group_invitation(
        &self,
        input: &AcceptGroupInvitationInput,
    ) -> Result<(), MilkyError> {
        self.call_void("accept_group_invitation", input).await
    }

    // --- group API ---

    /// Rejects an invitation for the account itself to join a group.
    ///
    /// Calls `reject_group_invitation`.
    pub async fn reject_group_invitation(
        &self,
        input: &RejectGroupInvitationInput,
    ) -> Result<(), MilkyError> {
        self.call_void("reject_group_invitation", input).await
    }

    // --- file API ---

    /// Uploads a file into a private conversation.
    ///
    /// Calls `upload_private_file`.
    pub async fn upload_private_file(
        &self,
        input: &UploadPrivateFileInput,
    ) -> Result<UploadPrivateFileOutput, MilkyError> {
        self.call("upload_private_file", input).await
    }

    // --- file API ---

    /// Uploads a file into a group's file list.
    ///
    /// Calls `upload_group_file`.
    pub async fn upload_group_file(
        &self,
        input: &UploadGroupFileInput,
    ) -> Result<UploadGroupFileOutput, MilkyError> {
        self.call("upload_group_file", input).await
    }

    // --- file API ---

    /// Resolves a private-chat file ID into a download URL.
    ///
    /// Calls `get_private_file_download_url`.
    pub async fn get_private_file_download_url(
        &self,
        input: &GetPrivateFileDownloadUrlInput,
    ) -> Result<GetPrivateFileDownloadUrlOutput, MilkyError> {
        self.call("get_private_file_download_url", input).await
    }

    // --- file API ---

    /// Resolves a group file ID into a download URL.
    ///
    /// Calls `get_group_file_download_url`.
    pub async fn get_group_file_download_url(
        &self,
        input: &GetGroupFileDownloadUrlInput,
    ) -> Result<GetGroupFileDownloadUrlOutput, MilkyError> {
        self.call("get_group_file_download_url", input).await
    }

    // --- file API ---

    /// Lists a group's folders and files.
    ///
    /// Calls `get_group_files`.
    pub async fn get_group_files(
        &self,
        input: &GetGroupFilesInput,
    ) -> Result<GetGroupFilesOutput, MilkyError> {
        self.call("get_group_files", input).await
    }

    // --- file API ---

    /// Moves a group file into another folder.
    ///
    /// Calls `move_group_file`.
    pub async fn move_group_file(&self, input: &MoveGroupFileInput) -> Result<(), MilkyError> {
        self.call_void("move_group_file", input).await
    }

    // --- file API ---

    /// Renames a group file.
    ///
    /// Calls `rename_group_file`.
    pub async fn rename_group_file(&self, input: &RenameGroupFileInput) -> Result<(), MilkyError> {
        self.call_void("rename_group_file", input).await
    }

    // --- file API ---

    /// Deletes a group file.
    ///
    /// Calls `delete_group_file`.
    pub async fn delete_group_file(&self, input: &DeleteGroupFileInput) -> Result<(), MilkyError> {
        self.call_void("delete_group_file", input).await
    }

    // --- file API ---

    /// Converts a temporary group file into a permanent one.
    ///
    /// Calls `persist_group_file`.
    pub async fn persist_group_file(
        &self,
        input: &PersistGroupFileInput,
    ) -> Result<(), MilkyError> {
        self.call_void("persist_group_file", input).await
    }

    // --- file API ---

    /// Creates a folder in a group's file list.
    ///
    /// Calls `create_group_folder`.
    pub async fn create_group_folder(
        &self,
        input: &CreateGroupFolderInput,
    ) -> Result<CreateGroupFolderOutput, MilkyError> {
        self.call("create_group_folder", input).await
    }

    // --- file API ---

    /// Renames a group folder.
    ///
    /// Calls `rename_group_folder`.
    pub async fn rename_group_folder(
        &self,
        input: &RenameGroupFolderInput,
    ) -> Result<(), MilkyError> {
        self.call_void("rename_group_folder", input).await
    }

    // --- file API ---

    /// Deletes a group folder.
    ///
    /// Calls `delete_group_folder`.
    pub async fn delete_group_folder(
        &self,
        input: &DeleteGroupFolderInput,
    ) -> Result<(), MilkyError> {
        self.call_void("delete_group_folder", input).await
    }
}
