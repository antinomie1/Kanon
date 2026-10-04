//! Generated Milky endpoints.

use super::*;

pub trait ApiEndpoint: Serialize {
    type Output: DeserializeOwned;
    const NAME: &'static str;
}

/// Retrieves login information.
impl ApiEndpoint for GetLoginInfoInput {
    type Output = GetLoginInfoOutput;
    const NAME: &'static str = "get_login_info";
}

/// Retrieves protocol implementation information.
impl ApiEndpoint for GetImplInfoInput {
    type Output = GetImplInfoOutput;
    const NAME: &'static str = "get_impl_info";
}

/// Retrieves the user's profile.
impl ApiEndpoint for GetUserProfileInput {
    type Output = GetUserProfileOutput;
    const NAME: &'static str = "get_user_profile";
}

/// Retrieves the friend list.
impl ApiEndpoint for GetFriendListInput {
    type Output = GetFriendListOutput;
    const NAME: &'static str = "get_friend_list";
}

/// Retrieves friend information.
impl ApiEndpoint for GetFriendInfoInput {
    type Output = GetFriendInfoOutput;
    const NAME: &'static str = "get_friend_info";
}

/// Retrieves the group list.
impl ApiEndpoint for GetGroupListInput {
    type Output = GetGroupListOutput;
    const NAME: &'static str = "get_group_list";
}

/// Retrieves group information.
impl ApiEndpoint for GetGroupInfoInput {
    type Output = GetGroupInfoOutput;
    const NAME: &'static str = "get_group_info";
}

/// Retrieves the group member list.
impl ApiEndpoint for GetGroupMemberListInput {
    type Output = GetGroupMemberListOutput;
    const NAME: &'static str = "get_group_member_list";
}

/// Retrieves group member information.
impl ApiEndpoint for GetGroupMemberInfoInput {
    type Output = GetGroupMemberInfoOutput;
    const NAME: &'static str = "get_group_member_info";
}

/// Retrieves the pinned friends and groups.
/// @since 1.2
impl ApiEndpoint for GetPeerPinsInput {
    type Output = GetPeerPinsOutput;
    const NAME: &'static str = "get_peer_pins";
}

/// Sets the pinned status of a friend or group.
/// @since 1.2
impl ApiEndpoint for SetPeerPinInput {
    type Output = SetPeerPinOutput;
    const NAME: &'static str = "set_peer_pin";
}

/// Sets the QQ account avatar.
/// @since 1.1
impl ApiEndpoint for SetAvatarInput {
    type Output = SetAvatarOutput;
    const NAME: &'static str = "set_avatar";
}

/// Sets the QQ account nickname.
/// @since 1.1
impl ApiEndpoint for SetNicknameInput {
    type Output = SetNicknameOutput;
    const NAME: &'static str = "set_nickname";
}

/// Sets the QQ account bio.
/// @since 1.1
impl ApiEndpoint for SetBioInput {
    type Output = SetBioOutput;
    const NAME: &'static str = "set_bio";
}

/// Retrieves the custom emoji URL list.
/// @since 1.1
impl ApiEndpoint for GetCustomFaceUrlListInput {
    type Output = GetCustomFaceUrlListOutput;
    const NAME: &'static str = "get_custom_face_url_list";
}

/// Retrieves Cookies.
impl ApiEndpoint for GetCookiesInput {
    type Output = GetCookiesOutput;
    const NAME: &'static str = "get_cookies";
}

/// Retrieves the CSRF Token.
impl ApiEndpoint for GetCsrfTokenInput {
    type Output = GetCsrfTokenOutput;
    const NAME: &'static str = "get_csrf_token";
}

/// Sends a private message.
impl ApiEndpoint for SendPrivateMessageInput {
    type Output = SendPrivateMessageOutput;
    const NAME: &'static str = "send_private_message";
}

/// Sends a group message.
impl ApiEndpoint for SendGroupMessageInput {
    type Output = SendGroupMessageOutput;
    const NAME: &'static str = "send_group_message";
}

/// Recalls a private message.
impl ApiEndpoint for RecallPrivateMessageInput {
    type Output = RecallPrivateMessageOutput;
    const NAME: &'static str = "recall_private_message";
}

/// Recalls a group message.
impl ApiEndpoint for RecallGroupMessageInput {
    type Output = RecallGroupMessageOutput;
    const NAME: &'static str = "recall_group_message";
}

/// Retrieves a message.
impl ApiEndpoint for GetMessageInput {
    type Output = GetMessageOutput;
    const NAME: &'static str = "get_message";
}

/// Retrieves the message history.
impl ApiEndpoint for GetHistoryMessagesInput {
    type Output = GetHistoryMessagesOutput;
    const NAME: &'static str = "get_history_messages";
}

/// Retrieves the temporary resource URL.
impl ApiEndpoint for GetResourceTempUrlInput {
    type Output = GetResourceTempUrlOutput;
    const NAME: &'static str = "get_resource_temp_url";
}

/// Retrieves the content of a merged forward message.
impl ApiEndpoint for GetForwardedMessagesInput {
    type Output = GetForwardedMessagesOutput;
    const NAME: &'static str = "get_forwarded_messages";
}

/// Marks messages as read.
impl ApiEndpoint for MarkMessageAsReadInput {
    type Output = MarkMessageAsReadOutput;
    const NAME: &'static str = "mark_message_as_read";
}

/// Sends a friend nudge.
impl ApiEndpoint for SendFriendNudgeInput {
    type Output = SendFriendNudgeOutput;
    const NAME: &'static str = "send_friend_nudge";
}

/// Sends a profile like.
impl ApiEndpoint for SendProfileLikeInput {
    type Output = SendProfileLikeOutput;
    const NAME: &'static str = "send_profile_like";
}

/// Deletes a friend.
/// @since 1.1
impl ApiEndpoint for DeleteFriendInput {
    type Output = DeleteFriendOutput;
    const NAME: &'static str = "delete_friend";
}

/// Retrieves the friend request list.
impl ApiEndpoint for GetFriendRequestsInput {
    type Output = GetFriendRequestsOutput;
    const NAME: &'static str = "get_friend_requests";
}

/// Accepts a friend request.
impl ApiEndpoint for AcceptFriendRequestInput {
    type Output = AcceptFriendRequestOutput;
    const NAME: &'static str = "accept_friend_request";
}

/// Rejects a friend request.
impl ApiEndpoint for RejectFriendRequestInput {
    type Output = RejectFriendRequestOutput;
    const NAME: &'static str = "reject_friend_request";
}

/// Sets the group name.
impl ApiEndpoint for SetGroupNameInput {
    type Output = SetGroupNameOutput;
    const NAME: &'static str = "set_group_name";
}

/// Sets the group avatar.
impl ApiEndpoint for SetGroupAvatarInput {
    type Output = SetGroupAvatarOutput;
    const NAME: &'static str = "set_group_avatar";
}

/// Sets the group card.
impl ApiEndpoint for SetGroupMemberCardInput {
    type Output = SetGroupMemberCardOutput;
    const NAME: &'static str = "set_group_member_card";
}

/// Sets a group member's special title.
impl ApiEndpoint for SetGroupMemberSpecialTitleInput {
    type Output = SetGroupMemberSpecialTitleOutput;
    const NAME: &'static str = "set_group_member_special_title";
}

/// Sets a group admin.
impl ApiEndpoint for SetGroupMemberAdminInput {
    type Output = SetGroupMemberAdminOutput;
    const NAME: &'static str = "set_group_member_admin";
}

/// Mutes a group member.
impl ApiEndpoint for SetGroupMemberMuteInput {
    type Output = SetGroupMemberMuteOutput;
    const NAME: &'static str = "set_group_member_mute";
}

/// Sets whole-group mute.
impl ApiEndpoint for SetGroupWholeMuteInput {
    type Output = SetGroupWholeMuteOutput;
    const NAME: &'static str = "set_group_whole_mute";
}

/// Kicks a group member.
impl ApiEndpoint for KickGroupMemberInput {
    type Output = KickGroupMemberOutput;
    const NAME: &'static str = "kick_group_member";
}

/// Retrieves the group announcement list.
impl ApiEndpoint for GetGroupAnnouncementsInput {
    type Output = GetGroupAnnouncementsOutput;
    const NAME: &'static str = "get_group_announcements";
}

/// Sends a group announcement.
impl ApiEndpoint for SendGroupAnnouncementInput {
    type Output = SendGroupAnnouncementOutput;
    const NAME: &'static str = "send_group_announcement";
}

/// Deletes a group announcement.
impl ApiEndpoint for DeleteGroupAnnouncementInput {
    type Output = DeleteGroupAnnouncementOutput;
    const NAME: &'static str = "delete_group_announcement";
}

/// Retrieves the group essence message list.
impl ApiEndpoint for GetGroupEssenceMessagesInput {
    type Output = GetGroupEssenceMessagesOutput;
    const NAME: &'static str = "get_group_essence_messages";
}

/// Sets a group essence message.
impl ApiEndpoint for SetGroupEssenceMessageInput {
    type Output = SetGroupEssenceMessageOutput;
    const NAME: &'static str = "set_group_essence_message";
}

/// Leaves a group.
impl ApiEndpoint for QuitGroupInput {
    type Output = QuitGroupOutput;
    const NAME: &'static str = "quit_group";
}

/// Sends a group message reaction.
impl ApiEndpoint for SendGroupMessageReactionInput {
    type Output = SendGroupMessageReactionOutput;
    const NAME: &'static str = "send_group_message_reaction";
}

/// Sends a group nudge.
impl ApiEndpoint for SendGroupNudgeInput {
    type Output = SendGroupNudgeOutput;
    const NAME: &'static str = "send_group_nudge";
}

/// Retrieves the group notification list.
impl ApiEndpoint for GetGroupNotificationsInput {
    type Output = GetGroupNotificationsOutput;
    const NAME: &'static str = "get_group_notifications";
}

/// Accepts a group join or invitation request.
impl ApiEndpoint for AcceptGroupRequestInput {
    type Output = AcceptGroupRequestOutput;
    const NAME: &'static str = "accept_group_request";
}

/// Rejects a group join or invitation request.
impl ApiEndpoint for RejectGroupRequestInput {
    type Output = RejectGroupRequestOutput;
    const NAME: &'static str = "reject_group_request";
}

/// Accepts an invitation to join a group.
impl ApiEndpoint for AcceptGroupInvitationInput {
    type Output = AcceptGroupInvitationOutput;
    const NAME: &'static str = "accept_group_invitation";
}

/// Rejects an invitation to join a group.
impl ApiEndpoint for RejectGroupInvitationInput {
    type Output = RejectGroupInvitationOutput;
    const NAME: &'static str = "reject_group_invitation";
}

/// Uploads a private chat file.
impl ApiEndpoint for UploadPrivateFileInput {
    type Output = UploadPrivateFileOutput;
    const NAME: &'static str = "upload_private_file";
}

/// Uploads a group file.
impl ApiEndpoint for UploadGroupFileInput {
    type Output = UploadGroupFileOutput;
    const NAME: &'static str = "upload_group_file";
}

/// Retrieves the private chat file download URL.
impl ApiEndpoint for GetPrivateFileDownloadUrlInput {
    type Output = GetPrivateFileDownloadUrlOutput;
    const NAME: &'static str = "get_private_file_download_url";
}

/// Retrieves the group file download URL.
impl ApiEndpoint for GetGroupFileDownloadUrlInput {
    type Output = GetGroupFileDownloadUrlOutput;
    const NAME: &'static str = "get_group_file_download_url";
}

/// Retrieves the group file list.
impl ApiEndpoint for GetGroupFilesInput {
    type Output = GetGroupFilesOutput;
    const NAME: &'static str = "get_group_files";
}

/// Moves a group file.
impl ApiEndpoint for MoveGroupFileInput {
    type Output = MoveGroupFileOutput;
    const NAME: &'static str = "move_group_file";
}

/// Renames a group file.
impl ApiEndpoint for RenameGroupFileInput {
    type Output = RenameGroupFileOutput;
    const NAME: &'static str = "rename_group_file";
}

/// Deletes a group file.
impl ApiEndpoint for DeleteGroupFileInput {
    type Output = DeleteGroupFileOutput;
    const NAME: &'static str = "delete_group_file";
}

/// Persists a group file as a permanent file.
/// @since 1.3
impl ApiEndpoint for PersistGroupFileInput {
    type Output = PersistGroupFileOutput;
    const NAME: &'static str = "persist_group_file";
}

/// Creates a group folder.
impl ApiEndpoint for CreateGroupFolderInput {
    type Output = CreateGroupFolderOutput;
    const NAME: &'static str = "create_group_folder";
}

/// Renames a group folder.
impl ApiEndpoint for RenameGroupFolderInput {
    type Output = RenameGroupFolderOutput;
    const NAME: &'static str = "rename_group_folder";
}

/// Deletes a group folder.
impl ApiEndpoint for DeleteGroupFolderInput {
    type Output = DeleteGroupFolderOutput;
    const NAME: &'static str = "delete_group_folder";
}
