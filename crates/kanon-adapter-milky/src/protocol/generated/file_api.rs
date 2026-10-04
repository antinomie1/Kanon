//! Generated Milky file api.

use super::*;

// ---- File APIs ----

/// Request parameters for the `upload_private_file` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadPrivateFileInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// File URI, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "file_uri")]
    pub file_uri: String,
    /// File name.
    #[serde(rename = "file_name")]
    pub file_name: String,
}

/// Response data for the `upload_private_file` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadPrivateFileOutput {
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
}

/// Request parameters for the `upload_group_file` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadGroupFileInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Target folder ID.
    #[serde(
        rename = "parent_folder_id",
        default = "default_upload_group_file_input_parent_folder_id",
        deserialize_with = "deserialize_upload_group_file_input_parent_folder_id"
    )]
    pub parent_folder_id: String,
    /// File URI, supporting the `file://`, `http(s)://`, and `base64://` formats.
    #[serde(rename = "file_uri")]
    pub file_uri: String,
    /// File name.
    #[serde(rename = "file_name")]
    pub file_name: String,
}

/// Response data for the `upload_group_file` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadGroupFileOutput {
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
}

/// Request parameters for the `get_private_file_download_url` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetPrivateFileDownloadUrlInput {
    /// Friend QQ number.
    #[serde(rename = "user_id")]
    pub user_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
    /// TriSHA1 hash of the file.
    #[serde(rename = "file_hash")]
    pub file_hash: String,
    /// Whether the file was sent by yourself.
    /// @since 1.3
    #[serde(
        rename = "is_self_send",
        default = "default_get_private_file_download_url_input_is_self_send",
        deserialize_with = "deserialize_get_private_file_download_url_input_is_self_send"
    )]
    pub is_self_send: bool,
}

/// Response data for the `get_private_file_download_url` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetPrivateFileDownloadUrlOutput {
    /// File download URL.
    #[serde(rename = "download_url")]
    pub download_url: String,
}

/// Request parameters for the `get_group_file_download_url` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupFileDownloadUrlInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
}

/// Response data for the `get_group_file_download_url` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupFileDownloadUrlOutput {
    /// File download URL.
    #[serde(rename = "download_url")]
    pub download_url: String,
}

/// Request parameters for the `get_group_files` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupFilesInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Parent folder ID.
    #[serde(
        rename = "parent_folder_id",
        default = "default_get_group_files_input_parent_folder_id",
        deserialize_with = "deserialize_get_group_files_input_parent_folder_id"
    )]
    pub parent_folder_id: String,
}

/// Response data for the `get_group_files` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetGroupFilesOutput {
    /// File list.
    #[serde(rename = "files")]
    pub files: Vec<GroupFileEntity>,
    /// Folder list.
    #[serde(rename = "folders")]
    pub folders: Vec<GroupFolderEntity>,
}

/// Request parameters for the `move_group_file` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveGroupFileInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
    /// ID of the folder containing the file.
    #[serde(
        rename = "parent_folder_id",
        default = "default_move_group_file_input_parent_folder_id",
        deserialize_with = "deserialize_move_group_file_input_parent_folder_id"
    )]
    pub parent_folder_id: String,
    /// Target folder ID.
    #[serde(
        rename = "target_folder_id",
        default = "default_move_group_file_input_target_folder_id",
        deserialize_with = "deserialize_move_group_file_input_target_folder_id"
    )]
    pub target_folder_id: String,
}

/// Response data for the `move_group_file` API.
pub type MoveGroupFileOutput = ApiEmptyStruct;

/// Request parameters for the `rename_group_file` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameGroupFileInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
    /// ID of the folder containing the file.
    #[serde(
        rename = "parent_folder_id",
        default = "default_rename_group_file_input_parent_folder_id",
        deserialize_with = "deserialize_rename_group_file_input_parent_folder_id"
    )]
    pub parent_folder_id: String,
    /// New file name.
    #[serde(rename = "new_file_name")]
    pub new_file_name: String,
}

/// Response data for the `rename_group_file` API.
pub type RenameGroupFileOutput = ApiEmptyStruct;

/// Request parameters for the `delete_group_file` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteGroupFileInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
}

/// Response data for the `delete_group_file` API.
pub type DeleteGroupFileOutput = ApiEmptyStruct;

/// Request parameters for the `persist_group_file` API.
/// @since 1.3
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistGroupFileInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// File ID.
    #[serde(rename = "file_id")]
    pub file_id: String,
}

/// Response data for the `persist_group_file` API.
/// @since 1.3
pub type PersistGroupFileOutput = ApiEmptyStruct;

/// Request parameters for the `create_group_folder` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateGroupFolderInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Folder name.
    #[serde(rename = "folder_name")]
    pub folder_name: String,
}

/// Response data for the `create_group_folder` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateGroupFolderOutput {
    /// Folder ID.
    #[serde(rename = "folder_id")]
    pub folder_id: String,
}

/// Request parameters for the `rename_group_folder` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameGroupFolderInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Folder ID.
    #[serde(rename = "folder_id")]
    pub folder_id: String,
    /// New folder name.
    #[serde(rename = "new_folder_name")]
    pub new_folder_name: String,
}

/// Response data for the `rename_group_folder` API.
pub type RenameGroupFolderOutput = ApiEmptyStruct;

/// Request parameters for the `delete_group_folder` API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteGroupFolderInput {
    /// Group number.
    #[serde(rename = "group_id")]
    pub group_id: i64,
    /// Folder ID.
    #[serde(rename = "folder_id")]
    pub folder_id: String,
}

/// Response data for the `delete_group_folder` API.
pub type DeleteGroupFolderOutput = ApiEmptyStruct;

// ####################################
// Serde Helpers
// ####################################
