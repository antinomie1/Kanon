//! Milky platform delivery and lifecycle contract.

use super::*;

#[async_trait]
impl PlatformAdapter for MilkyAdapter {
    /// Platform identifier owned by this adapter.
    fn platform(&self) -> &str {
        &self.platform
    }

    /// Console-facing name.
    fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Whether the event stream is currently up.
    fn is_connected(&self) -> bool {
        self.state
            .read()
            .expect("adapter state poisoned")
            .status
            .state
            == ConnectionState::Connected
    }

    /// Delivers one outbound message through the Milky API.
    async fn deliver(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliverMessageResponse, AdapterError> {
        let target = mapping::delivery_target(&request.channel_id).map_err(|err| {
            AdapterError::Delivery {
                platform: self.platform.clone(),
                reason: err.to_string(),
            }
        })?;

        let client = {
            self.state
                .read()
                .expect("adapter state poisoned")
                .client
                .clone()
        };
        let Some(client) = client else {
            return Err(AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: "the Milky adapter is disabled or not configured yet; enable it in the management console"
                    .to_string(),
            });
        };

        let delivery = mapping::outbound_delivery(&request.segments).map_err(|err| {
            AdapterError::Delivery {
                platform: self.platform.clone(),
                reason: err.to_string(),
            }
        })?;

        // Naming the segment kinds makes a delivery observable in the trace log without dumping
        // user content, which is what an operator needs when a platform rejects a payload.
        let segment_types: Vec<&str> = delivery
            .message
            .iter()
            .map(mapping::outbound_segment_type)
            .collect();
        let file_count = delivery.files.len();

        // The message goes first so text introducing a file arrives before it. A failure after it
        // was sent still fails the delivery: the operator must learn that a file never arrived.
        let mut message_id = String::new();
        if !delivery.message.is_empty() {
            let sent = match target.scene {
                ChannelScene::Group => {
                    let input = SendGroupMessageInput {
                        group_id: target.peer_id,
                        message: delivery.message,
                    };
                    client
                        .send_group_message(&input)
                        .await
                        .map(|out| out.message_seq)
                }
                ChannelScene::Friend => {
                    let input = SendPrivateMessageInput {
                        user_id: target.peer_id,
                        message: delivery.message,
                    };
                    client
                        .send_private_message(&input)
                        .await
                        .map(|out| out.message_seq)
                }
                // Rejected by `delivery_target` above; handled explicitly so a future scene cannot
                // silently fall through to a private message.
                ChannelScene::Temp => {
                    unreachable!("temporary conversations are rejected before send")
                }
            };
            match sent {
                Ok(message_seq) => message_id = message_seq.to_string(),
                Err(err) => {
                    return Err(record_failure(
                        &self.state,
                        err.into_adapter_error(&self.platform),
                    ));
                }
            }
        }

        // An upload yields a file id, not a message sequence, so it never becomes the message id.
        for file in delivery.files {
            let uploaded = match target.scene {
                ChannelScene::Group => client
                    .upload_group_file(&UploadGroupFileInput {
                        group_id: target.peer_id,
                        // The group's root folder, the protocol's own default.
                        parent_folder_id: "/".to_string(),
                        file_uri: file.uri,
                        file_name: file.name,
                    })
                    .await
                    .map(drop),
                ChannelScene::Friend => client
                    .upload_private_file(&UploadPrivateFileInput {
                        user_id: target.peer_id,
                        file_uri: file.uri,
                        file_name: file.name,
                    })
                    .await
                    .map(drop),
                ChannelScene::Temp => {
                    unreachable!("temporary conversations are rejected before send")
                }
            };
            if let Err(err) = uploaded {
                return Err(record_failure(
                    &self.state,
                    err.into_adapter_error(&self.platform),
                ));
            }
        }

        self.state
            .write()
            .expect("adapter state poisoned")
            .status
            .messages_delivered += 1;

        tracing::debug!(
            channel = %request.channel_id,
            segments = ?segment_types,
            files = file_count,
            message_id = %message_id,
            "Milky message delivered"
        );

        Ok(DeliverMessageResponse {
            success: true,
            message_id,
            error_message: String::new(),
        })
    }

    fn capabilities(&self) -> &[Capability] {
        CAPABILITIES
    }

    /// Passes the call to the Milky API endpoint of the same name.
    async fn call_api(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError> {
        let client = self
            .state
            .read()
            .expect("adapter state poisoned")
            .client
            .clone()
            .ok_or_else(|| AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: "the Milky adapter is disabled".to_string(),
            })?;
        client
            .call_raw(action, &params)
            .await
            .map_err(|error| AdapterError::Api {
                platform: self.platform.clone(),
                action: action.to_string(),
                reason: error.to_string(),
            })
    }

    /// Accepts a friend request or group invitation with the token its notice carried.
    async fn accept_request(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        let client = self
            .state
            .read()
            .expect("adapter state poisoned")
            .client
            .clone()
            .ok_or_else(|| AdapterError::Configuration {
                platform: self.platform.clone(),
                reason: "the Milky adapter is disabled".to_string(),
            })?;
        let input =
            mapping::accept_request_input(event).map_err(|reason| AdapterError::Delivery {
                platform: self.platform.clone(),
                reason,
            })?;
        let result = match input {
            mapping::AcceptRequest::Friend(initiator_uid) => {
                client
                    .accept_friend_request(&AcceptFriendRequestInput {
                        initiator_uid,
                        is_filtered: false,
                    })
                    .await
            }
            mapping::AcceptRequest::Group(group_id, invitation_seq) => {
                client
                    .accept_group_invitation(&AcceptGroupInvitationInput {
                        group_id,
                        invitation_seq,
                    })
                    .await
            }
        };
        result.map_err(|err| err.into_adapter_error(&self.platform))
    }

    /// Reacts to a group message the bot is about to answer.
    async fn acknowledge(&self, event: &PipelineEventRequest) -> Result<(), AdapterError> {
        let client = {
            let state = self.state.read().expect("adapter state poisoned");
            match &state.client {
                Some(client) => client.clone(),
                _ => return Ok(()),
            }
        };
        let Ok(target) = mapping::parse_channel_id(&event.channel_id) else {
            return Ok(());
        };
        let message_seq = event
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.fields.get(mapping::META_MESSAGE_SEQ))
            .and_then(|value| match value.kind {
                Some(kanon_proto::prost_types::value::Kind::NumberValue(seq)) => Some(seq as i64),
                _ => None,
            });
        // Only a group message can carry a reaction; notices have no message to react to.
        let (ChannelScene::Group, Some(message_seq)) = (target.scene, message_seq) else {
            return Ok(());
        };
        client
            .send_group_message_reaction(&SendGroupMessageReactionInput {
                group_id: target.peer_id,
                message_seq,
                reaction: ACK_REACTION.to_string(),
                reaction_type: "face".to_string(),
                is_add: true,
            })
            .await
            .map_err(|err| err.into_adapter_error(&self.platform))
    }

    /// Captures the ingest handle and brings up the event stream.
    async fn start(&self, ingress: EventIngress) -> Result<(), AdapterError> {
        let adapter = self.clone();
        tokio::spawn(async move {
            let mut lifecycle = adapter.lifecycle.lock().await;
            if lifecycle.source.is_some() {
                return Ok(());
            }
            lifecycle.ingress = Some(ingress);
            adapter
                .replace(&mut lifecycle, adapter.config(), |_| Ok(()))
                .await
        })
        .await
        .map_err(|error| AdapterError::Configuration {
            platform: self.platform.clone(),
            reason: format!("Milky lifecycle task failed: {error}"),
        })?
    }

    /// Releases the event stream.
    async fn stop(&self) -> Result<(), AdapterError> {
        let adapter = self.clone();
        tokio::spawn(async move {
            let mut lifecycle = adapter.lifecycle.lock().await;
            lifecycle.stop_runtime().await;
            lifecycle.ingress = None;
            let mut state = adapter.state.write().expect("adapter state poisoned");
            state.client = None;
            state.status.state = ConnectionState::Disabled;
        })
        .await
        .map_err(|error| AdapterError::Configuration {
            platform: self.platform.clone(),
            reason: format!("Milky lifecycle task failed: {error}"),
        })?;
        Ok(())
    }
}
